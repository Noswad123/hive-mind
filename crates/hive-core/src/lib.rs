use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub mod google_identity;
pub use google_identity::GoogleIdentity;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_PWA_DIR: &str = "hive-pwa";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProjectLayout {
    pub root: PathBuf,
    pub pwa_dir: PathBuf,
    pub docs_dir: PathBuf,
}

impl ProjectLayout {
    pub fn discover(root: Option<PathBuf>) -> Result<Self> {
        let root = match root {
            Some(root) => root,
            None => std::env::current_dir().context("read current directory")?,
        };
        Ok(Self {
            pwa_dir: root.join(DEFAULT_PWA_DIR),
            docs_dir: root.join("docs"),
            root,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorStatus::Fail)
    }
}

pub fn run_local_doctor(layout: &ProjectLayout) -> DoctorReport {
    let checks = vec![
        path_check("project root", &layout.root, true),
        path_check("docs", &layout.docs_dir, true),
        path_check("pwa", &layout.pwa_dir, true),
        path_check("pwa package", &layout.pwa_dir.join("package.json"), false),
        path_check(
            "pwa sync engine",
            &layout.pwa_dir.join("src/hiveSyncEngine.ts"),
            false,
        ),
    ];
    DoctorReport { checks }
}

fn path_check(name: &str, path: &std::path::Path, directory: bool) -> DoctorCheck {
    let exists = if directory {
        path.is_dir()
    } else {
        path.is_file()
    };
    DoctorCheck {
        name: name.to_string(),
        status: if exists {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        },
        message: path.display().to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStateResponse {
    pub server_time: String,
    pub latest_cursor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRegistration {
    pub device_id: String,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub app_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRegisterResponse {
    pub device_id: String,
    pub registered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCheckReport {
    pub endpoint: String,
    pub device_id: String,
    pub status: String,
    pub http_status: u16,
    pub registered: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Note,
    Todo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    Upsert,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncOperation {
    pub op_id: String,
    pub idempotency_key: String,
    pub entity_type: EntityType,
    pub entity_id: String,
    pub op_type: OperationType,
    pub payload: serde_json::Value,
    pub client_updated_at: String,
    pub base_version: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushRequest {
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
    pub app_version: String,
    pub operations: Vec<SyncOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushReject {
    pub op_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictEvent {
    pub entity_type: EntityType,
    pub entity_id: String,
    pub winner: String,
    pub loser: String,
    pub reason: String,
    pub winner_device_id: String,
    pub loser_device_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushResponse {
    pub accepted: Vec<String>,
    pub rejected: Vec<PushReject>,
    pub server_cursor: String,
    pub conflicts: Vec<ConflictEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullResponse {
    pub operations: Vec<SyncOperation>,
    pub next_cursor: String,
    pub has_more: bool,
}

pub fn fetch_sync_state(endpoint: &str, token: Option<&str>) -> Result<SyncStateResponse> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    if endpoint.is_empty() {
        bail!("endpoint is required");
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let mut req = client.get(format!("{endpoint}/v1/sync/state"));
    if let Some(token) = token.map(str::trim).filter(|value| !value.is_empty()) {
        req = req.bearer_auth(token);
    }

    let resp = req.send().context("request sync state")?;
    let status = resp.status();
    let body = resp.text().context("read sync state response")?;
    if !status.is_success() {
        bail!("sync state request failed: status {status} body={body}");
    }

    serde_json::from_str(&body).context("decode sync state response")
}

pub fn register_device(
    endpoint: &str,
    token: Option<&str>,
    req: &DeviceRegistration,
) -> Result<DeviceRegisterResponse> {
    request_json(
        endpoint,
        token,
        reqwest::Method::POST,
        "/v1/devices/register",
        Some(req),
    )
}

pub fn check_token_for_device(
    endpoint: &str,
    token: &str,
    device_id: &str,
) -> Result<TokenCheckReport> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    let device_id = device_id.trim();
    let token = token.trim();
    if endpoint.is_empty() {
        bail!("endpoint is required");
    }
    if device_id.is_empty() {
        bail!("device-id is required");
    }
    if token.is_empty() {
        bail!("token is required");
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let resp = client
        .post(format!("{endpoint}/v1/devices/register"))
        .bearer_auth(token)
        .json(&DeviceRegistration {
            device_id: device_id.to_string(),
            device_name: String::new(),
            platform: String::new(),
            app_version: String::new(),
        })
        .send()
        .context("token check request")?;
    let status = resp.status();
    let http_status = status.as_u16();
    let body = resp.text().context("read token check response")?;
    if status.is_success() {
        let registered = serde_json::from_str::<DeviceRegisterResponse>(&body)
            .map(|payload| payload.registered)
            .unwrap_or(false);
        return Ok(TokenCheckReport {
            endpoint: endpoint.to_string(),
            device_id: device_id.to_string(),
            status: if registered { "ok" } else { "attention" }.to_string(),
            http_status,
            registered,
            message: if registered {
                "token is valid for device_id"
            } else {
                "registration response did not confirm token/device match"
            }
            .to_string(),
        });
    }

    let message = extract_error_message(&body);
    Ok(TokenCheckReport {
        endpoint: endpoint.to_string(),
        device_id: device_id.to_string(),
        status: match http_status {
            401 => "unauthorized",
            403 => "forbidden",
            _ => "error",
        }
        .to_string(),
        http_status,
        registered: false,
        message,
    })
}

fn extract_error_message(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.as_str())
                .map(str::trim)
                .map(ToOwned::to_owned)
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| trimmed.to_string())
}

pub fn push_operations(
    endpoint: &str,
    token: Option<&str>,
    req: &PushRequest,
) -> Result<PushResponse> {
    request_json(
        endpoint,
        token,
        reqwest::Method::POST,
        "/v1/sync/push",
        Some(req),
    )
}

pub fn pull_operations(
    endpoint: &str,
    token: Option<&str>,
    cursor: &str,
    limit: usize,
) -> Result<PullResponse> {
    let cursor = cursor.trim();
    let cursor = if cursor.is_empty() { "0" } else { cursor };
    let limit = if limit == 0 { 100 } else { limit };
    request_json::<(), PullResponse>(
        endpoint,
        token,
        reqwest::Method::GET,
        &format!("/v1/sync/pull?cursor={cursor}&limit={limit}"),
        None,
    )
}

fn request_json<T, R>(
    endpoint: &str,
    token: Option<&str>,
    method: reqwest::Method,
    path: &str,
    body: Option<&T>,
) -> Result<R>
where
    T: Serialize + ?Sized,
    R: for<'de> Deserialize<'de>,
{
    let endpoint = endpoint.trim().trim_end_matches('/');
    if endpoint.is_empty() {
        bail!("endpoint is required");
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let mut req = client.request(method, format!("{endpoint}{path}"));
    if let Some(token) = token.map(str::trim).filter(|value| !value.is_empty()) {
        req = req.bearer_auth(token);
    }
    if let Some(body) = body {
        req = req.json(body);
    }
    let resp = req.send().context("send request")?;
    let status = resp.status();
    let text = resp.text().context("read response")?;
    if !status.is_success() {
        bail!("request failed: status {status} body={text}");
    }
    serde_json::from_str(&text).context("decode response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_doctor_reports_missing_paths() {
        let layout = ProjectLayout {
            root: PathBuf::from("/definitely/missing/hive-mind"),
            pwa_dir: PathBuf::from("/definitely/missing/hive-mind/hive-pwa"),
            docs_dir: PathBuf::from("/definitely/missing/hive-mind/docs"),
        };

        let report = run_local_doctor(&layout);
        assert!(report.has_failures());
        assert_eq!(report.checks.len(), 5);
    }

    #[test]
    fn parses_sync_state_response() {
        let state: SyncStateResponse =
            serde_json::from_str(r#"{"server_time":"2026-07-10T00:00:00Z","latest_cursor":"42"}"#)
                .unwrap();
        assert_eq!(state.latest_cursor, "42");
    }

    #[test]
    fn protocol_operation_matches_existing_wire_shape() {
        let op: SyncOperation = serde_json::from_str(
            r#"{
                "op_id":"op-1",
                "idempotency_key":"idem-1",
                "entity_type":"note",
                "entity_id":"notes/example.md",
                "op_type":"upsert",
                "payload":{"path":"notes/example.md"},
                "client_updated_at":"2026-07-10T00:00:00Z",
                "base_version":3
            }"#,
        )
        .unwrap();

        assert_eq!(op.entity_type, EntityType::Note);
        assert_eq!(op.op_type, OperationType::Upsert);
        assert_eq!(op.base_version, 3);
    }
}
