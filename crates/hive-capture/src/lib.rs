//! Inbound Google capture is separate from legacy LWW todo synchronization.
//! Google retains phone captures while the desktop is offline. This crate stages
//! observations durably and delegates canonical Markdown writes exclusively to mw.
pub mod google;
pub mod handoff;
pub mod mindweaver;
pub mod store;

use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleTask {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub assignment_info: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootstrapPolicy {
    Baseline,
    ImportOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Baseline,
    Pending,
    Delivering,
    Delivered,
    NeedsReview,
    CalendarPending,
    Ignored,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationBinding {
    pub notes_dir: String,
    pub inbox_path: String,
    pub dashboard_path: String,
}

impl DestinationBinding {
    pub fn validate(&self) -> Result<()> {
        for path in [&self.notes_dir, &self.inbox_path, &self.dashboard_path] {
            ensure!(
                Path::new(path).is_absolute() && !path.chars().any(char::is_control),
                "capture destination must be an absolute, control-free path"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureJob {
    pub capture_id: String,
    pub account_key: String,
    pub list_id: String,
    pub provider_id: String,
    pub title: String,
    pub state: DeliveryState,
    pub attempts: u64,
    pub binding: Option<DestinationBinding>,
    pub receipt: Option<Value>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub provider_changed: bool,
    pub observation: GoogleTask,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestReport {
    pub observed: usize,
    pub staged: usize,
    pub baselined: usize,
    pub deferred: usize,
    pub unchanged: usize,
    pub needs_review: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryReport {
    pub delivered: usize,
    pub reconciled: usize,
    pub blocked: usize,
    pub pending: usize,
    pub unsupported: usize,
    pub calendar_pending: usize,
    pub baselined: usize,
    pub ignored: usize,
}
