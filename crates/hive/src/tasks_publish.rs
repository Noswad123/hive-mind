use anyhow::Result;
use clap::Subcommand;
use hive_capture::handoff::TaskHandoff;
use hive_tasks::publisher::{self, Approval, Publisher};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum PublishCommand {
    /// Local binding only. Requires current source HEAD on main; no Google access.
    Init {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        mw_bin: PathBuf,
        #[arg(long)]
        connection: String,
        #[arg(long)]
        handoff: PathBuf,
    },
    /// Local inspection only; portable evidence is not live execution authority.
    Inspect {
        #[arg(long)]
        state: PathBuf,
    },
    /// Read exact committed source and authenticated Google task; durably freeze intent.
    Prepare {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        commit: String,
    },
    /// At most one due-only conditional PATCH; never retries or reschedules.
    Apply {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        expected_state_oid: String,
        #[arg(long)]
        expected_intent_sha256: String,
        #[arg(long)]
        acknowledge_live_write: bool,
    },
    /// Frozen-intent recovery, GET only when needed. Never reads source or PATCHes.
    Recover {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        operation_id: String,
    },
}
pub(super) fn command(command: PublishCommand) -> Result<Value> {
    match command {
        PublishCommand::Init {
            state,
            repo,
            mw_bin,
            connection,
            handoff,
        } => {
            let (handoff, _): (TaskHandoff, _) = super::read_json(&handoff)?;
            publisher::initialize(&state, &repo, &mw_bin, &connection, &handoff)
        }
        PublishCommand::Inspect { state } => Publisher::open(&state)?.inspect(),
        PublishCommand::Prepare { state, commit } => Publisher::open(&state)?.prepare(&commit),
        PublishCommand::Apply {
            state,
            operation_id,
            expected_state_oid,
            expected_intent_sha256,
            acknowledge_live_write,
        } => Publisher::open(&state)?.apply(Approval {
            operation_id: &operation_id,
            expected_state_oid: &expected_state_oid,
            expected_intent_sha256: &expected_intent_sha256,
            acknowledge_live_write,
        }),
        PublishCommand::Recover {
            state,
            operation_id,
        } => Publisher::open(&state)?.recover(&operation_id),
    }
}
