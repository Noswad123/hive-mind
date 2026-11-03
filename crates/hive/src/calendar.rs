//! Calendar planning, local state-ref updates, and separate OAuth management; no publisher.
use std::{io::Write, path::PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, clap::Subcommand)]
pub enum CalendarCommand {
    /// Manage a separate Calendar Events OAuth connection; never publishes events.
    Auth {
        #[command(subcommand)]
        command: crate::captures_auth::CaptureAuthCommand,
    },
    /// Preview open task due dates from one exact committed Markdown snapshot.
    ///
    /// No working-tree edits, credentials, databases, Calendar calls, or ledger writes.
    /// Output contains private task titles and paths; do not publish it in CI logs.
    Preview {
        #[command(flatten)]
        source: SourceArgs,
    },
    /// Compare committed source with explicit publication evidence; advice only.
    Plan(Box<PlanArgs>),
    /// Propose enrollment of one selected task, assigning a ledger UUID but no task marker.
    ///
    /// JSON candidate only: no notes, ledger, Git-ref, credential, or provider writes.
    Enroll(Box<EnrollArgs>),
    /// Propose new source evidence under an existing ledger identity.
    ///
    /// Explicit mapping intent, not semantic identity proof. Pending and retired targets refuse.
    Reack(Box<ReackArgs>),
    /// Validate approved candidate bytes and preview a deterministic local state commit.
    ///
    /// Read-only: no objects, refs, working tree, provider, or credential writes.
    VerifyCandidate(Box<ApplyArgs>),
    /// Persist approved candidate bytes to the dedicated local bare-repository state ref.
    ///
    /// Creates Git objects and performs one atomic ref update. Never pushes or calls Google.
    Apply(Box<ApplyArgs>),
    /// Propose a frozen create/update intent before any future provider dispatch.
    ///
    /// Candidate only. A prepared intent is not proof that retrying or dispatching is safe.
    Prepare(Box<PrepareArgs>),
    /// Propose prepared -> uncertain for one frozen intent, without reading current notes.
    MarkUncertain(Box<FrozenArgs>),
    /// Simulate an acknowledgement from an explicitly unverified operator observation.
    ///
    /// Hypothetical result only. Ordinary verify/apply refuses this output.
    SimulateAck(Box<SimulationArgs>),
}

#[derive(Debug, clap::Args)]
pub struct SourceArgs {
    /// Absolute local repository path; no fetch, checkout, commit, or push.
    #[arg(long)]
    repo: PathBuf,
    /// Full lowercase commit object ID, not HEAD, a ref, or an abbreviation.
    #[arg(long)]
    commit: String,
    /// Absolute path to a trusted standalone MW supporting `todos snapshot --json`.
    #[arg(long)]
    mw_bin: PathBuf,
}

impl SourceArgs {
    fn into_options(self) -> hive_calendar::PreviewOptions {
        hive_calendar::PreviewOptions {
            repository: self.repo,
            commit: self.commit,
            mw_binary: self.mw_bin,
        }
    }
}

#[derive(Debug, clap::Args)]
struct BindingArgs {
    /// Stable, independently supplied source repository namespace.
    #[arg(long)]
    source_repo_id: String,
    /// Claimed google-sub-v1 namespace, not credentials or an account alias.
    #[arg(long)]
    account_id: String,
    /// Concrete calendar ID, never primary/default.
    #[arg(long)]
    calendar_id: String,
}

impl BindingArgs {
    fn into_binding(self) -> hive_calendar::Binding {
        hive_calendar::Binding {
            provider: "google_calendar".into(),
            account_id: self.account_id,
            calendar_id: self.calendar_id,
            source_repo_id: self.source_repo_id,
        }
    }
}

#[derive(Debug, clap::Args)]
pub struct PlanArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[command(flatten)]
    binding: BindingArgs,
    /// Absolute local repository containing the committed JSON ledger.
    #[arg(
        long,
        required_unless_present = "bootstrap_preview",
        requires_all = ["ledger_commit", "ledger_path"]
    )]
    ledger_repo: Option<PathBuf>,
    #[arg(long, requires = "ledger_repo")]
    ledger_commit: Option<String>,
    /// Exact repository-relative JSON path, never a working-tree file.
    #[arg(long, requires = "ledger_repo")]
    ledger_path: Option<String>,
    /// Observation-only bootstrap, not authoritative empty publication state.
    #[arg(long, conflicts_with_all = ["ledger_repo", "ledger_commit", "ledger_path"])]
    bootstrap_preview: bool,
}

#[derive(Debug, clap::Args)]
struct LedgerArgs {
    /// Absolute local publication-state repository.
    #[arg(long)]
    ledger_repo: PathBuf,
    /// Exact ledger base commit. This does not prove the authoritative HEAD.
    #[arg(long)]
    ledger_commit: String,
    /// Repository-relative JSON path at that commit.
    #[arg(long)]
    ledger_path: String,
}

impl LedgerArgs {
    fn into_location(self) -> hive_calendar::LedgerLocation {
        hive_calendar::LedgerLocation {
            repository: self.ledger_repo,
            commit: self.ledger_commit,
            path: self.ledger_path,
        }
    }
}

#[derive(Debug, clap::Args)]
struct CandidateInputs {
    #[command(flatten)]
    source: SourceArgs,
    #[command(flatten)]
    binding: BindingArgs,
    #[command(flatten)]
    ledger: LedgerArgs,
    /// Exact relative Markdown path of the selected current task.
    #[arg(long)]
    source_path: String,
    /// One-based checkbox line in the selected source commit.
    #[arg(long)]
    source_line: usize,
}

impl CandidateInputs {
    fn into_parts(
        self,
    ) -> (
        hive_calendar::PreviewOptions,
        hive_calendar::Binding,
        hive_calendar::LedgerLocation,
        hive_calendar::TaskSelection,
    ) {
        (
            self.source.into_options(),
            self.binding.into_binding(),
            self.ledger.into_location(),
            hive_calendar::TaskSelection {
                path: self.source_path,
                line: self.source_line,
            },
        )
    }
}

#[derive(Debug, clap::Args)]
pub struct EnrollArgs {
    #[command(flatten)]
    input: CandidateInputs,
    /// Propose initialization only if the JSON path is absent at the explicit base commit.
    ///
    /// This does not prove that an authoritative ledger or remote events do not exist.
    #[arg(long)]
    initialize_ledger: bool,
}

#[derive(Debug, clap::Args)]
pub struct ReackArgs {
    #[command(flatten)]
    input: CandidateInputs,
    /// Optional advanced selector; otherwise choose the old acknowledged path and line.
    #[arg(long, conflicts_with_all = ["old_source_path", "old_source_line"])]
    logical_id: Option<String>,
    #[arg(
        long,
        required_unless_present = "logical_id",
        requires = "old_source_line"
    )]
    old_source_path: Option<String>,
    #[arg(long, requires = "old_source_path")]
    old_source_line: Option<usize>,
    /// Approve newly parsed evidence for this entry if the parser artifact changed.
    #[arg(long)]
    accept_parser_change: bool,
}

#[derive(Debug, clap::Args)]
pub struct ApplyArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[command(flatten)]
    binding: BindingArgs,
    /// Absolute dedicated bare state repository, with no registered linked worktrees.
    #[arg(long)]
    ledger_repo: PathBuf,
    /// Independently selected repository-relative JSON ledger path.
    #[arg(long)]
    ledger_path: String,
    /// Only refs/heads/hive-calendar-state is supported. No branch adoption.
    #[arg(long)]
    state_ref: String,
    /// Absolute regular file containing the exact candidate being approved.
    #[arg(long)]
    candidate: PathBuf,
    /// Independently approved SHA-256 of the exact candidate bytes (64 lowercase hex).
    #[arg(long)]
    candidate_sha256: String,
    /// Initialization only: require the dedicated ref to be absent, not merely its ledger path.
    #[arg(long)]
    create_ref: bool,
    /// Independently approve a selected reack's parser-artifact migration again at apply time.
    #[arg(long)]
    accept_parser_change: bool,
}

impl ApplyArgs {
    fn into_options(self) -> hive_calendar::ApplyOptions {
        hive_calendar::ApplyOptions {
            source: self.source.into_options(),
            binding: self.binding.into_binding(),
            ledger_repository: self.ledger_repo,
            ledger_path: self.ledger_path,
            state_ref: self.state_ref,
            candidate_path: self.candidate,
            expected_candidate_sha256: self.candidate_sha256,
            create_ref: self.create_ref,
            accept_parser_change: self.accept_parser_change,
        }
    }
}

#[derive(Debug, clap::Args)]
struct EntryArgs {
    #[arg(long, conflicts_with_all = ["old_source_path", "old_source_line"])]
    logical_id: Option<String>,
    #[arg(
        long,
        required_unless_present = "logical_id",
        requires = "old_source_line"
    )]
    old_source_path: Option<String>,
    #[arg(long, requires = "old_source_path")]
    old_source_line: Option<usize>,
}

impl EntryArgs {
    fn into_selection(self) -> Result<hive_calendar::EntrySelection> {
        match (self.logical_id, self.old_source_path, self.old_source_line) {
            (Some(id), None, None) => Ok(hive_calendar::EntrySelection::LogicalId(id)),
            (None, Some(path), Some(line)) => {
                Ok(hive_calendar::EntrySelection::AcknowledgedLocation { path, line })
            }
            _ => bail!("select a logical ID or an old acknowledged path and line"),
        }
    }
}

#[derive(Debug, clap::Args)]
pub struct PrepareArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[command(flatten)]
    binding: BindingArgs,
    #[command(flatten)]
    ledger: LedgerArgs,
    #[command(flatten)]
    entry: EntryArgs,
}

#[derive(Debug, clap::Args)]
pub struct FrozenArgs {
    #[command(flatten)]
    binding: BindingArgs,
    #[command(flatten)]
    ledger: LedgerArgs,
    #[command(flatten)]
    entry: EntryArgs,
    /// Exact persisted pending operation ID; not a new operation to allocate.
    #[arg(long)]
    operation_id: String,
}

#[derive(Debug, clap::Args)]
pub struct SimulationArgs {
    #[command(flatten)]
    frozen: FrozenArgs,
    /// Absolute regular observation JSON, captured once and never treated as authenticated.
    #[arg(long)]
    observation: PathBuf,
    /// Independently selected SHA-256 of the exact observation bytes (64 lowercase hex).
    #[arg(long)]
    observation_sha256: String,
    /// Acknowledge that these are operator assertions, not authenticated provider evidence.
    #[arg(long, required = true)]
    assert_operator_observation: bool,
}

pub fn command(command: CalendarCommand) -> Result<()> {
    command_with_auth(command, crate::captures_auth::auth_command_for)
}

fn command_with_auth(
    command: CalendarCommand,
    auth: impl FnOnce(
        crate::captures_auth::CaptureAuthCommand,
        hive_google_auth::AuthProfile,
    ) -> Result<()>,
) -> Result<()> {
    match command {
        CalendarCommand::Auth { command } => {
            auth(command, hive_google_auth::AuthProfile::CalendarEvents)
        }
        CalendarCommand::Preview { source } => {
            write_output(&hive_calendar::preview(&source.into_options())?)
        }
        CalendarCommand::Plan(args) => {
            let PlanArgs {
                source,
                binding,
                ledger_repo,
                ledger_commit,
                ledger_path,
                bootstrap_preview,
            } = *args;
            let ledger = match (bootstrap_preview, ledger_repo, ledger_commit, ledger_path) {
                (true, None, None, None) => hive_calendar::LedgerInput::BootstrapPreview,
                (false, Some(repository), Some(commit), Some(path)) => {
                    hive_calendar::LedgerInput::Committed {
                        repository,
                        commit,
                        path,
                    }
                }
                _ => bail!("choose a committed ledger or explicit bootstrap preview"),
            };
            write_output(&hive_calendar::plan(&hive_calendar::PlanOptions {
                source: source.into_options(),
                binding: binding.into_binding(),
                ledger,
            })?)
        }
        CalendarCommand::Enroll(args) => {
            let EnrollArgs {
                input,
                initialize_ledger,
            } = *args;
            let (source, binding, ledger, selection) = input.into_parts();
            write_candidate(&hive_calendar::enroll(&hive_calendar::EnrollmentOptions {
                source,
                binding,
                ledger,
                selection,
                initialize_ledger,
            })?)
        }
        CalendarCommand::Reack(args) => {
            let ReackArgs {
                input,
                logical_id,
                old_source_path,
                old_source_line,
                accept_parser_change,
            } = *args;
            let entry = match (logical_id, old_source_path, old_source_line) {
                (Some(id), None, None) => hive_calendar::EntrySelection::LogicalId(id),
                (None, Some(path), Some(line)) => {
                    hive_calendar::EntrySelection::AcknowledgedLocation { path, line }
                }
                _ => bail!("select a logical ID or an old acknowledged path and line"),
            };
            let (source, binding, ledger, selection) = input.into_parts();
            write_candidate(&hive_calendar::reack(&hive_calendar::ReackOptions {
                source,
                binding,
                ledger,
                entry,
                selection,
                accept_parser_change,
            })?)
        }
        CalendarCommand::VerifyCandidate(args) => {
            write_output(&hive_calendar::verify_candidate(&args.into_options())?)
        }
        CalendarCommand::Apply(args) => {
            let receipt = hive_calendar::apply_candidate(&args.into_options())?;
            // Output failure after CAS must not be mistaken for proof of no write.
            write_output(&receipt).map_err(|_| {
                anyhow::anyhow!("local ledger persistence may have completed; retry the exact approved candidate")
            })
        }
        CalendarCommand::Prepare(args) => {
            let PrepareArgs {
                source,
                binding,
                ledger,
                entry,
            } = *args;
            write_candidate(&hive_calendar::prepare_operation(
                &hive_calendar::PrepareOptions {
                    source: source.into_options(),
                    binding: binding.into_binding(),
                    ledger: ledger.into_location(),
                    entry: entry.into_selection()?,
                },
            )?)
        }
        CalendarCommand::MarkUncertain(args) => {
            let FrozenArgs {
                binding,
                ledger,
                entry,
                operation_id,
            } = *args;
            write_candidate(&hive_calendar::mark_uncertain(
                &hive_calendar::UncertainOptions {
                    binding: binding.into_binding(),
                    ledger: ledger.into_location(),
                    entry: entry.into_selection()?,
                    operation_id,
                },
            )?)
        }
        CalendarCommand::SimulateAck(args) => {
            let SimulationArgs {
                frozen,
                observation,
                observation_sha256,
                assert_operator_observation,
            } = *args;
            if !assert_operator_observation {
                bail!("simulation requires explicit operator-assertion acknowledgement");
            }
            write_candidate(&hive_calendar::simulate_acknowledgement(
                &hive_calendar::SimulationOptions {
                    binding: frozen.binding.into_binding(),
                    ledger: frozen.ledger.into_location(),
                    entry: frozen.entry.into_selection()?,
                    operation_id: frozen.operation_id,
                    observation_path: observation,
                    expected_observation_sha256: observation_sha256,
                },
            )?)
        }
    }
}

fn write_output(value: &impl serde::Serialize) -> Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut output, value).context("encode calendar response")?;
    writeln!(output).context("write calendar response")
}

fn write_candidate(candidate: &impl serde::Serialize) -> Result<()> {
    // Complete validation/serialization before exposing any proposed ledger bytes.
    let mut encoded = serde_json::to_vec_pretty(candidate).context("encode ledger candidate")?;
    encoded.push(b'\n');
    std::io::stdout()
        .lock()
        .write_all(&encoded)
        .context("write ledger candidate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captures_auth::CaptureAuthCommand;
    use clap::Parser;

    #[test]
    fn calendar_auth_routes_only_to_calendar_profile() {
        for operation in ["login", "status", "logout"] {
            let mut args = vec![
                "hive",
                "calendar",
                "auth",
                operation,
                "--connection",
                "personal",
            ];
            if operation == "login" {
                args.extend(["--client-json", "/fixture/client.json"]);
            }
            let cli = crate::Cli::try_parse_from(args).unwrap();
            let Some(crate::Commands::Calendar { command }) = cli.command else {
                panic!("wrong command")
            };
            command_with_auth(command, |command, profile| {
                assert_eq!(profile, hive_google_auth::AuthProfile::CalendarEvents);
                let connection = match command {
                    CaptureAuthCommand::Login {
                        connection,
                        client_json,
                    } => {
                        assert_eq!(operation, "login");
                        assert_eq!(client_json, PathBuf::from("/fixture/client.json"));
                        connection
                    }
                    CaptureAuthCommand::Status { connection } => {
                        assert_eq!(operation, "status");
                        connection
                    }
                    CaptureAuthCommand::Logout { connection } => {
                        assert_eq!(operation, "logout");
                        connection
                    }
                };
                assert_eq!(connection, "personal");
                Ok(())
            })
            .unwrap();
        }
        assert!(
            crate::Cli::try_parse_from([
                "hive",
                "calendar",
                "auth",
                "login",
                "--connection",
                "personal"
            ])
            .is_err()
        );
        assert!(
            crate::Cli::try_parse_from([
                "hive",
                "calendar",
                "auth",
                "status",
                "--connection",
                "personal",
                "--access-token-env",
                "HIVE_GOOGLE_TOKEN"
            ])
            .is_err()
        );
    }
}
