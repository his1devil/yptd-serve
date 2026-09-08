//! Client for the yptd sidecar: spawns the Go process, speaks NDJSON to it over
//! a Unix socket, and turns "reply to request 41" and "an event happened" into
//! a blocking call and a channel respectively.
//!
//! Threads, not an async runtime. The TUI's main loop is a plain `recv()` on
//! one channel that terminal input and sidecar events both feed; the only
//! concurrency here is a reader thread that demultiplexes frames by the single
//! rule the protocol guarantees -- a reply carries an `id`, an event never does.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

/// An unsolicited frame from the SDK.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub name: String,
    pub data: Value,
}

#[derive(Debug)]
pub enum Error {
    /// The sidecar binary could not be found or started.
    Spawn(String),
    /// The socket never became connectable.
    Connect(String),
    /// The SDK reported a failure: its own code and message.
    Sdk { code: i32, msg: String },
    /// No reply within the deadline.
    Timeout(&'static str),
    /// The reader thread is gone; the sidecar has exited.
    Disconnected,
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(m) => write!(f, "无法启动边车: {m}"),
            Self::Connect(m) => write!(f, "无法连接边车: {m}"),
            Self::Sdk { code, msg } => write!(f, "SDK 错误 {code}: {msg}"),
            Self::Timeout(op) => write!(f, "边车调用超时: {op}"),
            Self::Disconnected => write!(f, "边车已断开"),
            Self::Io(e) => write!(f, "IO: {e}"),
            Self::Json(e) => write!(f, "JSON: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Deserialize)]
struct Frame {
    id: Option<u64>,
    ev: Option<String>,
    ok: Option<bool>,
    data: Option<Value>,
    code: Option<i32>,
    msg: Option<String>,
}

/// Where to find the sidecar and where it should keep its files.
#[derive(Clone, Debug)]
pub struct Config {
    /// Explicit binary path; when `None`, looked up next to the running
    /// executable and then on `PATH`.
    pub binary: Option<PathBuf>,
    /// Unix socket path. Must be short: `sockaddr_un` allows about 100 bytes.
    pub socket: PathBuf,
    /// SDK data directory (its SQLite cache and logs).
    pub data_dir: PathBuf,
}

/// A running sidecar with its request channel.
pub struct Sidecar {
    child: Child,
    writer: Mutex<BufWriter<UnixStream>>,
    pending: Arc<Mutex<HashMap<u64, Sender<Frame>>>>,
    next_id: AtomicU64,
}

const CONNECT_DEADLINE: Duration = Duration::from_secs(8);
const CALL_DEADLINE: Duration = Duration::from_secs(60);

impl Sidecar {
    /// Starts the sidecar and connects. Returns the receiver on which SDK
    /// events arrive; the caller merges it with its other input sources.
    pub fn spawn(config: &Config) -> Result<(Self, Receiver<Event>)> {
        let binary = locate(config.binary.as_deref())?;
        std::fs::create_dir_all(&config.data_dir)?;
        if let Some(parent) = config.socket.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let child = Command::new(&binary)
            .arg("--socket")
            .arg(&config.socket)
            .arg("--data-dir")
            .arg(&config.data_dir)
            // The sidecar redirects its own stdio into a log file; these are
            // belt-and-braces so nothing reaches the terminal even before it
            // gets that far.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Spawn(format!("{}: {e}", binary.display())))?;

        let stream = connect_with_retry(&config.socket, CONNECT_DEADLINE)?;
        let reader = BufReader::new(stream.try_clone()?);
        let writer = Mutex::new(BufWriter::new(stream));

        let pending: Arc<Mutex<HashMap<u64, Sender<Frame>>>> = Arc::default();
        let (event_tx, event_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("sidecar-reader".into())
            .spawn({
                let pending = Arc::clone(&pending);
                move || read_loop(reader, pending, event_tx)
            })?;

        Ok((
            Self {
                child,
                writer,
                pending,
                next_id: AtomicU64::new(1),
            },
            event_rx,
        ))
    }

    /// Sends one request and waits for its reply.
    pub fn call(&self, op: &'static str, args: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().expect("pending map").insert(id, tx);

        let frame = serde_json::json!({ "id": id, "op": op, "args": args });
        {
            let mut w = self.writer.lock().expect("writer");
            let written = serde_json::to_writer(&mut *w, &frame)
                .map_err(std::io::Error::other)
                .and_then(|()| w.write_all(b"\n"))
                .and_then(|()| w.flush());
            if written.is_err() {
                self.pending.lock().expect("pending map").remove(&id);
                return Err(Error::Disconnected);
            }
        }

        match rx.recv_timeout(CALL_DEADLINE) {
            Ok(reply) => {
                if reply.ok.unwrap_or(false) {
                    Ok(reply.data.unwrap_or(Value::Null))
                } else {
                    Err(Error::Sdk {
                        code: reply.code.unwrap_or(0),
                        msg: reply.msg.unwrap_or_default(),
                    })
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.pending.lock().expect("pending map").remove(&id);
                Err(Error::Timeout(op))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::Disconnected),
        }
    }

    /// Whether the child process is still running.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Closing our end of the socket lets the sidecar finish its current
        // client; killing is the fallback for a process that does not exit.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_loop(
    reader: BufReader<UnixStream>,
    pending: Arc<Mutex<HashMap<u64, Sender<Frame>>>>,
    events: Sender<Event>,
) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.is_empty() {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<Frame>(&line) else {
            continue;
        };
        match (frame.id, frame.ev.clone()) {
            (Some(id), _) => {
                if let Some(tx) = pending.lock().expect("pending map").remove(&id) {
                    let _ = tx.send(frame);
                }
            }
            (None, Some(name)) => {
                if events
                    .send(Event {
                        name,
                        data: frame.data.unwrap_or(Value::Null),
                    })
                    .is_err()
                {
                    break;
                }
            }
            (None, None) => {}
        }
    }
    // Waking every pending caller with a closed channel turns their wait into
    // `Disconnected` instead of a sixty-second timeout.
    pending.lock().expect("pending map").clear();
}

fn connect_with_retry(socket: &Path, deadline: Duration) -> Result<UnixStream> {
    let start = Instant::now();
    let mut last = String::new();
    while start.elapsed() < deadline {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(Error::Connect(format!("{} ({last})", socket.display())))
}

/// Finds the sidecar binary: explicit path, then beside the running
/// executable, then `PATH`.
fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(Error::Spawn(format!("{} 不存在", path.display())))
        };
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let beside = dir.join("yptd-sidecar");
        if beside.is_file() {
            return Ok(beside);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("yptd-sidecar");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(Error::Spawn(
        "找不到 yptd-sidecar：放到 yptd 同目录、PATH 里，或用 YPTD_SIDECAR 指定".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_into_replies_and_events_by_the_id_rule() {
        let reply: Frame = serde_json::from_str(r#"{"id":7,"ok":true,"data":{"x":1}}"#).unwrap();
        assert_eq!(reply.id, Some(7));
        assert!(reply.ev.is_none());

        let event: Frame =
            serde_json::from_str(r#"{"ev":"OnRecvNewMessage","data":{"seq":9}}"#).unwrap();
        assert!(event.id.is_none());
        assert_eq!(event.ev.as_deref(), Some("OnRecvNewMessage"));
    }

    #[test]
    fn a_failure_frame_carries_the_sdk_code() {
        let f: Frame = serde_json::from_str(r#"{"id":1,"ok":false,"code":1501,"msg":"expired"}"#).unwrap();
        assert_eq!(f.ok, Some(false));
        assert_eq!(f.code, Some(1501));
    }

    #[test]
    fn locating_a_missing_explicit_binary_is_a_clear_error() {
        let err = locate(Some(Path::new("/definitely/not/here"))).unwrap_err();
        assert!(matches!(err, Error::Spawn(_)));
        assert!(err.to_string().contains("不存在"));
    }

    #[test]
    fn errors_render_in_chinese_for_the_user() {
        let e = Error::Sdk {
            code: 1004,
            msg: "record not found".into(),
        };
        assert!(e.to_string().starts_with("SDK 错误 1004"));
        assert!(Error::Timeout("login").to_string().contains("login"));
    }
}
