//! The sidecar that ships inside this binary.
//!
//! A release build carries the sidecar as bytes and writes it out on first
//! run, so what people download and hand to each other is one file. The copy
//! is named after the hash of the bytes it came from: an upgraded client
//! never runs the previous build's sidecar, which is the failure the single
//! file exists to prevent.
//!
//! The bytes are written back out verbatim, so a sidecar signed before it was
//! embedded is still signed when it is unpacked -- and a file this process
//! wrote itself carries no quarantine attribute, so Gatekeeper has nothing to
//! object to.

use std::path::PathBuf;

use crate::config::Paths;

#[cfg(embedded_sidecar)]
const SIDECAR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sidecar.bin"));

/// Where this build's sidecar lives, or `None` to let the spawner look beside
/// the executable and along `PATH` -- which is what a development tree wants.
pub fn sidecar_path(paths: &Paths) -> Option<PathBuf> {
    // An explicit path still wins, for running a locally built sidecar
    // against an installed client.
    if let Some(explicit) = std::env::var_os("YPTD_SIDECAR") {
        return Some(explicit.into());
    }
    unpack(paths)
}

/// Whether this build carries its own sidecar, for `doctor` to report.
pub fn is_embedded() -> bool {
    cfg!(embedded_sidecar)
}

#[cfg(not(embedded_sidecar))]
fn unpack(_paths: &Paths) -> Option<PathBuf> {
    None
}

#[cfg(embedded_sidecar)]
fn unpack(paths: &Paths) -> Option<PathBuf> {
    let dir = paths.root.join("bin");
    let name = concat!("yptd-sidecar-", env!("YPTD_SIDECAR_HASH"));
    let path = dir.join(name);
    if path.is_file() {
        return Some(path);
    }
    std::fs::create_dir_all(&dir).ok()?;

    // Write beside it and rename: a half-written sidecar must never be
    // reachable under the real name, and renaming over a copy that another
    // instance is running is allowed where overwriting one in place is not.
    let staged = dir.join(format!(".staged-{}", std::process::id()));
    std::fs::write(&staged, SIDECAR).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).ok()?;
    }
    if let Err(e) = std::fs::rename(&staged, &path) {
        let _ = std::fs::remove_file(&staged);
        // Another instance getting there first is a success, not a failure.
        return path.is_file().then_some(path).ok_or(e).ok();
    }
    sweep(&dir, name);
    Some(path)
}

/// Removes the sidecars left by earlier builds. Thirty megabytes each is too
/// much to leave lying around after an upgrade.
#[cfg(embedded_sidecar)]
fn sweep(dir: &std::path::Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name != keep && name.starts_with("yptd-sidecar-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
