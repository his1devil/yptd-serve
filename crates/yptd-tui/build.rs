//! Embeds the sidecar binary when the build is handed one.
//!
//! OpenIM's client SDK is Go-only, so the half of the client that talks to
//! the server is a second executable. Asking people to download two files and
//! keep them in step is a needless way to break an install, so a release
//! carries the sidecar inside it: `make dist` signs the sidecar, then builds
//! `yptd` with `YPTD_EMBED_SIDECAR` pointing at it.
//!
//! A plain `cargo build` embeds nothing and the client falls back to finding
//! the sidecar beside itself, which is what a development tree has.

use std::path::PathBuf;

fn main() {
    // The release tag when `dist.sh` builds, the manifest otherwise, so a
    // development tree still reports something rather than nothing.
    println!("cargo::rerun-if-env-changed=YPTD_VERSION");
    let version = std::env::var("YPTD_VERSION")
        .ok()
        .or_else(described)
        .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")));
    println!("cargo::rustc-env=YPTD_VERSION={version}");

    println!("cargo::rerun-if-env-changed=YPTD_EMBED_SIDECAR");
    println!("cargo::rustc-check-cfg=cfg(embedded_sidecar)");
    let Some(src) = std::env::var_os("YPTD_EMBED_SIDECAR") else {
        return;
    };
    let src = PathBuf::from(src);
    println!("cargo::rerun-if-changed={}", src.display());
    let bytes = std::fs::read(&src)
        .unwrap_or_else(|e| panic!("YPTD_EMBED_SIDECAR={}: {e}", src.display()));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("sidecar.bin");
    std::fs::write(&out, &bytes).expect("stage the sidecar for embedding");
    // The unpacked copy is named after this, so a client only ever runs the
    // sidecar it was built with and an upgrade cannot leave the two out of
    // step.
    println!("cargo::rustc-env=YPTD_SIDECAR_HASH={:016x}", fnv1a(&bytes));
    println!("cargo::rustc-cfg=embedded_sidecar");
}

/// What `git describe` says, so a build from a working tree identifies
/// itself as something other than the release it sits after. A release build
/// is handed its version instead and never gets here.
fn described() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// FNV-1a: not a defence against tampering, just a short stable name for one
/// particular build's bytes.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
