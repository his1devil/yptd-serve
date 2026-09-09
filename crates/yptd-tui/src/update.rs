//! What version this is, and whether a newer one has been published.
//!
//! The client is handed out privately rather than through a package manager,
//! so nothing else is going to tell anybody an update exists. A line in the
//! status bar and one command to take it is the whole mechanism: no silent
//! swap underneath a running client, which would leave the process people are
//! using and the binary on disk disagreeing about what they are.

use std::path::{Path, PathBuf};

use crate::config::Config;

/// Baked in at build time. `dist.sh` passes the release tag; a plain `cargo
/// build` falls back to the manifest, so a development tree still reports
/// something sensible.
pub const VERSION: &str = env!("YPTD_VERSION");

const TIMEOUT: u64 = 8;
/// Downloading a whole build is allowed to take much longer than asking what
/// the latest one is.
const DOWNLOAD_TIMEOUT: u64 = 180;
/// Downloads must not be able to fill a disk.
const MAX_BYTES: u64 = 128 * 1024 * 1024;

fn agent(seconds: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(seconds)))
        .build()
        .new_agent()
}

/// Fetches a small text file, trimmed.
fn text(url: &str, seconds: u64) -> Result<String, String> {
    agent(seconds)
        .get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map(|body| body.trim().to_owned())
        .map_err(|e| format!("{url}: {e}"))
}

/// Where the published files live: the same host the server is on, so a
/// self-hosted deployment needs no second setting.
fn base(config: &Config) -> String {
    if let Ok(explicit) = std::env::var("YPTD_BASE_URL") {
        return explicit.trim_end_matches('/').to_owned();
    }
    let server = config.server.trim_end_matches('/');
    // "https://host/yptd" -> "https://host/dl"
    match server.rfind('/') {
        Some(cut) if cut > "https://".len() => format!("{}/dl", &server[..cut]),
        _ => format!("{server}/dl"),
    }
}

/// The published version, or `None` when this build is already current.
///
/// Any failure is `None`: a client that cannot reach the internet has bigger
/// news to report than a missing update.
pub fn latest(config: &Config) -> Option<String> {
    let published = text(&format!("{}/VERSION", base(config)), TIMEOUT).ok()?;
    (!published.is_empty() && newer(&published, VERSION)).then_some(published)
}

/// Whether `candidate` is a later release than `current`.
///
/// Numeric field by field, so v0.10.0 beats v0.9.9 -- which string order gets
/// backwards. Anything unparseable counts as different-and-newer: better to
/// mention an update that turns out to be the same than to sit on a real one.
fn newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => candidate != current,
    }
}

fn parse(value: &str) -> Option<[u64; 3]> {
    let mut out = [0u64; 3];
    let trimmed = value.trim().trim_start_matches('v');
    // A tag like v0.1.0-3-gabc123 describes commits after a release; only the
    // release part is comparable, and the suffix makes it no older.
    let core = trimmed.split(['-', '+']).next()?;
    let mut fields = core.split('.');
    for slot in out.iter_mut() {
        *slot = fields.next()?.parse().ok()?;
    }
    Some(out)
}

/// Downloads the published build and puts it where this one is running from.
///
/// Returns the version now on disk. The running process keeps the file it
/// started with -- replacing a path does not touch an open inode -- so the
/// caller has to say that a restart is what makes it take effect.
pub fn install(config: &Config) -> Result<String, String> {
    let base = base(config);
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => return Err(format!("没有 {other} 的包，从源码构建")),
    };
    if std::env::consts::OS != "macos" {
        return Err("目前只发布 macOS 的包，从源码构建".into());
    }
    let name = format!("yptd-macos-{arch}.tar.gz");

    let target = std::env::current_exe().map_err(|e| format!("找不到自己的路径: {e}"))?;
    let dir = target
        .parent()
        .ok_or_else(|| "自己的路径没有上级目录".to_owned())?
        .to_path_buf();

    let tarball = fetch(&format!("{base}/{name}"))?;
    let sums = text(&format!("{base}/SHA256SUMS"), TIMEOUT)
        .map_err(|e| format!("取校验和失败: {e}"))?;
    verify(&tarball, &sums, &name)?;

    let staged = unpack(&tarball, &dir)?;
    // Rename over the old one: an atomic swap of the directory entry, which
    // is allowed even while that path is being executed.
    std::fs::rename(&staged, &target).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("替换 {} 失败: {e}", target.display())
    })?;

    Ok(text(&format!("{base}/VERSION"), TIMEOUT).unwrap_or_else(|_| "最新版".to_owned()))
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    agent(DOWNLOAD_TIMEOUT)
        .get(url)
        .call()
        .map_err(|e| format!("下载失败: {e}"))?
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_vec()
        .map_err(|e| format!("读取失败: {e}"))
}

/// Checks the download against the published list before anything is written.
fn verify(bytes: &[u8], sums: &str, name: &str) -> Result<(), String> {
    let expected = sums
        .lines()
        .find_map(|line| {
            let (sum, file) = line.split_once("  ")?;
            (file.trim() == name).then_some(sum.trim())
        })
        .ok_or_else(|| format!("校验和列表里没有 {name}"))?;
    let actual = sha256(bytes);
    if actual != expected {
        return Err("下载的包和校验和对不上，没有安装".into());
    }
    Ok(())
}

/// Unpacks the single binary out of the tarball, beside where it will land so
/// the rename that follows stays on one filesystem.
fn unpack(tarball: &[u8], dir: &Path) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let staged = dir.join(format!(".yptd-update-{}", std::process::id()));
    let mut child = Command::new("tar")
        .arg("xzf")
        .arg("-")
        .arg("-O")
        .arg("yptd")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("解包失败: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "解包进程没有标准输入".to_owned())?
        .write_all(tarball)
        .map_err(|e| format!("写入解包进程失败: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("解包失败: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err("包里没有 yptd".into());
    }
    std::fs::write(&staged, &out.stdout).map_err(|e| format!("写入 {} 失败: {e}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("设置权限失败: {e}"))?;
    }
    Ok(staged)
}

/// SHA-256, so a download can be checked without pulling in a crate for it.
fn sha256(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = bytes.to_vec();
    let bit_len = (bytes.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_release_is_recognised_and_the_same_one_is_not() {
        assert!(newer("v0.1.1", "v0.1.0"));
        assert!(newer("v0.2.0", "v0.1.9"));
        assert!(newer("v1.0.0", "v0.9.9"));
        assert!(!newer("v0.1.0", "v0.1.0"));
        assert!(!newer("v0.1.0", "v0.1.1"), "never offer to go backwards");
    }

    #[test]
    fn ten_beats_nine_which_string_order_gets_wrong() {
        assert!(newer("v0.10.0", "v0.9.0"));
        assert!(!newer("v0.9.0", "v0.10.0"));
    }

    #[test]
    fn a_describe_suffix_is_not_a_downgrade() {
        // What `git describe` produces between releases.
        assert!(!newer("v0.1.0", "v0.1.0-3-gabc1234"));
        assert!(newer("v0.2.0", "v0.1.0-3-gabc1234"));
    }

    #[test]
    fn something_unparseable_is_treated_as_different_rather_than_ignored() {
        assert!(newer("nightly", "v0.1.0"));
        assert!(!newer("v0.1.0", "v0.1.0"));
    }

    #[test]
    fn the_checksum_matches_a_known_answer() {
        // The empty string's SHA-256, so a wrong implementation cannot pass.
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_download_that_does_not_match_the_list_is_refused() {
        let sums = format!("{}  yptd-macos-arm64.tar.gz\n", sha256(b"good"));
        assert!(verify(b"good", &sums, "yptd-macos-arm64.tar.gz").is_ok());
        assert!(verify(b"tampered", &sums, "yptd-macos-arm64.tar.gz").is_err());
        assert!(verify(b"good", &sums, "yptd-macos-x86_64.tar.gz").is_err());
    }

    #[test]
    fn the_download_host_comes_from_the_configured_server() {
        let config = Config {
            server: "https://im.example.com/yptd".into(),
            api: String::new(),
            ws: String::new(),
            image: String::new(),
        };
        assert_eq!(base(&config), "https://im.example.com/dl");
    }
}
