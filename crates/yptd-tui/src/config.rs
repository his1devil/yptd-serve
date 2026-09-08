//! Where the client keeps its files, and what it remembers between runs.
//!
//! Everything lives under `~/.yptd/`, one short path on purpose: the sidecar's
//! Unix socket sits here too, and `sockaddr_un` caps that path at about 100
//! bytes. An XDG layout would be tidier and would push the socket over the
//! limit on a deep home directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Endpoints. Written once by `yptd login`, read on every start.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    /// yptd-server base URL, e.g. `https://im.zhanghuanyang.com/yptd`.
    pub server: String,
    /// OpenIM API base URL.
    pub api: String,
    /// OpenIM WebSocket gateway URL.
    pub ws: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: "https://im.zhanghuanyang.com/yptd".into(),
            api: "https://im.zhanghuanyang.com".into(),
            ws: "wss://im.zhanghuanyang.com/ws".into(),
        }
    }
}

/// The long-lived device credential. Exchanged for a fresh OpenIM token on
/// every start, so the IM token itself is never written to disk.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Credentials {
    pub user_id: String,
    pub nickname: String,
    pub device_token: String,
}

pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn discover() -> Self {
        let root = std::env::var_os("YPTD_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".yptd")))
            .unwrap_or_else(|| PathBuf::from(".yptd"));
        Self { root }
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    pub fn credentials(&self) -> PathBuf {
        self.root.join("credentials.toml")
    }
    pub fn socket(&self) -> PathBuf {
        self.root.join("sock")
    }
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    pub fn load_config(&self) -> Config {
        std::fs::read_to_string(self.config())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_config(&self, config: &Config) -> std::io::Result<()> {
        self.ensure()?;
        let body = toml::to_string_pretty(config).map_err(std::io::Error::other)?;
        write_private(&self.config(), &body)
    }

    pub fn load_credentials(&self) -> Option<Credentials> {
        let s = std::fs::read_to_string(self.credentials()).ok()?;
        toml::from_str(&s).ok()
    }

    pub fn save_credentials(&self, creds: &Credentials) -> std::io::Result<()> {
        self.ensure()?;
        let body = toml::to_string_pretty(creds).map_err(std::io::Error::other)?;
        write_private(&self.credentials(), &body)
    }

    pub fn clear_credentials(&self) -> std::io::Result<()> {
        match std::fs::remove_file(self.credentials()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Writes a file readable only by its owner. The credential is a bearer
/// token; a world-readable file would be a login for anyone on the machine.
fn write_private(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths() -> Paths {
        let root = std::env::temp_dir().join(format!("yptd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Paths { root }
    }

    #[test]
    fn credentials_round_trip_and_are_private() {
        let p = temp_paths();
        let creds = Credentials {
            user_id: "lina".into(),
            nickname: "李娜".into(),
            device_token: "yptd_abc".into(),
        };
        p.save_credentials(&creds).unwrap();
        let back = p.load_credentials().expect("reload");
        assert_eq!(back.device_token, "yptd_abc");
        assert_eq!(back.nickname, "李娜");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(p.credentials()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "credential file must be owner-only");
        }
        let _ = std::fs::remove_dir_all(&p.root);
    }

    #[test]
    fn a_missing_config_falls_back_to_defaults_rather_than_failing() {
        let p = temp_paths();
        let c = p.load_config();
        assert!(c.server.starts_with("https://"));
        assert!(p.load_credentials().is_none());
    }

    #[test]
    fn clearing_absent_credentials_is_not_an_error() {
        let p = temp_paths();
        p.clear_credentials().expect("no file is fine");
    }

    #[test]
    fn the_socket_path_stays_short() {
        let p = Paths::discover();
        // Leave headroom under the ~100-byte sockaddr_un limit.
        assert!(
            p.socket().as_os_str().len() < 90,
            "socket path too long: {}",
            p.socket().display()
        );
    }
}
