use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use hive_calendar::task_snapshot::read_committed_tasks;
use hive_capture::{
    handoff::{MAX_JSON_BYTES, TaskHandoff, export_handoff},
    store::CaptureStore,
};
use hive_tasks::policy::{TaskObservation, first_due_plan};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};

#[path = "tasks_fake.rs"]
mod fake;
#[path = "tasks_hosted.rs"]
mod hosted;
#[path = "tasks_observe.rs"]
mod observe;
#[cfg(feature = "tasks-conditional-probe")]
#[path = "tasks_probe.rs"]
mod probe;
#[path = "tasks_publish.rs"]
mod publish;

#[derive(Debug, Subcommand)]
pub enum TasksCommand {
    /// Read-only hosted admission foundation. No apply or publication authority.
    Hosted {
        #[command(subcommand)]
        command: hosted::HostedCommand,
    },
    /// Receipt-bound first-due publisher with durable local recovery.
    Publish {
        #[command(subcommand)]
        command: publish::PublishCommand,
    },
    /// Opt-in disposable-task diagnostic, not a production writer.
    #[cfg(feature = "tasks-conditional-probe")]
    ConditionalProbe {
        #[command(subcommand)]
        command: Box<probe::ProbeCommand>,
    },
    /// Explicit local synthetic executor. No Google writes, authentication or push.
    Fake {
        #[command(subcommand)]
        command: fake::FakeCommand,
    },
    /// Manage the separate Tasks write-profile connection. Status/logout are local only.
    Auth {
        #[command(subcommand)]
        command: crate::captures_auth::CaptureAuthCommand,
    },
    /// Authenticated exact read; emits portable unsigned evidence, not write authority.
    Observe {
        #[arg(long)]
        connection: String,
        #[arg(long)]
        handoff: PathBuf,
    },
    /// Export one delivered unchanged capture; no queue or source writes.
    Handoff {
        #[arg(long)]
        queue: PathBuf,
        #[arg(long)]
        capture_id: String,
        #[arg(long)]
        source_repo_id: String,
        #[arg(long)]
        accept_current_baseline: bool,
    },
    /// Advisory first-due plan from exact committed Markdown and operator-supplied evidence.
    DuePlan {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        mw_bin: PathBuf,
        #[arg(long)]
        source_repo_id: String,
        #[arg(long)]
        handoff: PathBuf,
        #[arg(long)]
        observation: PathBuf,
    },
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<(T, String)> {
    ensure!(path.is_absolute(), "JSON input path must be absolute");
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path).context("JSON input unavailable")?;
    ensure!(
        file.metadata()?.is_file(),
        "JSON input must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take((MAX_JSON_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_JSON_BYTES,
        "JSON input size limit exceeded"
    );
    Ok((
        serde_json::from_slice(&bytes).context("invalid JSON input")?,
        format!("{:x}", Sha256::digest(&bytes)),
    ))
}

// Blocking HTTP clients must be constructed, used, and dropped off runtime threads.
pub async fn dispatch(operation: impl FnOnce() -> Result<()> + Send + 'static) -> Result<()> {
    tokio::task::spawn_blocking(operation)
        .await
        .context("tasks worker failed")?
}

pub fn command(command: TasksCommand) -> Result<()> {
    let output = match command {
        TasksCommand::Hosted { command } => hosted::command(command)?,
        TasksCommand::Publish { command } => publish::command(command)?,
        #[cfg(feature = "tasks-conditional-probe")]
        TasksCommand::ConditionalProbe { command } => probe::command(*command)?,
        TasksCommand::Fake { command } => fake::command(command)?,
        TasksCommand::Auth { command } => {
            return crate::captures_auth::auth_command_for(
                command,
                hive_google_auth::AuthProfile::TasksWrite,
            );
        }
        TasksCommand::Observe {
            connection,
            handoff,
        } => {
            println!("{}", observe::command(&connection, &handoff)?);
            return Ok(());
        }
        TasksCommand::Handoff {
            queue,
            capture_id,
            source_repo_id,
            accept_current_baseline,
        } => {
            ensure!(queue.is_absolute(), "queue path must be absolute");
            ensure!(
                accept_current_baseline,
                "--accept-current-baseline is required"
            );
            let (job, _identity) = CaptureStore::inspect_selected(queue, &capture_id)?;
            serde_json::to_value(export_handoff(
                &job,
                &source_repo_id,
                accept_current_baseline,
            )?)?
        }
        TasksCommand::DuePlan {
            repo,
            commit,
            mw_bin,
            source_repo_id,
            handoff,
            observation,
        } => {
            let (handoff, handoff_digest): (TaskHandoff, _) = read_json(&handoff)?;
            let (observation, observation_digest): (TaskObservation, _) = read_json(&observation)?;
            let snapshot = read_committed_tasks(&repo, &commit, &mw_bin)?;
            let mut plan = first_due_plan(&snapshot, &source_repo_id, &handoff, &observation);
            plan["handoffSha256"] = json!(handoff_digest);
            plan["observationSha256"] = json!(observation_digest);
            plan
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_propagates_operation_and_join_failures() {
        let error = dispatch(|| anyhow::bail!("fixture operation failed"))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "fixture operation failed");

        let error = dispatch(|| panic!("fixture worker panic"))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "tasks worker failed");
        assert!(
            error
                .downcast_ref::<tokio::task::JoinError>()
                .unwrap()
                .is_panic()
        );
    }
}
