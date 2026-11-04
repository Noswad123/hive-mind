use anyhow::Result;
use clap::Subcommand;
use hive_tasks::{FakeSession, SourceOptions};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum FakeCommand {
    /// Create a new dedicated bare state repository and separate synthetic world.
    Init {
        #[arg(long)]
        state_repo: PathBuf,
        #[arg(long)]
        world: PathBuf,
        #[arg(long)]
        fixture: PathBuf,
    },
    /// Collect exact committed source, persist intent, then attempt one fake due-only patch.
    ExecuteFirstDue {
        #[arg(long)]
        state_repo: PathBuf,
        #[arg(long)]
        world: PathBuf,
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
    },
    /// Observe only using frozen intent. Never replay a patch.
    Recover {
        #[arg(long)]
        state_repo: PathBuf,
        #[arg(long)]
        world: PathBuf,
        #[arg(long)]
        operation_id: String,
    },
    /// Read retained fake operations without creating lock files.
    Inspect {
        #[arg(long)]
        state_repo: PathBuf,
    },
}

pub fn command(command: FakeCommand) -> Result<Value> {
    match command {
        FakeCommand::Init {
            state_repo,
            world,
            fixture,
        } => hive_tasks::initialize(&state_repo, &world, &fixture),
        FakeCommand::ExecuteFirstDue {
            state_repo,
            world,
            repo,
            commit,
            mw_bin,
            source_repo_id,
            handoff,
        } => {
            let (handoff, _): (hive_capture::handoff::TaskHandoff, _) = super::read_json(&handoff)?;
            FakeSession::open(&state_repo, &world)?.execute_first_due(
                SourceOptions {
                    repo: &repo,
                    commit: &commit,
                    mw_bin: &mw_bin,
                    source_repo_id: &source_repo_id,
                },
                &handoff,
            )
        }
        FakeCommand::Recover {
            state_repo,
            world,
            operation_id,
        } => FakeSession::open(&state_repo, &world)?.recover(&operation_id),
        FakeCommand::Inspect { state_repo } => hive_tasks::inspect(&state_repo),
    }
}
