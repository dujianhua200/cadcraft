//! CadCraft → ovimap sync bridge client (`docs/ovimap-link.md` §7.3, Phase 1.5).
//!
//! The bridge is the optional network channel beside file exchange (§7.1):
//! CadCraft pushes its GeoJSON (+ optional DXF) to ovimap's sync server and
//! pulls the server's latest copy back, per project.
//!
//! * **Endpoint**: `PUT /project/:id/cadcraft` / `GET /project/:id/cadcraft`
//!   on the sync server (same auth, envelope and `baseRev` optimistic
//!   concurrency as `PUT /project/:id`). CadCraft never touches a collection's
//!   `payload` — only the bridge endpoint.
//! * **Body**: always the JSON envelope
//!   `{baseRev, geojson, dxf?, deviceId, deviceName, updatedAt}` — never a bare
//!   GeoJSON document. `dxf` is base64 DXF when present.
//! * **409**: `baseRev` mismatch → [`PushOutcome::Conflict`] carrying the
//!   server's copy (same shape as the push body). The caller decides —
//!   CadCraft never overwrites silently, and the conflict is reported, not
//!   retried.
//! * **Secrets**: the token travels only in the `Authorization: Bearer`
//!   header. It is never written to a file, never put in a URL, and never
//!   included in an error message. The caller supplies it per call or via
//!   `CADCRAFT_OVIMAP_TOKEN`; there is no stored credential.
//! * **Transport**: `ureq` + rustls (the workspace's only HTTP/TLS
//!   dependency). Wasm has no bridge client: push/pull return an error.

use std::time::Duration;

use cadcraft_doc::gis::utc_now_iso;
use serde::{Deserialize, Serialize};

use crate::cmd::file::{base64_decode, base64_encode};

/// Per-request timeout for bridge calls (a project payload is small text).
const TIMEOUT: Duration = Duration::from_secs(30);

/// The push/pull envelope (§7.3, as implemented by the ovimap sync server):
/// the GeoJSON is a *string* field, never the bare document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeEnvelope {
    /// The `baseRev` the push is based on (0 for "no known revision").
    pub base_rev: u64,
    /// The GeoJSON document as text.
    pub geojson: String,
    /// Base64 DXF, when the push carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dxf: Option<String>,
    /// Sending device id (also sent as `X-Device-Id`).
    pub device_id: String,
    /// Sending device name (also sent as `X-Device-Name`).
    pub device_name: String,
    /// ISO-8601 timestamp of this copy.
    pub updated_at: String,
}

impl BridgeEnvelope {
    /// A new envelope for `geojson`, stamped now.
    pub fn new(base_rev: u64, geojson: String, dxf: Option<Vec<u8>>, device_id: &str, device_name: &str) -> Self {
        BridgeEnvelope {
            base_rev,
            geojson,
            dxf: dxf.map(|b| base64_encode(&b)),
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            updated_at: utc_now_iso(),
        }
    }

    /// The DXF payload decoded, when present and valid base64.
    pub fn dxf_bytes(&self) -> Option<Vec<u8>> {
        self.dxf.as_deref().and_then(base64_decode)
    }
}

/// What a push returns: the new revision, or the server's copy on conflict.
#[derive(Clone, Debug, PartialEq)]
pub enum PushOutcome {
    /// Accepted: the server's new revision.
    Updated { rev: u64 },
    /// `baseRev` mismatch: the server's copy, for the caller to reconcile.
    Conflict { server_rev: u64, server_geojson: Option<String>, server_dxf: Option<Vec<u8>> },
}

/// What a pull returns: the server's latest copy.
#[derive(Clone, Debug, PartialEq)]
pub struct Pulled {
    /// The server's revision of this copy.
    pub rev: u64,
    /// ISO-8601 timestamp of this copy.
    pub updated_at: String,
    /// The GeoJSON document as text.
    pub geojson: String,
    /// Base64-decoded DXF, when the server carries one.
    pub dxf: Option<Vec<u8>>,
}

/// The sync server's envelope: `{code, data, message}`.
#[derive(Debug, Deserialize)]
struct ServerEnvelope {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    data: serde_json::Value,
    #[serde(default)]
    message: String,
}

/// The bridge client: base URL + token + device identity. The token is held
/// in memory only, for the lifetime of this client.
#[derive(Clone, Debug)]
pub struct BridgeClient {
    base_url: String,
    token: String,
    device_id: String,
    device_name: String,
}

impl BridgeClient {
    /// Build a client. `base_url` must be `http(s)://…` (trailing `/` is
    /// stripped); `token` and `device_id` must be non-empty. The token is
    /// never logged or stored.
    pub fn new(base_url: &str, token: &str, device_id: &str, device_name: &str) -> Result<Self, String> {
        let base_url = base_url.trim().trim_end_matches('/');
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err("桥接地址须为 http:// 或 https:// 开头".into());
        }
        if token.trim().is_empty() {
            return Err("桥接 token 为空（传 token 参数或设 CADCRAFT_OVIMAP_TOKEN）".into());
        }
        if device_id.trim().is_empty() {
            return Err("deviceId 为空".into());
        }
        Ok(BridgeClient {
            base_url: base_url.to_string(),
            token: token.trim().to_string(),
            device_id: device_id.trim().to_string(),
            device_name: if device_name.trim().is_empty() { "CadCraft".into() } else { device_name.trim().to_string() },
        })
    }

    /// A client from the environment: `CADCRAFT_OVIMAP_BASE_URL` (required),
    /// `CADCRAFT_OVIMAP_TOKEN` (required), `CADCRAFT_OVIMAP_DEVICE_ID` /
    /// `CADCRAFT_OVIMAP_DEVICE_NAME` (defaults: `cadcraft-desktop` /
    /// `CadCraft`).
    pub fn from_env() -> Result<Self, String> {
        let base_url = std::env::var("CADCRAFT_OVIMAP_BASE_URL").unwrap_or_default();
        let token = std::env::var("CADCRAFT_OVIMAP_TOKEN").unwrap_or_default();
        let device_id = std::env::var("CADCRAFT_OVIMAP_DEVICE_ID").unwrap_or_else(|_| "cadcraft-desktop".into());
        let device_name = std::env::var("CADCRAFT_OVIMAP_DEVICE_NAME").unwrap_or_else(|_| "CadCraft".into());
        if base_url.trim().is_empty() {
            return Err("桥接地址为空（传 baseUrl 参数或设 CADCRAFT_OVIMAP_BASE_URL）".into());
        }
        BridgeClient::new(&base_url, &token, &device_id, &device_name)
    }

    /// Build the push envelope for `geojson` (+ optional DXF bytes) with
    /// this client's device identity, stamped now.
    pub fn envelope(&self, base_rev: u64, geojson: String, dxf: Option<Vec<u8>>) -> BridgeEnvelope {
        BridgeEnvelope::new(base_rev, geojson, dxf, &self.device_id, &self.device_name)
    }

    /// Push `envelope` to `PUT /project/:id/cadcraft`. A 409 (or `code` 409)
    /// becomes [`PushOutcome::Conflict`], never an overwrite, never a retry.
    pub fn push(&self, project_id: &str, envelope: &BridgeEnvelope) -> Result<PushOutcome, String> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (project_id, envelope);
            return Err("网页版不支持桥接同步".into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let url = self.project_url(project_id)?;
            let body = serde_json::to_string(envelope).map_err(|e| format!("桥接推送序列化失败：{e}"))?;
            let mut resp = self
                .agent()
                .put(&url)
                .header("Authorization", &format!("Bearer {}", self.token))
                .header("X-Device-Id", &self.device_id)
                .header("X-Device-Name", &self.device_name)
                .header("Content-Type", "application/json")
                .send(body)
                .map_err(|e| bridge_error("推送", &e))?;
            let text = resp.body_mut().read_to_string().map_err(|e| format!("桥接推送读取响应失败：{e}"))?;
            let status = resp.status();
            let env: ServerEnvelope = match serde_json::from_str(&text) {
                Ok(env) => env,
                // A 409 with an unreadable body is still a conflict: the
                // caller's baseRev is stale, the server's copy is unknown.
                Err(_) if status == 409 => {
                    return Ok(PushOutcome::Conflict { server_rev: envelope.base_rev, server_geojson: None, server_dxf: None });
                }
                Err(_) => return Err("桥接推送：服务端响应不是 JSON 信封".into()),
            };
            if status == 409 || env.code == 409 {
                let server_rev = rev_of(&env.data).unwrap_or(envelope.base_rev);
                let server_geojson = env.data.get("geojson").and_then(|v| v.as_str()).map(str::to_string);
                let server_dxf = env.data.get("dxf").and_then(|v| v.as_str()).and_then(base64_decode);
                return Ok(PushOutcome::Conflict { server_rev, server_geojson, server_dxf });
            }
            if env.code != 0 {
                let msg = if env.message.trim().is_empty() { format!("服务端返回 code {}", env.code) } else { env.message };
                return Err(format!("桥接推送失败：{msg}"));
            }
            let rev = rev_of(&env.data).ok_or_else(|| "桥接推送：服务端响应缺少 rev".to_string())?;
            Ok(PushOutcome::Updated { rev })
        }
    }

    /// Pull the server's latest copy: `GET /project/:id/cadcraft`.
    pub fn pull(&self, project_id: &str) -> Result<Pulled, String> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = project_id;
            return Err("网页版不支持桥接同步".into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let url = self.project_url(project_id)?;
            let mut resp = self
                .agent()
                .get(&url)
                .header("Authorization", &format!("Bearer {}", self.token))
                .header("X-Device-Id", &self.device_id)
                .header("X-Device-Name", &self.device_name)
                .call()
                .map_err(|e| bridge_error("拉取", &e))?;
            let text = resp.body_mut().read_to_string().map_err(|e| format!("桥接拉取读取响应失败：{e}"))?;
            let env: ServerEnvelope = serde_json::from_str(&text).map_err(|_| "桥接拉取：服务端响应不是 JSON 信封".to_string())?;
            if env.code != 0 {
                let msg = if env.message.trim().is_empty() { format!("服务端返回 code {}", env.code) } else { env.message };
                return Err(format!("桥接拉取失败：{msg}"));
            }
            let data = &env.data;
            let geojson = data.get("geojson").and_then(|v| v.as_str()).ok_or_else(|| "桥接拉取：服务端响应缺少 geojson".to_string())?.to_string();
            let rev = rev_of(data).ok_or_else(|| "桥接拉取：服务端响应缺少 rev".to_string())?;
            let updated_at = data.get("updatedAt").or_else(|| data.get("updated_at")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let dxf = data.get("dxf").and_then(|v| v.as_str()).and_then(base64_decode);
            Ok(Pulled { rev, updated_at, geojson, dxf })
        }
    }

    fn project_url(&self, project_id: &str) -> Result<String, String> {
        if project_id.trim().is_empty() {
            return Err("projectId 为空".into());
        }
        Ok(format!("{}/project/{}/cadcraft", self.base_url, encode_segment(project_id.trim())))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn agent(&self) -> ureq::Agent {
        static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
        // HTTP statuses are data, not errors: a 409 carries the server copy the
        // caller must surface, so it has to reach the status checks below.
        AGENT.get_or_init(|| ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).http_status_as_error(false).build().into()).clone()
    }
}

/// Percent-encode one URL path segment (RFC 3986 unreserved set kept bare).
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(char::from(b)),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The revision in a response `data` object: `rev`, `baseRev` or `newRev`.
fn rev_of(data: &serde_json::Value) -> Option<u64> {
    data.get("rev").or_else(|| data.get("baseRev")).or_else(|| data.get("newRev")).and_then(|v| v.as_u64())
}

/// An HTTP error as a message: status and kind only — never the token, never
/// the response body (it may echo request data).
#[cfg(not(target_arch = "wasm32"))]
fn bridge_error(op: &str, e: &ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(code) => format!("桥接{op}失败：HTTP {code}"),
        ureq::Error::Timeout(_) => format!("桥接{op}超时"),
        ureq::Error::ConnectionFailed => format!("桥接{op}连接失败"),
        ureq::Error::Io(_) => format!("桥接{op}读取失败"),
        other => format!("桥接{op}失败：{other}"),
    }
}
