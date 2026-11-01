//! Per-user bounded capture scheduling. This is cooperative editor coordination,
//! not filesystem-wide exclusion against arbitrary same-user writers.
pub mod config;
pub mod editor;
pub mod launchd;
pub mod process;
pub mod state;

use hive_capture::{DeliveryReport, DestinationBinding, IngestReport};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

pub const SERVICE_LABEL: &str = "com.hive-mind.google-captures";
pub const EDITOR_PROTOCOL: &str = "1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerConfig {
    pub schema_version: String,
    pub connection: String,
    pub account_namespace: String,
    pub queue_path: PathBuf,
    pub list_id: String,
    pub mw_binary: PathBuf,
    pub hive_binary: PathBuf,
    pub nvim_binary: PathBuf,
    pub working_directory: PathBuf,
    pub destination: DestinationBinding,
    pub mw_environment: BTreeMap<String, String>,
    pub interval_seconds: u64,
    pub cycle_timeout_seconds: u64,
    pub delivery_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: u32,
    pub uid: u32,
    pub process_group: u32,
    pub start_identity: String,
    pub executable: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditorEntry {
    pub schema_version: String,
    pub session_id: String,
    pub pid: u32,
    pub process_start: String,
    pub socket_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferralReason {
    DirtyBuffers,
    EditorBusy,
    EditorUnavailable,
    UnregisteredEditor,
    MultipleEditors,
    EditorContextMismatch,
    AdmissionBusy,
    SourceLocked,
    CycleBudget,
    UiActive,
    UnknownOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CycleStatus {
    pub schema_version: String,
    pub kind: String,
    pub cycle_id: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub phase: String,
    pub outcome: String,
    pub collected: Option<IngestReport>,
    pub delivery: Option<DeliveryReport>,
    pub pending_count: usize,
    pub deferred: Option<DeferralReason>,
    pub auth_action_required: bool,
    pub auth_generation: Option<u64>,
    #[serde(default)]
    pub auth_binding_digest: Option<String>,
    #[serde(default)]
    pub config_digest: Option<String>,
    pub worker_pid: Option<u32>,
    pub worker_pgid: Option<u32>,
}

impl CycleStatus {
    pub fn new(cycle_id: String) -> Self {
        Self {
            schema_version: "1".into(),
            kind: "capture-background-status".into(),
            cycle_id,
            started_at_ms: now_ms(),
            finished_at_ms: None,
            phase: "starting".into(),
            outcome: "running".into(),
            collected: None,
            delivery: None,
            pending_count: 0,
            deferred: None,
            auth_action_required: false,
            auth_generation: None,
            auth_binding_digest: None,
            config_digest: None,
            worker_pid: Some(std::process::id()),
            worker_pgid: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryLease {
    pub schema_version: String,
    pub cycle_id: String,
    pub config_digest: String,
    pub coordinator_pid: u32,
    pub coordinator_start: String,
    pub deadline_ms: u64,
    pub destination: DestinationBinding,
    pub editor_session: Option<String>,
    pub helper_pid: Option<u32>,
    pub helper_pgid: Option<u32>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub fn nonce() -> anyhow::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("could not generate worker identity"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
