//! Bounded scheduling and CLI glue. No credentials or provider text cross child RPCs.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use hive_background::{
    CycleStatus, DeferralReason, DeliveryLease, EditorEntry, WorkerConfig, config,
    editor::{self, EditorDecision},
    launchd, now_ms, process,
    state::{self, FileLock, Paths},
};
use hive_capture::{
    BootstrapPolicy, CaptureJob, DeliveryReport, DeliveryState, DestinationBinding,
    mindweaver::MindWeaver, store::CaptureStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::captures_auth::{self, BackgroundCredentials};

const SELECTORS: &[&str] = &[
    "MW_CONFIG",
    "NOTES_DIR",
    "INBOX_PATH",
    "MW_INBOX_PATH",
    "DASHBOARD_PATH",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
];
const WORKER_FD: &str = "HIVE_BACKGROUND_WORKER_FD";
const DELIVERY_FD: &str = "HIVE_BACKGROUND_DELIVERY_FD";
const ADMISSION_FD: &str = "HIVE_BACKGROUND_ADMISSION_FD";

#[derive(Debug, clap::Subcommand)]
pub enum BackgroundCommand {
    /// Print fixed OS-user state paths without creating them.
    Paths,
    /// Explicitly bind one existing queue and one verified Google connection.
    Configure {
        #[arg(long)]
        connection: String,
        #[arg(long)]
        queue_path: PathBuf,
        #[arg(long)]
        list_id: String,
        #[arg(long)]
        mw_bin: PathBuf,
        #[arg(long)]
        nvim_bin: PathBuf,
        #[arg(long)]
        hive_bin: Option<PathBuf>,
        #[arg(long)]
        working_directory: Option<PathBuf>,
        #[arg(long, default_value_t = 60)]
        interval_seconds: u64,
        #[arg(long, default_value_t = 45)]
        cycle_timeout_seconds: u64,
        #[arg(long)]
        replace: bool,
    },
    /// Read-only local readiness and queue counts; never requests editor delivery.
    Check,
    /// Probe registered editor readiness without requesting any delivery or source write.
    EditorCheck,
    /// Run one bounded, supervised poll and local delivery cycle.
    RunOnce,
    /// Read the last sanitized cycle status without creating state.
    Status,
    Install,
    Start,
    Stop,
    Uninstall,
    EditorRegister {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        session_id: String,
    },
    /// Resolve the configured editor destination in a bounded, credential-free child.
    EditorContext {
        #[arg(long)]
        config_digest: String,
    },
    #[command(hide = true)]
    ContextCycle {
        #[arg(long)]
        config_digest: String,
        #[arg(long)]
        deadline_ms: u64,
    },
    EditorUnregister {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long)]
        session_id: String,
    },
    LeaseStatus {
        #[arg(long)]
        cycle_id: String,
    },
    #[command(hide = true)]
    Cycle {
        #[arg(long)]
        cycle_id: String,
        #[arg(long)]
        deadline_ms: u64,
    },
    #[command(hide = true)]
    DeliverOwned {
        #[arg(long)]
        cycle_id: String,
        #[arg(long)]
        config_digest: String,
    },
    #[command(hide = true)]
    DeliverCycle {
        #[arg(long)]
        cycle_id: String,
        #[arg(long)]
        config_digest: String,
    },
}

fn output(value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| anyhow::anyhow!("background response encoding failed"))?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes)?;
    writeln!(stdout)?;
    Ok(())
}

pub fn command(cmd: BackgroundCommand) -> Result<()> {
    // All errors exposed by this command are static, including serde and native failures.
    dispatch(cmd).map_err(|_| anyhow::anyhow!("background command failed; inspect sanitized background status and explicit configuration"))
}

fn dispatch(cmd: BackgroundCommand) -> Result<()> {
    let paths = Paths::production()?;
    match cmd {
        BackgroundCommand::Paths => output(&paths_reply(&paths)),
        BackgroundCommand::Configure {
            connection,
            queue_path,
            list_id,
            mw_bin,
            nvim_bin,
            hive_bin,
            working_directory,
            interval_seconds,
            cycle_timeout_seconds,
            replace,
        } => {
            hive_google_auth::validate_connection(&connection)?;
            // The queue is identified before native credential storage is constructed.
            let jobs = CaptureStore::inspect(&queue_path)?;
            let working_directory =
                std::fs::canonicalize(working_directory.unwrap_or(std::env::current_dir()?))?;
            let mw_binary = std::fs::canonicalize(mw_bin)?;
            let hive_binary = std::fs::canonicalize(hive_bin.unwrap_or(std::env::current_exe()?))?;
            let nvim_binary = std::fs::canonicalize(nvim_bin)?;
            let mw_environment = selectors(std::env::vars_os())?;
            let destination = prepare_binding(
                &mw_binary,
                &hive_binary,
                &nvim_binary,
                &working_directory,
                || {
                    discover_binding(
                        &mw_binary,
                        &working_directory,
                        &source_env(&mw_environment)?,
                    )
                },
            )?;
            let store = hive_google_auth::storage::MacKeychainStore::new()?;
            let transport = hive_google_auth::google::GoogleOAuth::new()?;
            let status = hive_google_auth::provider::Authenticator::new(&store, &transport)
                .status(&connection)?;
            ensure!(status.credential_stored, "saved connection required");
            let cfg = WorkerConfig {
                schema_version: "1".into(),
                connection,
                account_namespace: status
                    .account_namespace
                    .context("verified connection required")?,
                queue_path,
                list_id,
                mw_binary,
                hive_binary,
                nvim_binary,
                working_directory,
                destination,
                mw_environment,
                interval_seconds,
                cycle_timeout_seconds,
                delivery_limit: 1,
            };
            scope(&cfg, &jobs)?;
            config::validate(&cfg)?;
            // Do not replace the routing of a running coordinator, or strand a differently
            // generated plist whose ownership can no longer be established with this config.
            if paths.config.try_exists()? {
                let old = config::load(&paths)?;
                let agents = agents_dir()?;
                if launchd::managed_plist_path(&agents).try_exists()? {
                    ensure!(
                        old == cfg,
                        "uninstall the owned agent before changing its configuration"
                    );
                }
            }
            paths.ensure()?;
            let _worker = FileLock::acquire(&paths.worker_lock, false, Duration::ZERO)?;
            scope(&cfg, &CaptureStore::inspect(&cfg.queue_path)?)?;
            let digest = config::save(&paths, &cfg, replace)?;
            output(&json!({"schemaVersion":"1", "configured":true, "configDigest":digest}))
        }
        BackgroundCommand::Check => output(&check(&paths)?),
        BackgroundCommand::EditorCheck => {
            let cfg = config::load(&paths)?;
            let admission = FileLock::acquire(&paths.admission_lock, true, Duration::ZERO);
            let readiness = if let Ok(_guard) = admission {
                editor::check_readiness(&paths, &cfg)?
            } else {
                editor::EditorReadiness::Deferred(DeferralReason::AdmissionBusy)
            };
            let value = match readiness {
                editor::EditorReadiness::NoEditor => {
                    json!({"schemaVersion":"1","ready":true,"noEditor":true,"deferred":null})
                }
                editor::EditorReadiness::Ready => {
                    json!({"schemaVersion":"1","ready":true,"noEditor":false,"deferred":null})
                }
                editor::EditorReadiness::Deferred(reason) => {
                    json!({"schemaVersion":"1","ready":false,"noEditor":false,"deferred":reason})
                }
            };
            output(&value)
        }
        BackgroundCommand::Status => {
            let cfg = config::load(&paths).ok();
            let mut status = service_status(cfg.as_ref(), &agents_dir()?, launchctl);
            status["schemaVersion"] = "1".into();
            status["configured"] = cfg.is_some().into();
            status["lastCycle"] = serde_json::to_value(previous(&paths)?)?;
            output(&status)
        }
        BackgroundCommand::RunOnce => output(&run_once(&paths)?),
        BackgroundCommand::Cycle {
            cycle_id,
            deadline_ms,
        } => output(&cycle(&paths, &cycle_id, deadline_ms)?),
        BackgroundCommand::DeliverOwned {
            cycle_id,
            config_digest,
        } => output(&deliver_owned(&paths, &cycle_id, &config_digest)?),
        BackgroundCommand::DeliverCycle {
            cycle_id,
            config_digest,
        } => {
            // This is the in-group writer, not the supervisor. Only its caller may
            // attest writerExited after reaping the complete group.
            output(&deliver_cycle(&paths, &cycle_id, &config_digest)?)
        }
        BackgroundCommand::LeaseStatus { cycle_id } => output(&lease_status(&paths, &cycle_id)?),
        BackgroundCommand::EditorRegister {
            pid,
            socket,
            session_id,
        } => {
            let cfg = config::load(&paths)?;
            let socket = physical_leaf(&socket)?;
            output(&editor::register(&paths, &cfg, pid, &socket, &session_id)?)
        }
        BackgroundCommand::EditorContext { config_digest } => {
            output(&editor_context(&paths, &config_digest)?)
        }
        BackgroundCommand::ContextCycle {
            config_digest,
            deadline_ms,
        } => output(&context_cycle(&paths, &config_digest, deadline_ms)?),
        BackgroundCommand::EditorUnregister {
            pid,
            socket: _,
            session_id,
        } => {
            editor::unregister(&paths, pid, &session_id)?;
            output(&json!({"schemaVersion":"1", "unregistered":true}))
        }
        action @ (BackgroundCommand::Install
        | BackgroundCommand::Start
        | BackgroundCommand::Stop
        | BackgroundCommand::Uninstall) => {
            let cfg = config::load(&paths)?;
            output(&service(&paths, &cfg, &agents_dir()?, action, launchctl)?)
        }
    }
}

fn prepare_binding(
    mw: &Path,
    hive: &Path,
    nvim: &Path,
    cwd: &Path,
    discover: impl FnOnce() -> Result<DestinationBinding>,
) -> Result<DestinationBinding> {
    config::validate_programs(mw, hive, nvim, cwd)?;
    discover()
}

fn paths_reply(paths: &Paths) -> Value {
    json!({"schemaVersion":"1", "configPath":paths.config, "registryPath":paths.registry,
        "statusPath":paths.status, "leasesPath":paths.leases, "workerLockPath":paths.worker_lock,
        "admissionLockPath":paths.admission_lock, "deliveryLockPath":paths.delivery_lock})
}

fn selectors(
    values: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for (key, value) in values {
        let Some(key) = key.to_str().filter(|key| SELECTORS.contains(key)) else {
            continue;
        };
        let value = value.to_str().context("source selector must be UTF-8")?;
        ensure!(
            Path::new(value).is_absolute() && !value.chars().any(char::is_control),
            "invalid source selector"
        );
        result.insert(key.into(), value.into());
    }
    Ok(result)
}

fn source_env(selectors: &BTreeMap<String, String>) -> Result<BTreeMap<OsString, OsString>> {
    let mut env = BTreeMap::from([
        (
            "HOME".into(),
            hive_google_auth::storage::system_user_home()?.into_os_string(),
        ),
        ("PATH".into(), "/usr/bin:/bin:/opt/homebrew/bin".into()),
    ]);
    env.extend(
        selectors
            .iter()
            .map(|(key, value)| (key.into(), value.into())),
    );
    Ok(env)
}

fn physical_leaf(path: &Path) -> Result<PathBuf> {
    Ok(
        std::fs::canonicalize(path.parent().context("path has no parent")?)?
            .join(path.file_name().context("path has no filename")?),
    )
}

fn binding_from_json(bytes: &[u8]) -> Result<DestinationBinding> {
    let value: Value = serde_json::from_slice(bytes)?;
    ensure!(
        value["schemaVersion"] == "1",
        "unsupported source configuration"
    );
    let field = |name| -> Result<String> {
        Ok(value[name]
            .as_str()
            .context("missing source binding")?
            .into())
    };
    let binding = DestinationBinding {
        notes_dir: field("notesDir")?,
        inbox_path: field("inboxPath")?,
        dashboard_path: field("dashboardPath")?,
    };
    binding.validate()?;
    Ok(binding)
}

fn discover_binding(
    binary: &Path,
    cwd: &Path,
    env: &BTreeMap<OsString, OsString>,
) -> Result<DestinationBinding> {
    let mut cmd = Command::new(binary);
    cmd.args(["config", "json"])
        .current_dir(cwd)
        .env_clear()
        .envs(env);
    let result = process::run_supervised(&mut cmd, Duration::from_secs(5), &[])?;
    ensure!(successful(&result), "source configuration unavailable");
    binding_from_json(&result.stdout)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContextResponse {
    schema_version: String,
    context: DestinationBinding,
}

fn editor_context(paths: &Paths, digest: &str) -> Result<ContextResponse> {
    editor_context_with(paths, digest, |cmd, timeout| {
        process::run_supervised(cmd, timeout, &[])
    })
}

fn editor_context_with(
    paths: &Paths,
    digest: &str,
    run: impl FnOnce(&mut Command, Duration) -> Result<process::ProcessResult>,
) -> Result<ContextResponse> {
    let cfg = config::load(paths)?;
    ensure!(
        config::digest(&cfg)? == digest,
        "editor context digest mismatch"
    );
    let deadline = now_ms() + 4500;
    let mut cmd = Command::new(&cfg.hive_binary);
    cmd.args([
        "captures",
        "background",
        "context-cycle",
        "--config-digest",
        digest,
        "--deadline-ms",
        &deadline.to_string(),
    ])
    .current_dir(&cfg.working_directory)
    .env_clear()
    .envs(config::source_environment(&cfg)?);
    let result = run(&mut cmd, Duration::from_secs(5))?;
    ensure!(successful(&result), "editor context unavailable");
    let response: ContextResponse = serde_json::from_slice(&result.stdout)?;
    ensure!(
        response.schema_version == "1",
        "unsupported editor context response"
    );
    same_binding(&response.context, &cfg.destination)?;
    Ok(response)
}

fn context_cycle(paths: &Paths, digest: &str, deadline: u64) -> Result<ContextResponse> {
    let cfg = config::load(paths)?;
    ensure!(
        config::digest(&cfg)? == digest && deadline > now_ms() && deadline <= now_ms() + 5000,
        "editor context admission mismatch"
    );
    own_group()?;
    let _watchdog =
        process::install_group_watchdog(deadline, unsafe { libc::getppid() } as u32, None)?;
    source_context(&cfg)?;
    let context = MindWeaver::new(&cfg.mw_binary)?.binding()?;
    same_binding(&context, &cfg.destination)?;
    Ok(ContextResponse {
        schema_version: "1".into(),
        context,
    })
}

fn same_binding(actual: &DestinationBinding, expected: &DestinationBinding) -> Result<()> {
    actual.validate()?;
    expected.validate()?;
    ensure!(
        std::fs::canonicalize(&actual.notes_dir)? == std::fs::canonicalize(&expected.notes_dir)?,
        "source root changed"
    );
    for (a, b) in [
        (&actual.inbox_path, &expected.inbox_path),
        (&actual.dashboard_path, &expected.dashboard_path),
    ] {
        let resolve = |p: &str| -> Result<PathBuf> {
            match std::fs::canonicalize(p) {
                Ok(path) => Ok(path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => physical_leaf(Path::new(p)),
                Err(e) => Err(e.into()),
            }
        };
        ensure!(resolve(a)? == resolve(b)?, "source destination changed");
    }
    Ok(())
}

fn scope(cfg: &WorkerConfig, jobs: &[CaptureJob]) -> Result<()> {
    ensure!(
        jobs.iter()
            .all(|j| j.account_key == cfg.account_namespace && j.list_id == cfg.list_id),
        "queue_scope_mismatch"
    );
    Ok(())
}

fn counts(jobs: &[CaptureJob]) -> DeliveryReport {
    let mut r = DeliveryReport::default();
    for job in jobs {
        match job.state {
            DeliveryState::Pending => r.pending += 1,
            DeliveryState::Delivering | DeliveryState::NeedsReview => r.blocked += 1,
            DeliveryState::Unsupported => r.unsupported += 1,
            DeliveryState::CalendarPending => r.calendar_pending += 1,
            DeliveryState::Baseline => r.baselined += 1,
            DeliveryState::Ignored => r.ignored += 1,
            DeliveryState::Delivered => {}
        }
        if job.provider_changed
            && !matches!(
                job.state,
                DeliveryState::Delivering | DeliveryState::NeedsReview
            )
        {
            r.blocked += 1;
        }
    }
    r
}

fn previous(paths: &Paths) -> Result<Option<CycleStatus>> {
    match state::read_json::<CycleStatus>(&paths.status) {
        Ok(status) => {
            validate_status(&status, None)?;
            Ok(Some(status))
        }
        Err(e) if missing(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

fn validate_status(status: &CycleStatus, id: Option<&str>) -> Result<()> {
    ensure!(
        status.schema_version == "1"
            && status.kind == "capture-background-status"
            && id.is_none_or(|id| status.cycle_id == id),
        "invalid child cycle response"
    );
    state::lease_path(&Paths::in_root(PathBuf::from("/unused")), &status.cycle_id)?;
    for digest in [&status.auth_binding_digest, &status.config_digest]
        .into_iter()
        .flatten()
    {
        let hex = digest
            .strip_prefix("sha256:")
            .context("invalid status digest")?;
        ensure!(
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid status digest"
        );
    }
    // Status strings are an allowlist, not a channel for child diagnostics.
    ensure!(
        ["starting", "polling", "staged", "delivery", "finished"].contains(&status.phase.as_str()),
        "invalid cycle phase"
    );
    ensure!(
        [
            "running",
            "complete",
            "no_pending",
            "deferred",
            "queue_scope_mismatch",
            "source_scope_mismatch",
            "poll_unavailable",
            "auth_action_required",
            "credential_busy",
            "authentication_transport",
            "keychain_locked_or_denied",
            "sign_in_required",
            "not_connected",
            "connection_binding_mismatch_or_logged_out",
            "connection_binding_mismatch",
            "unknown_outcome",
            "cycle_failed",
            "cycle_timeout",
            "cycle_cancelled"
        ]
        .contains(&status.outcome.as_str()),
        "invalid cycle outcome"
    );
    Ok(())
}

fn check(paths: &Paths) -> Result<Value> {
    let cfg = config::load(paths)?;
    let jobs = CaptureStore::inspect(&cfg.queue_path)?;
    let count = registry(paths)?.len();
    Ok(
        json!({"schemaVersion":"1", "configValid":true, "queueScopeValid":scope(&cfg, &jobs).is_ok(),
        "counts":counts(&jobs), "editorRegistrationCount":count,
        "editorProbe":"not_performed_read_only_check; run-once uses describe and guarded delivery"}),
    )
}

fn registry(paths: &Paths) -> Result<Vec<EditorEntry>> {
    let directory = match std::fs::read_dir(&paths.registry) {
        Ok(directory) => directory,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut entries = Vec::new();
    for item in directory {
        ensure!(entries.len() < 256, "editor registry limit");
        let path = item?.path();
        let entry: EditorEntry = state::read_json(&path)?;
        ensure!(
            entry.schema_version == "1" && entry.pid > 1 && !entry.process_start.is_empty(),
            "invalid editor registration"
        );
        state::lease_path(paths, &entry.session_id)?;
        ensure!(
            path.file_name()
                == Some(std::ffi::OsStr::new(&format!(
                    "{}-{}.json",
                    entry.pid, entry.session_id
                ))),
            "invalid registration name"
        );
        entries.push(entry);
    }
    Ok(entries)
}

fn successful(result: &process::ProcessResult) -> bool {
    result.reason == process::ProcessReason::Exited && result.exit_code == Some(0)
}

fn child_command(cfg: &WorkerConfig, action: &str, id: &str) -> Result<Command> {
    let mut cmd = Command::new(&cfg.hive_binary);
    cmd.args(["captures", "background", action, "--cycle-id", id])
        .current_dir(&cfg.working_directory)
        .env_clear()
        .envs(config::source_environment(cfg)?);
    Ok(cmd)
}

fn publish(paths: &Paths, status: &CycleStatus) -> Result<()> {
    validate_status(status, None)?;
    state::write_json(&paths.status, status)
}

fn run_once(paths: &Paths) -> Result<CycleStatus> {
    config::load(paths)?;
    paths.ensure()?;
    let lock = FileLock::acquire(&paths.worker_lock, false, Duration::ZERO)?;
    let cfg = config::load(paths)?;
    let old = previous(paths)?;
    let id = hive_background::nonce()?;
    let mut status = starting_status(&cfg, old.as_ref(), id.clone())?;
    publish(paths, &status)?;
    let deadline = now_ms() + cfg.cycle_timeout_seconds * 1000;
    let mut cmd = child_command(&cfg, "cycle", &id)?;
    cmd.args(["--deadline-ms", &deadline.to_string()])
        .env(WORKER_FD, lock.as_raw_fd().to_string());
    let result = process::run_supervised(
        &mut cmd,
        Duration::from_secs(cfg.cycle_timeout_seconds + 2),
        &[lock.as_raw_fd()],
    );
    match result {
        Ok(result) if successful(&result) => {
            if let Ok(child) = serde_json::from_slice::<CycleStatus>(&result.stdout)
                && validate_status(&child, Some(&id)).is_ok()
                && child.finished_at_ms.is_some()
                && child.config_digest == status.config_digest
                && child.auth_binding_digest == status.auth_binding_digest
            {
                publish(paths, &child)?;
                return Ok(child);
            }
            status.outcome = "unknown_outcome".into();
        }
        Ok(result) => {
            status.outcome = match result.reason {
                process::ProcessReason::Timeout => "cycle_timeout",
                process::ProcessReason::Cancelled => "cycle_cancelled",
                _ => "unknown_outcome",
            }
            .into()
        }
        Err(_) => status.outcome = "unknown_outcome".into(),
    }
    // The child may have published newer counts before its interruption. Never
    // discard those or retry an uncertain write.
    if let Ok(Some(latest)) = previous(paths)
        && latest.cycle_id == id
        && latest.config_digest == status.config_digest
        && latest.auth_binding_digest == status.auth_binding_digest
    {
        status.pending_count = latest.pending_count;
        status.collected = latest.collected;
        status.delivery = latest.delivery;
        status.auth_action_required = latest.auth_action_required;
        status.auth_generation = latest.auth_generation;
    }
    status.phase = "finished".into();
    status.finished_at_ms = Some(now_ms());
    publish(paths, &status)?;
    Ok(status)
}

fn auth_binding_digest(cfg: &WorkerConfig) -> Result<String> {
    let bytes = serde_json::to_vec(&(
        "hive-background-auth-binding:v1",
        &cfg.connection,
        &cfg.account_namespace,
    ))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn starting_status(
    cfg: &WorkerConfig,
    old: Option<&CycleStatus>,
    id: String,
) -> Result<CycleStatus> {
    let mut status = CycleStatus::new(id);
    status.config_digest = Some(config::digest(cfg)?);
    status.auth_binding_digest = Some(auth_binding_digest(cfg)?);
    if let Some(old) = old {
        if old.config_digest == status.config_digest {
            status.pending_count = old.pending_count;
            status.delivery = old.delivery.clone();
        }
        if old.auth_binding_digest == status.auth_binding_digest {
            status.auth_action_required = old.auth_action_required;
            status.auth_generation = old.auth_generation;
        }
    }
    Ok(status)
}

/// Inherited locks must still name the private, persistent inode. Reflocking
/// succeeds on the same open-file description, not a competing coordinator's.
fn inherited(name: &str, path: &Path, shared: bool) -> Result<File> {
    let fd: RawFd = std::env::var(name)?.parse()?;
    inherited_fd(fd, path, shared)
}

fn inherited_fd(fd: RawFd, path: &Path, shared: bool) -> Result<File> {
    ensure!(fd >= 3, "invalid inherited descriptor");
    let copy = unsafe { libc::dup(fd) };
    ensure!(copy >= 3, "missing inherited descriptor");
    let file = unsafe { File::from_raw_fd(copy) };
    let a = file.metadata()?;
    let b = std::fs::symlink_metadata(path)?;
    ensure!(
        a.is_file()
            && b.is_file()
            && a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.uid() == unsafe { libc::geteuid() }
            && a.mode() & 0o777 == 0o600
            && a.nlink() == 1,
        "inherited descriptor identity mismatch"
    );
    ensure!(
        FileLock::acquire(path, false, Duration::ZERO).is_err(),
        "descriptor did not inherit a held lock"
    );
    ensure!(
        unsafe {
            libc::flock(
                copy,
                (if shared { libc::LOCK_SH } else { libc::LOCK_EX }) | libc::LOCK_NB,
            )
        } == 0,
        "inherited lock unavailable"
    );
    Ok(file)
}

fn own_group() -> Result<()> {
    ensure!(
        unsafe { libc::getpgrp() } == std::process::id() as i32 && std::process::id() > 1,
        "dedicated child group required"
    );
    Ok(())
}

fn cycle(paths: &Paths, id: &str, deadline: u64) -> Result<CycleStatus> {
    state::lease_path(paths, id)?;
    let cfg = config::load(paths)?;
    ensure!(
        deadline > now_ms() && deadline <= now_ms() + cfg.cycle_timeout_seconds * 1000,
        "invalid cycle deadline"
    );
    let _worker = inherited(WORKER_FD, &paths.worker_lock, false)?;
    own_group()?;
    let _watchdog =
        process::install_group_watchdog(deadline, unsafe { libc::getppid() } as u32, None)?;
    source_context(&cfg)?;
    let mut status = previous(paths)?.context("missing cycle admission")?;
    ensure!(
        status.cycle_id == id
            && status.finished_at_ms.is_none()
            && status.config_digest.as_deref() == Some(config::digest(&cfg)?.as_str())
            && status.auth_binding_digest.as_deref() == Some(auth_binding_digest(&cfg)?.as_str()),
        "cycle admission mismatch"
    );
    status.worker_pid = Some(std::process::id());
    status.worker_pgid = Some(std::process::id());
    let result = cycle_work(paths, &cfg, deadline, &mut status);
    if result.is_err() {
        status.outcome = "cycle_failed".into();
    }
    status.phase = "finished".into();
    status.finished_at_ms = Some(now_ms());
    publish(paths, &status)?;
    Ok(status)
}

fn cycle_work(
    paths: &Paths,
    cfg: &WorkerConfig,
    deadline: u64,
    status: &mut CycleStatus,
) -> Result<()> {
    cycle_work_with(
        paths,
        cfg,
        deadline,
        status,
        |status| poll(cfg, status),
        // Keep mw in this protected group, not a nested group that could outlive
        // the coordinator watchdog while retaining its cooperative worker FD.
        || MindWeaver::new(&cfg.mw_binary)?.binding(),
        |lease| editor::deliver_via_editor(paths, cfg, lease),
    )
}

fn poll(cfg: &WorkerConfig, status: &mut CycleStatus) -> Result<()> {
    let paused = paused_generation(status);
    match captures_auth::background_client(&cfg.connection, &cfg.account_namespace, paused) {
        Ok(BackgroundCredentials::Ready {
            resolved,
            generation,
        }) => {
            status.auth_action_required = false;
            status.auth_generation = Some(generation);
            match resolved.client.list_tasks(&cfg.list_id) {
                Ok(tasks) => {
                    let mut store = CaptureStore::open_existing(&cfg.queue_path)?;
                    scope(cfg, &store.jobs()?)?;
                    status.collected = Some(
                        store.ingest_verified(
                            resolved
                                .identity
                                .as_ref()
                                .context("verified identity missing")?,
                            &cfg.list_id,
                            &tasks,
                            BootstrapPolicy::Baseline,
                        )?,
                    );
                    status.outcome = "complete".into();
                }
                Err(_) => status.outcome = "poll_unavailable".into(),
            }
        }
        Ok(BackgroundCredentials::NeedsAction { generation, reason }) => {
            status.auth_action_required = true;
            status.auth_generation = generation;
            status.outcome = reason.into();
        }
        Ok(BackgroundCredentials::Transient { reason }) => status.outcome = reason.into(),
        Err(_) => status.outcome = "poll_unavailable".into(),
    }
    Ok(())
}

fn paused_generation(status: &CycleStatus) -> Option<u64> {
    if status.auth_action_required {
        status.auth_generation
    } else {
        None
    }
}

fn cycle_work_with(
    paths: &Paths,
    cfg: &WorkerConfig,
    deadline: u64,
    status: &mut CycleStatus,
    poll: impl FnOnce(&mut CycleStatus) -> Result<()>,
    binding: impl FnOnce() -> Result<DestinationBinding>,
    deliver: impl FnOnce(&DeliveryLease) -> Result<EditorDecision>,
) -> Result<()> {
    let jobs = CaptureStore::inspect(&cfg.queue_path)?;
    if scope(cfg, &jobs).is_err() {
        status.outcome = "queue_scope_mismatch".into();
        return Ok(());
    }
    status.pending_count = counts(&jobs).pending;
    status.delivery = Some(counts(&jobs));
    status.phase = "polling".into();
    publish(paths, status)?;
    poll(status)?;
    let jobs = CaptureStore::inspect(&cfg.queue_path)?;
    if scope(cfg, &jobs).is_err() {
        status.outcome = "queue_scope_mismatch".into();
        return Ok(());
    }
    status.pending_count = counts(&jobs).pending;
    status.delivery = Some(counts(&jobs));
    status.phase = "staged".into();
    publish(paths, status)?;
    if status.pending_count == 0 {
        if status.outcome == "complete" {
            status.outcome = "no_pending".into();
        }
        return Ok(());
    }
    if deadline.saturating_sub(now_ms()) < 5000 {
        return defer(status, DeferralReason::CycleBudget);
    }
    if source_locked(cfg)? {
        return defer(status, DeferralReason::SourceLocked);
    }
    let _admission = match FileLock::acquire(&paths.admission_lock, true, Duration::ZERO) {
        Ok(lock) => lock,
        Err(_) => return defer(status, DeferralReason::AdmissionBusy),
    };
    // Read-only binding discovery is bounded by the group's independent deadline.
    let actual = binding()?;
    if same_binding(&actual, &cfg.destination).is_err() {
        status.outcome = "source_scope_mismatch".into();
        return Ok(());
    }
    let me =
        process::find_process(std::process::id())?.context("coordinator metadata unavailable")?;
    let lease = DeliveryLease {
        schema_version: "1".into(),
        cycle_id: status.cycle_id.clone(),
        config_digest: config::digest(cfg)?,
        coordinator_pid: me.pid,
        coordinator_start: me.start_identity,
        deadline_ms: deadline,
        destination: cfg.destination.clone(),
        editor_session: None,
        helper_pid: None,
        helper_pgid: None,
    };
    state::write_json(&state::lease_path(paths, &status.cycle_id)?, &lease)?;
    status.phase = "delivery".into();
    publish(paths, status)?;
    match deliver(&lease).unwrap_or(EditorDecision::Deferred(DeferralReason::UnknownOutcome)) {
        EditorDecision::Deferred(reason) => defer(status, reason)?,
        EditorDecision::Completed(report) => completed(status, report),
        EditorDecision::NoEditor => {
            let mut cmd = child_command(cfg, "deliver-owned", &status.cycle_id)?;
            cmd.args(["--config-digest", &lease.config_digest]);
            match process::run_supervised(&mut cmd, remaining(deadline)?, &[]) {
                Ok(result) if successful(&result) => {
                    match serde_json::from_slice::<DeliveryResponse>(&result.stdout) {
                        Ok(reply)
                            if reply.schema_version == "1"
                                && reply.kind == "capture-background-delivery"
                                && reply.writer_exited =>
                        {
                            completed_writer(status, reply)?;
                        }
                        _ => defer(status, DeferralReason::UnknownOutcome)?,
                    }
                }
                _ => defer(status, DeferralReason::UnknownOutcome)?,
            }
        }
    }
    Ok(())
}

fn completed(status: &mut CycleStatus, report: DeliveryReport) {
    status.pending_count = report.pending;
    status.delivery = Some(report);
    // Successful local delivery does not erase a paused or unavailable polling lane.
}

fn completed_writer(status: &mut CycleStatus, reply: DeliveryResponse) -> Result<()> {
    completed(status, reply.report);
    if let Some(reason) = reply.deferred {
        defer(status, reason)?;
    }
    Ok(())
}

fn defer(status: &mut CycleStatus, reason: DeferralReason) -> Result<()> {
    status.outcome = "deferred".into();
    status.deferred = Some(reason);
    Ok(())
}

fn source_locked(cfg: &WorkerConfig) -> Result<bool> {
    match std::fs::symlink_metadata(Path::new(&cfg.destination.notes_dir).join(".mw/todos.lock")) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn remaining(deadline: u64) -> Result<Duration> {
    let left = deadline.saturating_sub(now_ms());
    ensure!(left > 0, "cycle deadline expired");
    Ok(Duration::from_millis(left))
}

fn read_lease(paths: &Paths, id: &str) -> Result<DeliveryLease> {
    let lease: DeliveryLease = state::read_json(&state::lease_path(paths, id)?)?;
    ensure!(
        lease.schema_version == "1"
            && lease.cycle_id == id
            && lease.coordinator_pid > 1
            && !lease.coordinator_start.is_empty(),
        "invalid lease origin"
    );
    Ok(lease)
}

fn active_lease(
    paths: &Paths,
    cfg: &WorkerConfig,
    id: &str,
    digest: &str,
) -> Result<DeliveryLease> {
    let lease = read_lease(paths, id)?;
    ensure!(
        lease.config_digest == digest
            && config::digest(cfg)? == digest
            && lease.destination == cfg.destination
            && lease.deadline_ms > now_ms(),
        "lease configuration mismatch or expired"
    );
    let owner =
        process::find_process(lease.coordinator_pid)?.context("lease coordinator unavailable")?;
    ensure!(
        owner.uid == unsafe { libc::geteuid() }
            && owner.start_identity == lease.coordinator_start
            && owner.executable == cfg.hive_binary
            && owner.process_group == owner.pid,
        "lease coordinator changed"
    );
    Ok(lease)
}

fn authorized_parent(
    paths: &Paths,
    cfg: &WorkerConfig,
    lease: &DeliveryLease,
    pid: u32,
) -> Result<()> {
    if pid == lease.coordinator_pid {
        return Ok(());
    }
    let parent = process::find_process(pid)?.context("delivery parent unavailable")?;
    ensure!(
        parent.uid == unsafe { libc::geteuid() } && parent.executable == cfg.nvim_binary,
        "delivery parent not an editor"
    );
    ensure!(
        registry(paths)?.iter().any(|entry| entry.pid == pid
            && entry.process_start == parent.start_identity
            && lease
                .editor_session
                .as_ref()
                .is_none_or(|session| session == &entry.session_id)),
        "delivery parent is not registered"
    );
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeliveryResponse {
    schema_version: String,
    kind: String,
    report: DeliveryReport,
    writer_exited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deferred: Option<DeferralReason>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WriterResult {
    report: DeliveryReport,
    budget_deferred: bool,
}

fn deliver_owned(paths: &Paths, id: &str, digest: &str) -> Result<DeliveryResponse> {
    let cfg = config::load(paths)?;
    let lease = active_lease(paths, &cfg, id, digest)?;
    authorized_parent(paths, &cfg, &lease, unsafe { libc::getppid() } as u32)?;
    let delivery = FileLock::acquire(&paths.delivery_lock, false, Duration::ZERO)?;
    let admission = FileLock::acquire(&paths.admission_lock, true, Duration::ZERO)?;
    // Recheck after admission, which serializes explicit configuration replacement.
    active_lease(paths, &config::load(paths)?, id, digest)?;
    let mut cmd = child_command(&cfg, "deliver-cycle", id)?;
    cmd.args(["--config-digest", digest])
        .env(DELIVERY_FD, delivery.as_raw_fd().to_string())
        .env(ADMISSION_FD, admission.as_raw_fd().to_string());
    let result = process::run_supervised(
        &mut cmd,
        remaining(lease.deadline_ms)?,
        &[delivery.as_raw_fd(), admission.as_raw_fd()],
    )?;
    ensure!(successful(&result), "delivery outcome unknown");
    let result: WriterResult = serde_json::from_slice(&result.stdout)?;
    Ok(DeliveryResponse {
        schema_version: "1".into(),
        kind: "capture-background-delivery".into(),
        report: result.report,
        writer_exited: true,
        deferred: result
            .budget_deferred
            .then_some(DeferralReason::CycleBudget),
    })
}

fn deliver_cycle(paths: &Paths, id: &str, digest: &str) -> Result<WriterResult> {
    let cfg = config::load(paths)?;
    let mut lease = active_lease(paths, &cfg, id, digest)?;
    let _delivery = inherited(DELIVERY_FD, &paths.delivery_lock, false)?;
    let _admission = inherited(ADMISSION_FD, &paths.admission_lock, true)?;
    own_group()?;
    let parent = unsafe { libc::getppid() } as u32;
    let supervisor = process::find_process(parent)?.context("delivery supervisor unavailable")?;
    ensure!(
        supervisor.executable == cfg.hive_binary && supervisor.uid == unsafe { libc::geteuid() },
        "delivery supervisor mismatch"
    );
    let _watchdog =
        process::install_group_watchdog(lease.deadline_ms, parent, Some(lease.coordinator_pid))?;
    lease.helper_pid = Some(std::process::id());
    lease.helper_pgid = Some(std::process::id());
    state::write_json(&state::lease_path(paths, id)?, &lease)?;
    let jobs = CaptureStore::inspect(&cfg.queue_path)?;
    scope(&cfg, &jobs)?;
    ensure!(!source_locked(&cfg)?, "source locked");
    // This process was launched with env_clear + frozen public source selectors.
    // MindWeaver freezes them and pins its discovered binding on every write.
    source_context(&cfg)?;
    let deadline = Instant::now()
        .checked_add(remaining(lease.deadline_ms)?)
        .context("invalid writer deadline")?;
    let mw = MindWeaver::new(&cfg.mw_binary)?;
    same_binding(&mw.binding()?, &cfg.destination)?;
    let mut store = CaptureStore::open_existing(&cfg.queue_path)?;
    scope(&cfg, &store.jobs()?)?;
    ensure!(!source_locked(&cfg)?, "source locked");
    let (report, budget_deferred) =
        mw.deliver_pending_before(&mut store, 1, deadline, Duration::from_secs(5))?;
    Ok(WriterResult {
        report,
        budget_deferred,
    })
}

fn source_context(cfg: &WorkerConfig) -> Result<()> {
    ensure!(
        std::env::current_dir()? == cfg.working_directory,
        "source working directory changed"
    );
    let expected_env = config::source_environment(cfg)?;
    for (key, value) in &expected_env {
        ensure!(
            std::env::var_os(key).as_ref() == Some(value),
            "source environment changed"
        );
    }
    ensure!(
        selectors(std::env::vars_os())? == cfg.mw_environment,
        "source selectors changed"
    );
    Ok(())
}

fn lease_status(paths: &Paths, id: &str) -> Result<Value> {
    let _lease = read_lease(paths, id)?;
    // No deadline, current config digest, PID disappearance or RPC disconnect is
    // a substitute for this positive cooperative-lock proof.
    let proof = FileLock::acquire(&paths.delivery_lock, false, Duration::ZERO);
    Ok(json!({"schemaVersion":"1", "cycleId":id, "writerGroupDead":proof.is_ok()}))
}

fn agents_dir() -> Result<PathBuf> {
    Ok(hive_google_auth::storage::system_user_home()?.join("Library/LaunchAgents"))
}

#[derive(Clone)]
struct LoadedJobIdentity {
    origin: PathBuf,
    program: PathBuf,
    arguments: Vec<String>,
    working_directory: PathBuf,
}

enum LaunchctlResult {
    NotLoaded,
    Loaded(LoadedJobIdentity),
    Controlled,
}

fn parse_loaded_job(bytes: &[u8], target: &str) -> Result<LoadedJobIdentity> {
    ensure!(bytes.len() <= 1024 * 1024, "service identity output limit");
    let text =
        std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("service identity unavailable"))?;
    let mut lines = text.lines();
    ensure!(
        lines.next() == Some(format!("{target} = {{").as_str()) && text.trim_end().ends_with("\n}"),
        "service identity format unavailable"
    );
    let mut origin = None;
    let mut program = None;
    let mut arguments = None;
    let mut cwd = None;
    while let Some(line) = lines.next() {
        if line == "}" {
            ensure!(
                lines.all(|line| line.trim().is_empty()),
                "unexpected service identity trailer"
            );
            break;
        }
        // Only the fixed one-tab fields belong to the job. In particular, never
        // interpret nested environment dictionaries as job identity metadata.
        let Some(field) = line.strip_prefix('\t').filter(|s| !s.starts_with('\t')) else {
            continue;
        };
        ensure!(field != "}", "unexpected service identity block end");
        if field == "arguments = {" {
            ensure!(arguments.is_none(), "duplicate service arguments");
            let mut args = Vec::new();
            loop {
                let item = lines.next().context("incomplete service arguments")?;
                if item == "\t}" {
                    break;
                }
                let item = item
                    .strip_prefix("\t\t")
                    .context("invalid service argument indentation")?;
                ensure!(
                    args.len() < 64 && item.len() <= 4096 && !item.chars().any(char::is_control),
                    "invalid service argument"
                );
                args.push(item.to_owned());
            }
            arguments = Some(args);
        } else if field.ends_with(" = {") {
            // Skip unselected dictionaries as blocks, not as candidate identity
            // fields. Ambiguous indentation must fail closed.
            loop {
                let item = lines.next().context("incomplete service metadata block")?;
                if item == "\t}" {
                    break;
                }
                ensure!(
                    item.is_empty() || item.starts_with("\t\t"),
                    "invalid service metadata indentation"
                );
            }
        } else {
            let destination = if let Some(value) = field.strip_prefix("path = ") {
                Some((&mut origin, value))
            } else if let Some(value) = field.strip_prefix("program = ") {
                Some((&mut program, value))
            } else {
                field
                    .strip_prefix("working directory = ")
                    .map(|value| (&mut cwd, value))
            };
            if let Some((slot, value)) = destination {
                ensure!(
                    slot.is_none()
                        && value.len() <= 4096
                        && Path::new(value).is_absolute()
                        && !value.chars().any(char::is_control),
                    "invalid service identity field"
                );
                *slot = Some(PathBuf::from(value));
            }
        }
    }
    Ok(LoadedJobIdentity {
        origin: origin.context("service origin unavailable")?,
        program: program.context("service program unavailable")?,
        arguments: arguments.context("service arguments unavailable")?,
        working_directory: cwd.context("service working directory unavailable")?,
    })
}

fn launchctl_result(args: &[OsString], result: process::ProcessResult) -> Result<LaunchctlResult> {
    ensure!(
        result.reason == process::ProcessReason::Exited,
        "service control unavailable"
    );
    if args.first().is_some_and(|arg| arg == "print") {
        // Darwin ESRCH (3), or launchctl's service-not-found exit (113), are
        // absence. Permission, domain, transport and other failures are unknown.
        return match result.exit_code {
            Some(0) => Ok(LaunchctlResult::Loaded(parse_loaded_job(
                &result.stdout,
                args.get(1)
                    .and_then(|arg| arg.to_str())
                    .context("invalid service target")?,
            )?)),
            Some(3 | 113) => Ok(LaunchctlResult::NotLoaded),
            _ => anyhow::bail!("service identity unavailable"),
        };
    }
    ensure!(result.exit_code == Some(0), "service control failed");
    Ok(LaunchctlResult::Controlled)
}

fn launchctl(args: &[OsString]) -> Result<LaunchctlResult> {
    let mut cmd = Command::new("/bin/launchctl");
    cmd.args(args).env_clear().env("PATH", "/usr/bin:/bin");
    let result = process::run_supervised(&mut cmd, Duration::from_secs(5), &[])?;
    launchctl_result(args, result)
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::geteuid() })
}
fn loaded_owned(cfg: &WorkerConfig, agents: &Path, result: LaunchctlResult) -> Result<bool> {
    match result {
        LaunchctlResult::NotLoaded => Ok(false),
        LaunchctlResult::Loaded(job) => {
            let binary = cfg
                .hive_binary
                .to_str()
                .context("invalid service executable")?;
            ensure!(
                job.origin == launchd::managed_plist_path(agents)
                    && job.program == cfg.hive_binary
                    && job.arguments == [binary, "captures", "background", "run-once"]
                    && job.working_directory == cfg.working_directory,
                "loaded service identity collision"
            );
            Ok(true)
        }
        LaunchctlResult::Controlled => anyhow::bail!("service identity unavailable"),
    }
}

fn service_status(
    cfg: Option<&WorkerConfig>,
    agents: &Path,
    mut runner: impl FnMut(&[OsString]) -> Result<LaunchctlResult>,
) -> Value {
    let result = runner(&[
        "print".into(),
        format!("{}/{}", domain(), hive_background::SERVICE_LABEL).into(),
    ]);
    match result {
        Ok(LaunchctlResult::NotLoaded) => {
            json!({"serviceActive":false, "serviceState":"not_loaded"})
        }
        Ok(result @ LaunchctlResult::Loaded(_)) => {
            let owned = cfg.is_some_and(|cfg| {
                validate_owned_plist(cfg, agents).is_ok()
                    && loaded_owned(cfg, agents, result).is_ok()
            });
            if owned {
                json!({"serviceActive":true, "serviceState":"loaded_owned"})
            } else {
                json!({"serviceActive":null, "serviceState":"collision_or_unverified"})
            }
        }
        _ => json!({"serviceActive":null, "serviceState":"unavailable"}),
    }
}

fn validate_owned_plist(cfg: &WorkerConfig, agents: &Path) -> Result<PathBuf> {
    let path = launchd::managed_plist_path(agents);
    // Unlike state JSON the LaunchAgents directory conventionally permits 0755.
    // Pin the file with O_NOFOLLOW and compare identities before control actions.
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0
            && meta.len() <= 65536,
        "unowned launch agent"
    );
    let mut bytes = Vec::new();
    (&mut file).take(65537).read_to_end(&mut bytes)?;
    ensure!(
        bytes == launchd::render(cfg)?.as_bytes(),
        "unowned launch agent content"
    );
    let after = std::fs::symlink_metadata(&path)?;
    ensure!(
        after.dev() == meta.dev() && after.ino() == meta.ino() && after.is_file(),
        "launch agent changed"
    );
    ensure!(
        std::fs::canonicalize(agents)? == agents,
        "launch agent directory must be physical"
    );
    for ancestor in agents.ancestors() {
        let meta = std::fs::symlink_metadata(ancestor)?;
        ensure!(
            meta.is_dir()
                && (meta.uid() == 0 || meta.uid() == unsafe { libc::geteuid() })
                && meta.mode() & 0o002 == 0,
            "untrusted launch agent ancestor"
        );
    }
    Ok(path)
}

fn service(
    paths: &Paths,
    cfg: &WorkerConfig,
    agents: &Path,
    action: BackgroundCommand,
    mut runner: impl FnMut(&[OsString]) -> Result<LaunchctlResult>,
) -> Result<Value> {
    let target = format!("{}/{}", domain(), hive_background::SERVICE_LABEL);
    if matches!(action, BackgroundCommand::Install) {
        if launchd::managed_plist_path(agents).try_exists()? {
            validate_owned_plist(cfg, agents)?;
            loaded_owned(cfg, agents, runner(&["print".into(), target.into()])?)?;
        } else {
            ensure!(
                matches!(
                    runner(&["print".into(), target.into()])?,
                    LaunchctlResult::NotLoaded
                ),
                "refusing an already loaded label without an owned plist"
            );
        }
        launchd::install_plist(paths, cfg, agents)?;
        return Ok(json!({"schemaVersion":"1", "installed":true}));
    }
    let plist = validate_owned_plist(cfg, agents)?;
    let loaded = loaded_owned(
        cfg,
        agents,
        runner(&["print".into(), target.clone().into()])?,
    )?;
    match action {
        BackgroundCommand::Start => {
            if !loaded {
                ensure!(
                    matches!(
                        runner(&["bootstrap".into(), domain().into(), plist.into_os_string()])?,
                        LaunchctlResult::Controlled
                    ),
                    "service start failed"
                );
            }
            Ok(json!({"schemaVersion":"1", "serviceActive":true}))
        }
        BackgroundCommand::Stop | BackgroundCommand::Uninstall => {
            if loaded {
                ensure!(
                    matches!(
                        runner(&["bootout".into(), target.into()])?,
                        LaunchctlResult::Controlled
                    ),
                    "service stop failed"
                );
            }
            if matches!(action, BackgroundCommand::Uninstall) {
                launchd::remove_plist(paths, cfg, agents)?;
            }
            Ok(json!({"schemaVersion":"1", "serviceActive":false, "dataPreserved":true}))
        }
        _ => anyhow::bail!("invalid service control action"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use hive_capture::GoogleTask;
    use std::{cell::Cell, os::unix::fs::PermissionsExt};

    struct Fixture {
        _temp: tempfile::TempDir,
        paths: Paths,
        cfg: WorkerConfig,
        identity: hive_core::GoogleIdentity,
    }

    impl Fixture {
        fn new() -> Self {
            // Explicit fixtures only: no production constructor, Keychain, Google,
            // launchctl, editor discovery or user note process is used by tests.
            let temp = tempfile::tempdir_in(
                std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
            )
            .unwrap();
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let root = temp.path();
            let identity = hive_core::GoogleIdentity::new("background-fixture-subject").unwrap();
            let binary = root.join("never-executed");
            std::fs::write(&binary, b"test fixture, not code").unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            let cfg = WorkerConfig {
                schema_version: "1".into(),
                connection: "fixture".into(),
                account_namespace: identity.namespace().unwrap(),
                queue_path: root.join("queue.sqlite"),
                list_id: "fixture-list".into(),
                mw_binary: binary.clone(),
                hive_binary: binary.clone(),
                nvim_binary: binary,
                working_directory: root.into(),
                destination: DestinationBinding {
                    notes_dir: root.to_str().unwrap().into(),
                    inbox_path: root.join("inbox.md").to_str().unwrap().into(),
                    dashboard_path: root.join("dashboard.md").to_str().unwrap().into(),
                },
                mw_environment: BTreeMap::new(),
                interval_seconds: 60,
                cycle_timeout_seconds: 45,
                delivery_limit: 1,
            };
            let paths = Paths::in_root(root.join("state"));
            let mut store = CaptureStore::open(&cfg.queue_path).unwrap();
            store
                .ingest_verified(&identity, &cfg.list_id, &[], BootstrapPolicy::Baseline)
                .unwrap();
            drop(store);
            Self {
                _temp: temp,
                paths,
                cfg,
                identity,
            }
        }

        fn ingest(&self, tasks: &[GoogleTask]) -> hive_capture::IngestReport {
            CaptureStore::open_existing(&self.cfg.queue_path)
                .unwrap()
                .ingest_verified(
                    &self.identity,
                    &self.cfg.list_id,
                    tasks,
                    BootstrapPolicy::Baseline,
                )
                .unwrap()
        }

        fn status(&self) -> CycleStatus {
            CycleStatus::new("a".repeat(32))
        }
        fn prepare(&self) {
            self.paths.ensure().unwrap();
        }
        fn lease(&self) -> DeliveryLease {
            DeliveryLease {
                schema_version: "1".into(),
                cycle_id: "a".repeat(32),
                config_digest: "old-config".into(),
                coordinator_pid: 1234,
                coordinator_start: "fixture-start".into(),
                deadline_ms: 0,
                destination: self.cfg.destination.clone(),
                editor_session: None,
                helper_pid: None,
                helper_pgid: None,
            }
        }
    }

    fn task(id: &str) -> GoogleTask {
        GoogleTask {
            id: id.into(),
            title: "PRIVATE-TASK-CANARY".into(),
            status: "needsAction".into(),
            ..Default::default()
        }
    }

    fn loaded_job(cfg: &WorkerConfig, agents: &Path) -> LoadedJobIdentity {
        LoadedJobIdentity {
            origin: launchd::managed_plist_path(agents),
            program: cfg.hive_binary.clone(),
            arguments: vec![
                cfg.hive_binary.to_str().unwrap().into(),
                "captures".into(),
                "background".into(),
                "run-once".into(),
            ],
            working_directory: cfg.working_directory.clone(),
        }
    }

    fn process_reply(stdout: Vec<u8>, code: i32) -> process::ProcessResult {
        process::ProcessResult {
            stdout,
            stderr: b"PRIVATE-STDERR-CANARY".to_vec(),
            exit_code: Some(code),
            timed_out: false,
            cancelled: false,
            reason: process::ProcessReason::Exited,
        }
    }

    #[test]
    fn unsafe_program_is_rejected_before_discovery_or_native_storage() {
        let f = Fixture::new();
        std::fs::set_permissions(&f.cfg.mw_binary, std::fs::Permissions::from_mode(0o777)).unwrap();
        let invoked = Cell::new(false);
        let prepared = prepare_binding(
            &f.cfg.mw_binary,
            &f.cfg.hive_binary,
            &f.cfg.nvim_binary,
            &f.cfg.working_directory,
            || {
                invoked.set(true);
                Ok(f.cfg.destination.clone())
            },
        );
        assert!(prepared.is_err());
        assert!(!invoked.get(), "untrusted source must never be executed");
        // Runtime constructs native storage only after this preparation succeeds.
    }

    #[test]
    fn auth_pause_is_bound_to_connection_and_account_not_schedule_or_generation_alone() {
        let f = Fixture::new();
        let id = "a".repeat(32);
        let mut old = starting_status(&f.cfg, None, id.clone()).unwrap();
        old.auth_action_required = true;
        old.auth_generation = Some(1);
        old.pending_count = 9;
        old.delivery = Some(DeliveryReport {
            pending: 9,
            ..Default::default()
        });
        let same = starting_status(&f.cfg, Some(&old), id.clone()).unwrap();
        assert_eq!(paused_generation(&same), Some(1));
        assert_eq!(same.pending_count, 9);
        let mut other = f.cfg.clone();
        other.connection = "other-connection".into();
        let healthy_b = starting_status(&other, Some(&old), id.clone()).unwrap();
        assert_eq!(paused_generation(&healthy_b), None);
        assert!(!healthy_b.auth_action_required);
        assert_eq!(healthy_b.pending_count, 0);
        assert!(healthy_b.delivery.is_none());
        other = f.cfg.clone();
        other.account_namespace = hive_core::GoogleIdentity::new("other-fixture-subject")
            .unwrap()
            .namespace()
            .unwrap();
        assert_eq!(
            paused_generation(&starting_status(&other, Some(&old), id.clone()).unwrap()),
            None
        );
        other = f.cfg.clone();
        other.interval_seconds = 90;
        let scheduled = starting_status(&other, Some(&old), id.clone()).unwrap();
        assert_eq!(paused_generation(&scheduled), Some(1));
        assert_eq!(scheduled.pending_count, 0);
        assert!(scheduled.delivery.is_none());
        old.auth_binding_digest = None;
        assert_eq!(
            paused_generation(&starting_status(&f.cfg, Some(&old), id).unwrap()),
            None
        );
        assert!(
            !auth_binding_digest(&f.cfg)
                .unwrap()
                .contains(&f.cfg.connection)
        );
    }

    #[test]
    fn editor_context_is_bounded_digest_bound_and_needs_no_queue() {
        let f = Fixture::new();
        config::save(&f.paths, &f.cfg, false).unwrap();
        std::fs::remove_file(&f.cfg.queue_path).unwrap();
        let digest = config::digest(&f.cfg).unwrap();
        let response = editor_context_with(&f.paths, &digest, |cmd, timeout| {
            assert_eq!(timeout, Duration::from_secs(5));
            assert_eq!(cmd.get_program(), f.cfg.hive_binary.as_os_str());
            let args: Vec<_> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
            assert_eq!(
                &args[..5],
                [
                    "captures",
                    "background",
                    "context-cycle",
                    "--config-digest",
                    &digest
                ]
            );
            assert_eq!(args[5], "--deadline-ms");
            let deadline: u64 = args[6].parse().unwrap();
            assert!(deadline > now_ms() && deadline <= now_ms() + 4500);
            assert_eq!(cmd.get_envs().count(), 2);
            assert!(
                !cmd.get_envs()
                    .any(|(k, _)| k == WORKER_FD || k == DELIVERY_FD || k == ADMISSION_FD)
            );
            Ok(process_reply(
                serde_json::to_vec(&ContextResponse {
                    schema_version: "1".into(),
                    context: f.cfg.destination.clone(),
                })
                .unwrap(),
                0,
            ))
        })
        .unwrap();
        assert_eq!(response.context, f.cfg.destination);
        assert!(
            editor_context_with(&f.paths, "sha256:wrong", |_, _| panic!(
                "digest mismatch must not spawn"
            ))
            .is_err()
        );
        std::fs::set_permissions(&f.cfg.mw_binary, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            editor_context_with(&f.paths, &digest, |_, _| panic!(
                "unsafe source must not spawn"
            ))
            .is_err()
        );
        assert!(!f.paths.status.exists());
    }

    #[test]
    fn late_budget_after_source_preflight_defers_without_claim_and_releases_lock() {
        let f = Fixture::new();
        f.ingest(&[task("pending")]);
        f.prepare();
        let preflight = f.cfg.working_directory.join("preflight-marker");
        let writer = f.cfg.working_directory.join("writer-marker");
        let binding = serde_json::to_string(&json!({"schemaVersion":"1", "notesDir":f.cfg.destination.notes_dir,
            "inboxPath":f.cfg.destination.inbox_path, "dashboardPath":f.cfg.destination.dashboard_path})).unwrap();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = config ]; then\n printf '%s\\n' '{binding}'\nelif [ \"$3\" = --help ]; then\n : > '{}'\n printf '%s\\n' '--capture-id --json'\nelse\n : > '{}'\n exit 1\nfi\n",
            preflight.display(),
            writer.display()
        );
        std::fs::write(&f.cfg.mw_binary, script).unwrap();
        let held = FileLock::acquire(&f.paths.delivery_lock, false, Duration::ZERO).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mw = MindWeaver::new(&f.cfg.mw_binary).unwrap();
        let mut store = CaptureStore::open_existing(&f.cfg.queue_path).unwrap();
        let (report, deferred) = mw
            .deliver_pending_before(&mut store, 1, deadline, Duration::from_secs(5))
            .unwrap();
        assert!(deferred);
        assert!(preflight.exists());
        assert!(!writer.exists());
        assert_eq!(store.jobs().unwrap()[0].attempts, 0);
        assert_eq!(store.jobs().unwrap()[0].state, DeliveryState::Pending);
        let mut status = f.status();
        completed_writer(
            &mut status,
            DeliveryResponse {
                schema_version: "1".into(),
                kind: "capture-background-delivery".into(),
                report,
                writer_exited: true,
                deferred: Some(DeferralReason::CycleBudget),
            },
        )
        .unwrap();
        assert_eq!(status.deferred, Some(DeferralReason::CycleBudget));
        assert_eq!(status.pending_count, 1);
        assert_eq!(status.outcome, "deferred");
        assert_eq!(status.delivery.as_ref().unwrap().delivered, 0);
        drop(held);
        assert!(FileLock::acquire(&f.paths.delivery_lock, false, Duration::ZERO).is_ok());
    }

    #[test]
    fn owned_disk_plist_does_not_authorize_foreign_loaded_job() {
        let f = Fixture::new();
        let agents = f.cfg.working_directory.join("agents");
        let path = launchd::install_plist(&f.paths, &f.cfg, &agents).unwrap();
        let before = std::fs::read(&path).unwrap();
        for variant in 0..4 {
            let mut foreign = loaded_job(&f.cfg, &agents);
            match variant {
                0 => foreign.program = "/unowned/program".into(),
                1 => foreign.arguments.push("PRIVATE-TOKEN-CANARY".into()),
                2 => foreign.origin = "/unowned/origin.plist".into(),
                _ => foreign.working_directory = "/unowned/cwd".into(),
            }
            for action in [
                BackgroundCommand::Install,
                BackgroundCommand::Start,
                BackgroundCommand::Stop,
                BackgroundCommand::Uninstall,
            ] {
                let calls = Cell::new(0);
                let result = service(&f.paths, &f.cfg, &agents, action, |args| {
                    assert_eq!(
                        args[0], "print",
                        "collision must never control a loaded job"
                    );
                    calls.set(calls.get() + 1);
                    Ok(LaunchctlResult::Loaded(foreign.clone()))
                });
                assert!(result.is_err());
                assert!(
                    !result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("PRIVATE-TOKEN-CANARY")
                );
                assert_eq!(calls.get(), 1);
                assert_eq!(std::fs::read(&path).unwrap(), before);
            }
            let status = service_status(Some(&f.cfg), &agents, |_| {
                Ok(LaunchctlResult::Loaded(foreign.clone()))
            });
            assert_eq!(status["serviceState"], "collision_or_unverified");
            assert!(status["serviceActive"].is_null());
        }
    }

    #[test]
    fn launchctl_parser_retains_only_identity_and_rejects_unknown_errors() {
        let target = format!("{}/{}", domain(), hive_background::SERVICE_LABEL);
        let sample = format!(
            "{target} = {{\n\tpath = /fixture/agent.plist\n\tprogram = /fixture/hive\n\targuments = {{\n\t\t/fixture/hive\n\t\tcaptures\n\t\tbackground\n\t\trun-once\n\t}}\n\tworking directory = /fixture/work\n\tenvironment = {{\n\t\tprogram = PRIVATE-TOKEN-CANARY\n\t\tSECRET = PRIVATE-ENV-CANARY\n\t}}\n}}\n"
        );
        let job = parse_loaded_job(sample.as_bytes(), &target).unwrap();
        assert_eq!(job.program, Path::new("/fixture/hive"));
        assert_eq!(job.origin, Path::new("/fixture/agent.plist"));
        assert_eq!(job.arguments.len(), 4);
        assert_eq!(job.working_directory, Path::new("/fixture/work"));
        let args = ["print".into(), target.clone().into()];
        assert!(matches!(
            launchctl_result(&args, process_reply(sample.as_bytes().to_vec(), 0)).unwrap(),
            LaunchctlResult::Loaded(_)
        ));
        for code in [3, 113] {
            assert!(matches!(
                launchctl_result(&args, process_reply(Vec::new(), code)).unwrap(),
                LaunchctlResult::NotLoaded
            ));
        }
        for code in [1, 5, 13, 112] {
            let error =
                launchctl_result(&args, process_reply(b"PRIVATE-ENV-CANARY".to_vec(), code))
                    .err()
                    .unwrap();
            assert_eq!(error.to_string(), "service identity unavailable");
        }
        assert!(
            parse_loaded_job(
                sample
                    .replace(
                        "\tprogram = /fixture/hive",
                        "\tprogram = /fixture/hive\n\tprogram = /duplicate"
                    )
                    .as_bytes(),
                &target
            )
            .is_err()
        );
        assert!(parse_loaded_job(sample.as_bytes(), "wrong-target").is_err());
    }

    #[test]
    fn cli_contract_and_no_runtime_root_override() {
        #[derive(Debug, Parser)]
        struct Cli {
            #[command(subcommand)]
            command: BackgroundCommand,
        }
        for args in [
            vec!["test", "paths"],
            vec!["test", "editor-context", "--config-digest", "sha256:abc"],
            vec![
                "test",
                "context-cycle",
                "--config-digest",
                "sha256:abc",
                "--deadline-ms",
                "1234",
            ],
            vec![
                "test",
                "editor-register",
                "--pid",
                "42",
                "--socket",
                "/fixture/socket",
                "--session-id",
                "abc",
            ],
            vec![
                "test",
                "editor-unregister",
                "--pid",
                "42",
                "--session-id",
                "abc",
                "--socket",
                "/ignored",
            ],
            vec!["test", "lease-status", "--cycle-id", "abc"],
            vec![
                "test",
                "deliver-owned",
                "--cycle-id",
                "abc",
                "--config-digest",
                "sha256:abc",
            ],
            vec![
                "test",
                "deliver-cycle",
                "--cycle-id",
                "abc",
                "--config-digest",
                "sha256:abc",
            ],
            vec![
                "test",
                "cycle",
                "--cycle-id",
                "abc",
                "--deadline-ms",
                "1234",
            ],
            vec![
                "test",
                "configure",
                "--connection",
                "fixture",
                "--queue-path",
                "/queue",
                "--list-id",
                "list",
                "--mw-bin",
                "/mw",
                "--nvim-bin",
                "/nvim",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        assert!(Cli::try_parse_from(["test", "paths", "--state-root", "/tmp"]).is_err());
        assert!(Cli::try_parse_from(["test", "configure"]).is_err());
        assert!(Cli::try_parse_from(["test", "editor-context"]).is_err());
    }

    #[test]
    fn metadata_and_missing_config_never_create_state() {
        let f = Fixture::new();
        assert_eq!(
            paths_reply(&f.paths)["configPath"],
            f.paths.config.to_str().unwrap()
        );
        assert!(run_once(&f.paths).is_err());
        assert!(check(&f.paths).is_err());
        assert!(previous(&f.paths).unwrap().is_none());
        assert!(!f.paths.root.exists());
    }

    #[test]
    fn inherited_descriptors_require_private_persistent_matching_inode() {
        const CHILD_ENV: &str = "HIVE_TEST_INHERITED_DESCRIPTORS_ISOLATED";
        if std::env::var(CHILD_ENV).as_deref() != Ok("1") {
            // Parallel tests can fork and retain the intentionally inheritable duplicate.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "captures_background::tests::inherited_descriptors_require_private_persistent_matching_inode",
                    "--test-threads=1",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "isolated descriptor test failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let f = Fixture::new();
        f.prepare();
        let held = FileLock::acquire(&f.paths.delivery_lock, false, Duration::ZERO).unwrap();
        let inherited = inherited_fd(held.as_raw_fd(), &f.paths.delivery_lock, false).unwrap();
        assert!(FileLock::acquire(&f.paths.delivery_lock, false, Duration::ZERO).is_err());
        assert!(inherited_fd(held.as_raw_fd(), &f.paths.admission_lock, false).is_err());
        assert!(inherited_fd(1, &f.paths.delivery_lock, false).is_err());
        drop(held);
        assert!(FileLock::acquire(&f.paths.delivery_lock, false, Duration::ZERO).is_err());
        drop(inherited);
        assert!(FileLock::acquire(&f.paths.delivery_lock, false, Duration::ZERO).is_ok());
        let unlocked = File::open(&f.paths.delivery_lock).unwrap();
        assert!(inherited_fd(unlocked.as_raw_fd(), &f.paths.delivery_lock, false).is_err());
        std::fs::set_permissions(
            &f.paths.delivery_lock,
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let file = File::open(&f.paths.delivery_lock).unwrap();
        assert!(inherited_fd(file.as_raw_fd(), &f.paths.delivery_lock, false).is_err());
    }

    #[test]
    fn worker_children_have_only_explicit_public_source_environment() {
        let mut f = Fixture::new();
        f.cfg
            .mw_environment
            .insert("NOTES_DIR".into(), f.cfg.destination.notes_dir.clone());
        let cmd = child_command(&f.cfg, "cycle", &"a".repeat(32)).unwrap();
        let env: BTreeMap<_, _> = cmd
            .get_envs()
            .map(|(key, value)| (key.to_str().unwrap(), value.unwrap()))
            .collect();
        assert_eq!(env.len(), 3);
        assert!(env.contains_key("HOME"));
        assert!(env.contains_key("PATH"));
        assert!(env.contains_key("NOTES_DIR"));
        assert_eq!(
            cmd.get_current_dir(),
            Some(f.cfg.working_directory.as_path())
        );
        assert_eq!(
            cmd.get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "captures",
                "background",
                "cycle",
                "--cycle-id",
                &"a".repeat(32)
            ]
        );
    }

    #[test]
    fn selectors_only_serialize_approved_absolute_paths() {
        let values = [
            ("MW_CONFIG", "/fixture/config"),
            ("NOTES_DIR", "/fixture/notes"),
            ("HOME", "/private/home"),
            ("PATH", "/private/bin"),
            ("GOOGLE_TOKEN", "SECRET-CANARY"),
            ("HIVE_BACKGROUND_ROOT", "/override"),
        ];
        let selected = selectors(values.map(|(k, v)| (k.into(), v.into()))).unwrap();
        assert_eq!(selected.len(), 2);
        assert!(
            !serde_json::to_string(&selected)
                .unwrap()
                .contains("SECRET-CANARY")
        );
        for bad in ["relative", "", "/absolute\ncontrol"] {
            assert!(selectors([("MW_CONFIG".into(), bad.into())]).is_err());
        }
    }

    #[test]
    fn readonly_check_preserves_config_queue_and_pending_jobs() {
        let f = Fixture::new();
        f.ingest(&[task("new")]);
        config::save(&f.paths, &f.cfg, false).unwrap();
        let before_queue = std::fs::read(&f.cfg.queue_path).unwrap();
        let before_config = std::fs::read(&f.paths.config).unwrap();
        let result = check(&f.paths).unwrap();
        assert_eq!(result["counts"]["pending"], 1);
        assert_eq!(result["editorRegistrationCount"], 0);
        assert!(!result.to_string().contains("PRIVATE-TASK-CANARY"));
        assert_eq!(std::fs::read(&f.cfg.queue_path).unwrap(), before_queue);
        assert_eq!(std::fs::read(&f.paths.config).unwrap(), before_config);
        assert!(!f.paths.status.exists());
    }

    #[test]
    fn queue_scope_mismatch_precedes_poll_binding_and_editor() {
        let mut f = Fixture::new();
        f.ingest(&[task("new")]);
        f.cfg.list_id = "different-list".into();
        f.prepare();
        let mut status = f.status();
        cycle_work_with(
            &f.paths,
            &f.cfg,
            now_ms() + 20_000,
            &mut status,
            |_| panic!("must not poll"),
            || panic!("must not invoke source"),
            |_| panic!("must not discover editor"),
        )
        .unwrap();
        assert_eq!(status.outcome, "queue_scope_mismatch");
        let mut jobs = CaptureStore::inspect(&f.cfg.queue_path).unwrap();
        assert_eq!(jobs[0].attempts, 0);
        f.cfg.list_id = jobs[0].list_id.clone();
        assert!(scope(&f.cfg, &jobs).is_ok());
        jobs[0].account_key = "other-account".into();
        assert!(scope(&f.cfg, &jobs).is_err());
    }

    #[test]
    fn source_lock_and_budget_defer_before_source_or_claim() {
        let f = Fixture::new();
        f.ingest(&[task("new")]);
        f.prepare();
        let dir = Path::new(&f.cfg.destination.notes_dir).join(".mw");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("todos.lock"), b"fixture lock").unwrap();
        for (left, reason) in [
            (100, DeferralReason::CycleBudget),
            (20_000, DeferralReason::SourceLocked),
        ] {
            let mut status = f.status();
            cycle_work_with(
                &f.paths,
                &f.cfg,
                now_ms() + left,
                &mut status,
                |s| {
                    s.outcome = "complete".into();
                    Ok(())
                },
                || panic!("must not invoke source"),
                |_| panic!("must not deliver"),
            )
            .unwrap();
            assert_eq!(status.deferred, Some(reason));
        }
        let jobs = CaptureStore::inspect(&f.cfg.queue_path).unwrap();
        assert_eq!(jobs[0].state, DeliveryState::Pending);
        assert_eq!(jobs[0].attempts, 0);
    }

    #[test]
    fn source_binding_mismatch_prevents_lease_and_delivery() {
        let f = Fixture::new();
        f.ingest(&[task("new")]);
        f.prepare();
        let mut status = f.status();
        let mut other = f.cfg.destination.clone();
        other.inbox_path = Path::new(&other.notes_dir)
            .join("other.md")
            .to_str()
            .unwrap()
            .into();
        cycle_work_with(
            &f.paths,
            &f.cfg,
            now_ms() + 20_000,
            &mut status,
            |_| Ok(()),
            || Ok(other),
            |_| panic!("must not deliver"),
        )
        .unwrap();
        assert_eq!(status.outcome, "source_scope_mismatch");
        assert!(
            !state::lease_path(&f.paths, &status.cycle_id)
                .unwrap()
                .exists()
        );
        assert_eq!(
            CaptureStore::inspect(&f.cfg.queue_path).unwrap()[0].attempts,
            0
        );
    }

    #[test]
    fn repeated_empty_cycle_is_noop_without_source_or_editor() {
        let f = Fixture::new();
        f.prepare();
        for _ in 0..2 {
            let mut status = f.status();
            cycle_work_with(
                &f.paths,
                &f.cfg,
                now_ms() + 20_000,
                &mut status,
                |s| {
                    s.collected = Some(f.ingest(&[]));
                    s.outcome = "complete".into();
                    Ok(())
                },
                || panic!("must not invoke source"),
                |_| panic!("must not deliver"),
            )
            .unwrap();
            assert_eq!(status.outcome, "no_pending");
            assert_eq!(status.pending_count, 0);
        }
    }

    #[test]
    fn review_and_unsupported_counts_remain_visible_without_auto_resume() {
        let f = Fixture::new();
        let mut unsupported = task("unsupported");
        unsupported.notes = Some("extra content is not silently dropped".into());
        f.ingest(&[task("pending"), unsupported.clone()]);
        let mut store = CaptureStore::open_existing(&f.cfg.queue_path).unwrap();
        let job = store
            .jobs()
            .unwrap()
            .into_iter()
            .find(|j| j.state == DeliveryState::Pending)
            .unwrap();
        store.claim(&job.capture_id, &f.cfg.destination).unwrap();
        drop(store);
        let mut changed = task("pending");
        changed.title = "changed provider title".into();
        f.ingest(&[changed, unsupported]);
        f.prepare();
        let mut status = f.status();
        cycle_work_with(
            &f.paths,
            &f.cfg,
            now_ms() + 20_000,
            &mut status,
            |_| Ok(()),
            || panic!("must not invoke source"),
            |_| panic!("must not deliver"),
        )
        .unwrap();
        assert_eq!(status.pending_count, 0);
        let report = status.delivery.unwrap();
        assert_eq!(report.unsupported, 1);
        assert_eq!(report.blocked, 1);
        assert_eq!(
            CaptureStore::inspect(&f.cfg.queue_path)
                .unwrap()
                .iter()
                .find(|j| j.capture_id == job.capture_id)
                .unwrap()
                .attempts,
            1
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn dirty_editor_and_unknown_outcome_never_claim_or_retry() {
        let f = Fixture::new();
        f.ingest(&[task("pending")]);
        f.prepare();
        for reason in [DeferralReason::DirtyBuffers, DeferralReason::UnknownOutcome] {
            let mut status = f.status();
            let calls = Cell::new(0);
            cycle_work_with(
                &f.paths,
                &f.cfg,
                now_ms() + 20_000,
                &mut status,
                |s| {
                    s.outcome = "complete".into();
                    Ok(())
                },
                || Ok(f.cfg.destination.clone()),
                |_| {
                    calls.set(calls.get() + 1);
                    Ok(EditorDecision::Deferred(reason.clone()))
                },
            )
            .unwrap();
            assert_eq!(status.deferred, Some(reason));
            assert_eq!(calls.get(), 1);
            assert_eq!(status.pending_count, 1);
            let jobs = CaptureStore::inspect(&f.cfg.queue_path).unwrap();
            assert_eq!(jobs[0].state, DeliveryState::Pending);
            assert_eq!(jobs[0].attempts, 0);
            assert!(
                !serde_json::to_string(&status)
                    .unwrap()
                    .contains("PRIVATE-TASK-CANARY")
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn paused_auth_does_not_block_owned_pending_lane() {
        let f = Fixture::new();
        f.ingest(&[task("pending")]);
        f.prepare();
        let mut status = f.status();
        status.auth_action_required = true;
        status.auth_generation = Some(7);
        assert_eq!(paused_generation(&status), Some(7));
        cycle_work_with(
            &f.paths,
            &f.cfg,
            now_ms() + 20_000,
            &mut status,
            |s| {
                assert_eq!(paused_generation(s), Some(7));
                s.outcome = "sign_in_required".into();
                Ok(())
            },
            || Ok(f.cfg.destination.clone()),
            |_| {
                Ok(EditorDecision::Completed(DeliveryReport {
                    delivered: 1,
                    ..Default::default()
                }))
            },
        )
        .unwrap();
        assert!(status.auth_action_required);
        assert_eq!(status.outcome, "sign_in_required");
        assert_eq!(status.delivery.as_ref().unwrap().delivered, 1);
        // A local unlock has no generation pause. A foreground replacement is
        // detected by the auth helper comparing the stored generation to this one.
        status.auth_generation = None;
        assert_eq!(paused_generation(&status), None);
        status.auth_action_required = false;
        status.auth_generation = Some(8);
        assert_eq!(paused_generation(&status), None);
    }

    #[test]
    fn lease_exit_proof_uses_lock_not_deadline_current_config_or_helper_pid() {
        let f = Fixture::new();
        f.prepare();
        let lease = f.lease();
        state::write_json(
            &state::lease_path(&f.paths, &lease.cycle_id).unwrap(),
            &lease,
        )
        .unwrap();
        let held = FileLock::acquire(&f.paths.delivery_lock, false, Duration::ZERO).unwrap();
        assert_eq!(
            lease_status(&f.paths, &lease.cycle_id).unwrap()["writerGroupDead"],
            false
        );
        drop(held);
        assert_eq!(
            lease_status(&f.paths, &lease.cycle_id).unwrap()["writerGroupDead"],
            true
        );
        assert!(!f.paths.config.exists());
        assert!(f.paths.delivery_lock.exists());
        let mut invalid = lease;
        invalid.schema_version = "2".into();
        state::write_json(
            &state::lease_path(&f.paths, &invalid.cycle_id).unwrap(),
            &invalid,
        )
        .unwrap();
        assert!(lease_status(&f.paths, &invalid.cycle_id).is_err());
        assert!(lease_status(&f.paths, "../status").is_err());
    }

    #[test]
    fn service_controls_are_owned_idempotent_and_preserve_all_data() {
        let f = Fixture::new();
        config::save(&f.paths, &f.cfg, false).unwrap();
        let agents = f.cfg.working_directory.join("agents");
        let before_queue = std::fs::read(&f.cfg.queue_path).unwrap();
        let before_config = std::fs::read(&f.paths.config).unwrap();
        service(
            &f.paths,
            &f.cfg,
            &agents,
            BackgroundCommand::Install,
            |args| {
                assert_eq!(args[0], "print");
                Ok(LaunchctlResult::NotLoaded)
            },
        )
        .unwrap();
        let mut calls = Vec::new();
        service(
            &f.paths,
            &f.cfg,
            &agents,
            BackgroundCommand::Start,
            |args| {
                calls.push(args.to_vec());
                Ok(LaunchctlResult::Loaded(loaded_job(&f.cfg, &agents)))
            },
        )
        .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][0], "print");
        calls.clear();
        service(&f.paths, &f.cfg, &agents, BackgroundCommand::Stop, |args| {
            calls.push(args.to_vec());
            if args[0] == "print" {
                Ok(LaunchctlResult::Loaded(loaded_job(&f.cfg, &agents)))
            } else {
                Ok(LaunchctlResult::Controlled)
            }
        })
        .unwrap();
        assert_eq!(calls[1][0], "bootout");
        calls.clear();
        service(
            &f.paths,
            &f.cfg,
            &agents,
            BackgroundCommand::Uninstall,
            |args| {
                calls.push(args.to_vec());
                Ok(LaunchctlResult::NotLoaded)
            },
        )
        .unwrap();
        assert_eq!(calls.len(), 1);
        assert!(!launchd::managed_plist_path(&agents).exists());
        assert_eq!(std::fs::read(&f.cfg.queue_path).unwrap(), before_queue);
        assert_eq!(std::fs::read(&f.paths.config).unwrap(), before_config);
    }

    #[test]
    fn changed_plist_never_invokes_even_print_or_bootout() {
        let f = Fixture::new();
        let agents = f.cfg.working_directory.join("agents");
        let path = launchd::install_plist(&f.paths, &f.cfg, &agents).unwrap();
        std::fs::write(&path, b"<!-- hive-captures-background:v1 --> unowned").unwrap();
        for action in [
            BackgroundCommand::Start,
            BackgroundCommand::Stop,
            BackgroundCommand::Uninstall,
        ] {
            assert!(
                service(&f.paths, &f.cfg, &agents, action, |_| panic!(
                    "unowned label must not be controlled"
                ))
                .is_err()
            );
        }
        assert!(path.exists());
    }

    #[test]
    fn loaded_label_without_owned_plist_is_not_adopted() {
        let f = Fixture::new();
        let agents = f.cfg.working_directory.join("agents");
        assert!(
            service(
                &f.paths,
                &f.cfg,
                &agents,
                BackgroundCommand::Install,
                |args| {
                    assert_eq!(args[0], "print");
                    Ok(LaunchctlResult::Loaded(loaded_job(&f.cfg, &agents)))
                }
            )
            .is_err()
        );
        assert!(!agents.exists());
        assert!(!f.paths.root.exists());
    }

    #[test]
    fn status_and_binding_responses_fail_closed_without_leaking_raw_fields() {
        let f = Fixture::new();
        let mut status = f.status();
        validate_status(&status, Some(&status.cycle_id)).unwrap();
        status.outcome = "PRIVATE-PROVIDER-ERROR".into();
        assert!(validate_status(&status, None).is_err());
        assert!(binding_from_json(br#"{"schemaVersion":"2","notesDir":"/notes"}"#).is_err());
        let parsed = binding_from_json(br#"{"schemaVersion":"1","notesDir":"/notes","inboxPath":"/notes/i","dashboardPath":"/notes/d","token":"SECRET"}"#).unwrap();
        assert!(!serde_json::to_string(&parsed).unwrap().contains("SECRET"));
        assert!(serde_json::from_value::<DeliveryResponse>(json!({"schemaVersion":"1", "kind":"capture-background-delivery", "report":{}, "writerExited":true})).is_err());
    }
}
