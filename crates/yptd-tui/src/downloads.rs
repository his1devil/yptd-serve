//! Fetching the pictures other people sent.
//!
//! One worker thread, not one per image: a conversation can hold dozens of
//! attachments and spawning a thread apiece to hammer the same object store
//! is worse than fetching them in order. The UI never waits on any of it --
//! results arrive on the main loop's channel like any other event.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Refuse anything larger than this. A terminal preview is a dozen rows tall;
/// a 200 MB "picture" is a mistake or an attack, and either way not worth the
/// memory.
const MAX_BYTES: u64 = 24 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

/// What the worker sends back: the cache key it was asked about, and the
/// bytes or the reason there are none.
pub type Fetched = (String, Result<Vec<u8>, String>);

pub struct Downloads {
    jobs: Option<Sender<(String, String)>>,
    /// Keys already asked for, so a redraw does not queue the same picture
    /// again on every frame.
    requested: HashSet<String>,
}

impl Downloads {
    /// Starts the worker. `results` is the main loop's channel; `wake` turns
    /// a fetch into whatever input variant that loop understands.
    pub fn start<T, F>(cache: std::path::PathBuf, results: Sender<T>, wake: F) -> Self
    where
        T: Send + 'static,
        F: Fn(Fetched) -> T + Send + 'static,
    {
        let _ = std::fs::create_dir_all(&cache);
        let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
        let worker_cache = cache;
        let spawned = std::thread::Builder::new()
            .name("image-downloads".into())
            .spawn(move || {
                for (key, url) in rx {
                    let outcome = fetch(&worker_cache, &url);
                    if results.send(wake((key, outcome))).is_err() {
                        break;
                    }
                }
            });
        Self {
            jobs: spawned.is_ok().then_some(tx),
            requested: HashSet::new(),
        }
    }

    /// A `Downloads` that fetches nothing, for the mock and for terminals
    /// with no graphics protocol.
    pub fn disabled() -> Self {
        Self {
            jobs: None,
            requested: HashSet::new(),
        }
    }

    /// Queues a picture unless it is already on its way. Returns whether this
    /// call is what queued it.
    pub fn request(&mut self, key: &str, url: &str) -> bool {
        if url.is_empty() || self.requested.contains(key) {
            return false;
        }
        let Some(jobs) = self.jobs.as_ref() else {
            return false;
        };
        if jobs.send((key.to_owned(), url.to_owned())).is_err() {
            return false;
        }
        self.requested.insert(key.to_owned());
        true
    }
}

/// Cache first, network second. The object store names files by content, so
/// a cached file can never be stale.
pub fn fetch(cache: &Path, url: &str) -> Result<Vec<u8>, String> {
    let path = cache.join(cache_name(url));
    if let Ok(bytes) = std::fs::read(&path)
        && !bytes.is_empty()
    {
        return Ok(bytes);
    }
    let bytes = download(url)?;
    // A failed write is not a failed fetch: show the picture, cache it next
    // time. The directory is created here as well as at startup, because a
    // one-shot caller has not been through startup.
    let _ = std::fs::create_dir_all(cache);
    let _ = std::fs::write(&path, &bytes);
    Ok(bytes)
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| format!("下载失败: {e}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("下载失败: HTTP {status}"));
    }
    // Trust the declared length only as a reason to give up early; the read
    // below is what actually bounds memory.
    if let Some(len) = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        && len > MAX_BYTES
    {
        return Err(format!("图片太大 ({len} 字节)"));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取失败: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("图片太大".into());
    }
    Ok(bytes)
}

/// A file name for the cache. The object store already names files by a hash
/// of their content, so the last path segment is unique; everything outside
/// a conservative character set is replaced so a hostile URL cannot walk out
/// of the cache directory.
fn cache_name(url: &str) -> String {
    let tail = url
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("image");
    let tail = tail.split(['?', '#']).next().unwrap_or(tail);
    let cleaned: String = tail
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .take(96)
        .collect();
    // Two different URLs can end in the same name; a short digest of the whole
    // URL keeps them apart without making the name unreadable.
    format!("{:016x}-{}", digest(url), if cleaned.is_empty() { "image".into() } else { cleaned })
}

/// FNV-1a. Not a security boundary -- just enough to separate two URLs that
/// happen to end in the same file name.
fn digest(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cache_name_cannot_escape_the_cache_directory() {
        for url in [
            "https://h/object/../../etc/passwd",
            "https://h/object/..%2f..%2fetc",
            "https://h/a/b/../c.png",
        ] {
            let name = cache_name(url);
            assert!(!name.contains('/'), "{name}");
            assert!(!name.contains("..") || !name.contains('/'), "{name}");
            assert_eq!(Path::new(&name).components().count(), 1, "{name}");
        }
    }

    #[test]
    fn two_urls_ending_in_the_same_file_name_get_different_cache_entries() {
        let a = cache_name("https://one.example/object/alice/pic.png");
        let b = cache_name("https://one.example/object/bob/pic.png");
        assert_ne!(a, b);
        assert!(a.ends_with("pic.png") && b.ends_with("pic.png"), "still readable");
    }

    #[test]
    fn a_query_string_does_not_become_part_of_the_name() {
        let name = cache_name("https://h/object/pic.png?X-Amz-Signature=deadbeef");
        assert!(name.ends_with("pic.png"), "{name}");
    }

    #[test]
    fn a_fetch_writes_through_to_the_cache_and_reads_back_from_it() {
        let cache = std::env::temp_dir().join(format!("yptd-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        // No network here: seed the cache by hand and check it is preferred.
        std::fs::create_dir_all(&cache).expect("cache dir");
        let url = "https://example.invalid/object/seeded.png";
        std::fs::write(cache.join(cache_name(url)), b"seeded").expect("seed");
        assert_eq!(fetch(&cache, url).expect("cache hit"), b"seeded");
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_disabled_downloader_queues_nothing() {
        let mut downloads = Downloads::disabled();
        assert!(!downloads.request("k", "https://h/x.png"));
    }

    #[test]
    fn the_same_picture_is_only_requested_once() {
        let (tx, rx) = std::sync::mpsc::channel::<Fetched>();
        let cache = std::env::temp_dir().join(format!("yptd-dl-{}", std::process::id()));
        let mut downloads = Downloads::start(cache.clone(), tx, |f| f);
        assert!(downloads.request("k", "https://127.0.0.1:1/nope.png"));
        assert!(!downloads.request("k", "https://127.0.0.1:1/nope.png"));
        // The worker answers even when the fetch fails, so the UI can stop
        // showing a spinner that would otherwise never end.
        let (key, outcome) = rx.recv_timeout(Duration::from_secs(40)).expect("an answer");
        assert_eq!(key, "k");
        assert!(outcome.is_err());
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn an_empty_url_is_never_queued() {
        let (tx, _rx) = std::sync::mpsc::channel::<Fetched>();
        let mut downloads = Downloads::start(std::env::temp_dir().join("yptd-dl-empty"), tx, |f| f);
        assert!(!downloads.request("k", ""));
    }
}
