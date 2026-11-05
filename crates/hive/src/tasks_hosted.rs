use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use hive_tasks::hosted::{self, Inputs, PushClaims};
use serde_json::Value;
use std::path::PathBuf;
use zeroize::Zeroizing;

#[derive(Debug, Args)]
pub struct HostedArgs {
    #[arg(long)]
    binding: PathBuf,
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    mw_bin: PathBuf,
    #[arg(long)]
    commit: String,
}
#[derive(Debug, Subcommand)]
pub enum HostedCommand {
    /// Local main-push claims/source admission and credential structure only. No network.
    Check(HostedArgs),
    /// Read-only fresh authenticated exact task read; redacted unsigned summary only.
    Plan(HostedArgs),
    /// Read-only authenticated GitHub private identity and exact current main check.
    VerifyMain(HostedArgs),
}
pub(super) fn command(command: HostedCommand) -> Result<Value> {
    let (args, mode) = match command {
        HostedCommand::Check(args) => (args, 0),
        HostedCommand::Plan(args) => (args, 1),
        HostedCommand::VerifyMain(args) => (args, 2),
    };
    let claims = PushClaims::from_environment(|key| std::env::var(key).ok())?;
    let inputs = Inputs {
        binding: &args.binding,
        repo: &args.repo,
        mw_bin: &args.mw_bin,
        commit: &args.commit,
    };
    let secret = || {
        // Only this local copy is zeroized. The runner's original environment persists.
        Ok(Zeroizing::new(
            std::env::var("HIVE_HOSTED_TASKS_WRITE_RECORD")
                .map_err(|_| anyhow!("credential_unavailable"))?,
        ))
    };
    if mode == 2 {
        hosted::verify_main(inputs, claims, || {
            Ok(Zeroizing::new(
                std::env::var("GITHUB_TOKEN").map_err(|_| anyhow!("credential_unavailable"))?,
            ))
        })
    } else if mode == 1 {
        hosted::plan(inputs, claims, secret)
    } else {
        hosted::check(inputs, claims, secret)
    }
}
