//! Talking to yptd-server: register with an invitation, exchange a device
//! credential for an OpenIM session.

use serde::Deserialize;

use crate::config::{Config, Credentials};

#[derive(Debug, Deserialize)]
struct Session {
    user_id: String,
    nickname: String,
    #[serde(default)]
    device_token: String,
    im_token: String,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// A successful registration: the credential to keep for later starts.
pub struct Login {
    pub credentials: Credentials,
}

/// Reads the server's error body when a request is rejected, so the person
/// sees "邀请码已被使用" rather than "HTTP 400".
fn post(url: &str, body: serde_json::Value) -> Result<Session, Error> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut response = agent
        .post(url)
        .send_json(&body)
        .map_err(|e| Error(format!("连不上服务端 {url}: {e}")))?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return response
            .body_mut()
            .read_json::<Session>()
            .map_err(|e| Error(format!("服务端返回了无法解析的内容: {e}")));
    }
    let detail = response
        .body_mut()
        .read_json::<ApiError>()
        .map(|e| if e.message.is_empty() { e.error } else { e.message })
        .unwrap_or_default();
    if detail.is_empty() {
        Err(Error(format!("服务端返回 HTTP {status}")))
    } else {
        Err(Error(detail))
    }
}

/// First-time registration with an invitation code.
pub fn register(config: &Config, invite: &str, nickname: &str, user_id: Option<&str>) -> Result<Login, Error> {
    let mut body = serde_json::json!({
        "invite_code": invite,
        "nickname": nickname,
        "device_name": device_name(),
        "platform_id": platform_id(),
    });
    if let Some(id) = user_id {
        body["user_id"] = serde_json::Value::String(id.to_owned());
    }
    let s = post(&format!("{}/v1/register", config.server), body)?;
    if s.device_token.is_empty() {
        return Err(Error("服务端没有返回设备凭据".into()));
    }
    Ok(Login {
        credentials: Credentials {
            user_id: s.user_id,
            nickname: s.nickname,
            device_token: s.device_token,
        },
    })
}

/// Every subsequent start: device credential → fresh OpenIM token.
pub fn login(config: &Config, creds: &Credentials) -> Result<String, Error> {
    let body = serde_json::json!({
        "device_token": creds.device_token,
        "platform_id": platform_id(),
    });
    let s = post(&format!("{}/v1/login", config.server), body)?;
    Ok(s.im_token)
}

/// OpenIM platform id for this OS. The server uses it for multi-login policy
/// and it shows up on every message we send.
pub fn platform_id() -> i32 {
    if cfg!(target_os = "macos") {
        4
    } else if cfg!(target_os = "windows") {
        3
    } else {
        7
    }
}

fn device_name() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into());
    format!("{host} ({})", std::env::consts::OS)
}
