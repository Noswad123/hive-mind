//! Portable operator-accepted evidence, not a signature or provider write authority.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};

use crate::{CaptureJob, DeliveryState, GoogleTask, store::deterministic_capture_id};

pub const MAX_JSON_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskHandoff {
    pub schema_version: String,
    pub kind: String,
    pub source_repo_id: String,
    pub capture_id: String,
    pub account_key: String,
    pub list_id: String,
    pub provider_id: String,
    pub receipt: ReceiptEvidence,
    pub accepted_current_baseline: bool,
    pub baseline: GoogleTask,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceiptEvidence {
    pub method: String,
    /// SHA-256 of serde_json's serialized stored receipt, not a signature.
    pub sha256: String,
    pub stable_id: String,
    pub source_id: String,
    pub source_path: String,
    pub source_revision: String,
}

pub fn opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn relative(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && value
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git"))
        && Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Conservative first-milestone shape; unknown Google fields require review.
pub fn validate_open_task(task: &GoogleTask) -> Result<()> {
    ensure!(
        task.status == "needsAction" && !task.deleted && !task.hidden,
        "task is completed, deleted, hidden, or has unsupported status"
    );
    ensure!(
        crate::store::unsupported_reason(task).is_none() && task.title.len() <= 8192,
        "unsupported task shape"
    );
    for (key, value) in &task.extra {
        let value = value
            .as_str()
            .context("provider metadata must be a string")?;
        ensure!(
            !value.is_empty()
                && value.len() <= 4096
                && value.is_ascii()
                && !value.chars().any(char::is_control),
            "invalid provider metadata"
        );
        match key.as_str() {
            "kind" => ensure!(value == "tasks#task", "unsupported Google resource kind"),
            "position" => {}
            "webViewLink" => {
                let url = reqwest::Url::parse(value).context("invalid task webViewLink")?;
                ensure!(
                    url.scheme() == "https"
                        && url.host_str() == Some("tasks.google.com")
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.port().is_none(),
                    "invalid task webViewLink"
                );
            }
            "selfLink" => {
                let url = reqwest::Url::parse(value).context("invalid task selfLink")?;
                let parts: Vec<_> = url.path().trim_start_matches('/').split('/').collect();
                ensure!(
                    url.scheme() == "https"
                        && matches!(
                            url.host_str(),
                            Some("www.googleapis.com" | "tasks.googleapis.com")
                        )
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.port().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none()
                        && parts.len() == 6
                        && parts[..3] == ["tasks", "v1", "lists"]
                        && !parts[3].is_empty()
                        && parts[4] == "tasks"
                        && !parts[5].is_empty(),
                    "invalid task selfLink"
                );
            }
            _ => anyhow::bail!("unsupported task field"),
        }
    }
    Ok(())
}

impl TaskHandoff {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_JSON_BYTES,
            "handoff size limit exceeded"
        );
        ensure!(
            self.schema_version == "1" && self.kind == "google-task-handoff",
            "unsupported handoff version or kind"
        );
        ensure!(
            opaque_id(&self.source_repo_id),
            "invalid opaque source repository ID"
        );
        let prefix = hive_core::google_identity::VERIFIED_NAMESPACE_PREFIX;
        ensure!(
            self.account_key.strip_prefix(prefix).is_some_and(hex),
            "verified account namespace required"
        );
        ensure!(
            self.capture_id
                == deterministic_capture_id(&self.account_key, &self.list_id, &self.provider_id)?
                && self.baseline.id == self.provider_id
                && self.receipt.stable_id == self.capture_id,
            "capture tuple or receipt identity mismatch"
        );
        ensure!(
            self.accepted_current_baseline,
            "explicit current baseline acceptance required"
        );
        validate_open_task(&self.baseline)?;
        ensure!(
            self.baseline.due.is_none(),
            "first-due baseline must have no due date"
        );
        ensure!(
            matches!(
                self.receipt.method.as_str(),
                "todos-add" | "snapshot-reconciliation"
            ) && hex(&self.receipt.sha256)
                && self
                    .receipt
                    .source_revision
                    .strip_prefix("sha256:")
                    .is_some_and(hex)
                && relative(&self.receipt.source_path)
                && !self.receipt.source_id.trim().is_empty()
                && self.receipt.source_id.len() <= 4096
                && !Path::new(&self.receipt.source_id).is_absolute()
                && !self.receipt.source_id.contains('\\')
                && !self.receipt.source_id.chars().any(char::is_control),
            "invalid receipt provenance"
        );
        Ok(())
    }
}

pub fn export_handoff(
    job: &CaptureJob,
    source_repo_id: &str,
    accept_current_baseline: bool,
) -> Result<TaskHandoff> {
    ensure!(
        job.state == DeliveryState::Delivered
            && !job.provider_changed
            && job.last_error.is_none()
            && job.attempts > 0,
        "capture is not delivered unchanged"
    );
    ensure!(
        job.title == job.observation.title,
        "capture observation title mismatch"
    );
    let receipt = job.receipt.as_ref().context("capture receipt required")?;
    let bytes = serde_json::to_vec(receipt)?;
    ensure!(bytes.len() <= MAX_JSON_BYTES, "receipt size limit exceeded");
    ensure!(receipt["schemaVersion"] == "1", "invalid receipt schema");
    let binding = job
        .binding
        .as_ref()
        .context("frozen destination required")?;
    binding.validate()?;
    ensure!(
        receipt["binding"] == serde_json::to_value(binding)?,
        "receipt destination mismatch"
    );
    let method = receipt["method"]
        .as_str()
        .context("receipt method required")?;
    let task = match method {
        "todos-add" => {
            let result = &receipt["result"];
            ensure!(
                result["schemaVersion"] == "1"
                    && result["alreadyCaptured"].is_boolean()
                    && result["projectionUpdated"].is_boolean()
                    && receipt.get("task").is_none(),
                "invalid add receipt"
            );
            ensure!(
                result["alreadyCaptured"] != false || result["task"]["todoSection"] == "Inbox",
                "new capture receipt is not in Inbox"
            );
            &result["task"]
        }
        "snapshot-reconciliation" => {
            ensure!(receipt.get("result").is_none(), "ambiguous receipt");
            &receipt["task"]
        }
        _ => anyhow::bail!("unsupported receipt method"),
    };
    let string = |key: &str| -> Result<String> {
        Ok(task[key]
            .as_str()
            .context("receipt task field required")?
            .into())
    };
    ensure!(
        task["id"] == format!("task:{}", job.capture_id)
            && task["stableId"] == job.capture_id
            && task["identityKind"] == "stable"
            && task["done"] == false
            && task["text"] == job.title.trim(),
        "receipt task identity, completion, or text mismatch"
    );
    let source_path = string("sourcePath")?;
    ensure!(relative(&source_path), "invalid receipt source path");
    if Path::new(&binding.notes_dir).join(&source_path) != Path::new(&binding.inbox_path) {
        // Match the inbound writer's metadata-only canonical checks for aliases and
        // parent components. Never normalize '..' across a possible symlink lexically.
        let root = Path::new(&binding.notes_dir)
            .canonicalize()
            .context("notes root alias unavailable")?;
        let source = root
            .join(&source_path)
            .canonicalize()
            .context("receipt source alias unavailable")?;
        let inbox = Path::new(&binding.inbox_path)
            .canonicalize()
            .context("Inbox alias unavailable")?;
        ensure!(
            source.starts_with(&root) && source == inbox,
            "receipt source differs from frozen Inbox"
        );
    }
    let handoff = TaskHandoff {
        schema_version: "1".into(),
        kind: "google-task-handoff".into(),
        source_repo_id: source_repo_id.into(),
        capture_id: job.capture_id.clone(),
        account_key: job.account_key.clone(),
        list_id: job.list_id.clone(),
        provider_id: job.provider_id.clone(),
        receipt: ReceiptEvidence {
            method: method.into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            stable_id: string("stableId")?,
            source_id: string("sourceId")?,
            source_path,
            source_revision: string("sourceRevision")?,
        },
        accepted_current_baseline: accept_current_baseline,
        baseline: job.observation.clone(),
    };
    handoff.validate()?;
    Ok(handoff)
}
