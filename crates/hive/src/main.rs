use std::{io::Read, path::PathBuf, process::Command, thread, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use hive_core::{DoctorStatus, ProjectLayout};
use serde::Serialize;

mod calendar;
mod captures_auth;
mod captures_background;
mod tasks;
use captures_auth::{CaptureAuthCommand, GoogleCredentialArgs, ResolvedGoogleClient};

const DEFAULT_TOKEN_KEYCHAIN_SERVICE: &str = "mw/hive-sync";

#[derive(Debug, Parser)]
#[command(
    name = "hive",
    version,
    about = "Hive Mind sync/API/mobile companion CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Google Tasks auth, exact read, portable handoff, and advisory committed-source planning.
    Tasks {
        #[command(subcommand)]
        command: tasks::TasksCommand,
    },
    /// Local Calendar planning and separate OAuth management; no event publication.
    Calendar {
        #[command(subcommand)]
        command: calendar::CalendarCommand,
    },
    /// Inbound Google Tasks capture, separate from legacy todo synchronization.
    Captures {
        #[command(subcommand)]
        command: CapturesCommand,
    },
    /// Print Hive CLI version.
    Version {
        /// Print only the version number.
        #[arg(long)]
        short: bool,
    },
    /// Check local project layout and tool readiness.
    Doctor {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Project root to inspect. Defaults to current directory.
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Remote sync transport helpers.
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    /// PWA development helpers.
    Pwa {
        #[command(subcommand)]
        command: PwaCommand,
    },
    /// Run the Hive sync API server.
    Serve {
        /// Listen address.
        #[arg(long, env = "HIVE_ADDR", default_value = "127.0.0.1:8080")]
        addr: String,
        /// Server store backend.
        #[arg(long, value_enum, env = "HIVE_STORE", default_value_t = ServeStore::Memory)]
        store: ServeStore,
        /// SQLite store path when `--store sqlite` is selected.
        #[arg(long, env = "HIVE_SQLITE_PATH")]
        sqlite_path: Option<PathBuf>,
        /// Comma-separated device token map: desktop=token,phone=token.
        #[arg(long, env = "HIVE_SYNC_DEVICE_TOKENS")]
        device_tokens: Option<String>,
        /// Allow permissive CORS for browser/PWA development.
        #[arg(long, env = "HIVE_SYNC_PERMISSIVE_CORS")]
        permissive_cors: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CapturesCommand {
    /// Saved per-user capture scheduling and editor-aware delivery.
    Background {
        #[command(subcommand)]
        command: captures_background::BackgroundCommand,
    },
    /// Explicit Google browser login and local credential management.
    Auth {
        #[command(subcommand)]
        command: CaptureAuthCommand,
    },
    /// List Google task lists; read-only, no queue database or Markdown writes.
    Lists {
        #[command(flatten)]
        credentials: GoogleCredentialArgs,
    },
    /// Fetch a complete Google list and stage observations in a dedicated Hive queue.
    Poll {
        #[command(flatten)]
        connection: CaptureConnectionArgs,
        #[command(flatten)]
        credentials: GoogleCredentialArgs,
    },
    /// Deliver pending captures through mw; does not poll Google or retry unknown outcomes.
    Deliver {
        #[arg(long)]
        db_path: PathBuf,
        /// Absolute path to the trusted MindWeaver executable.
        #[arg(long)]
        mw_bin: PathBuf,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Run one Google poll followed by bounded local delivery. No background service is installed.
    Run {
        #[command(flatten)]
        connection: CaptureConnectionArgs,
        #[command(flatten)]
        credentials: GoogleCredentialArgs,
        #[arg(long)]
        mw_bin: PathBuf,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Read a dedicated queue without creating it or changing delivery state.
    Status {
        #[arg(long)]
        db_path: PathBuf,
    },
    /// Explicitly recover an identified capture queue, without invoking mw or Google.
    RecoverStore {
        #[arg(long)]
        db_path: PathBuf,
    },
    /// Resolve an interrupted capture by observing its existing stable ID; never recaptures.
    Reconcile {
        #[arg(long)]
        db_path: PathBuf,
        #[arg(long)]
        mw_bin: PathBuf,
        #[arg(long)]
        capture_id: String,
    },
}

#[derive(Debug, Args)]
struct CaptureConnectionArgs {
    /// Dedicated Hive capture SQLite file; never use the MindWeaver notes database.
    #[arg(long)]
    db_path: PathBuf,
    /// Operator namespace for explicit token-env mode only; saved connections derive identity.
    #[arg(long, requires = "access_token_env", conflicts_with = "connection")]
    account_key: Option<String>,
    #[arg(long)]
    list_id: String,
    /// Explicit first-scan policy: baseline existing items, or capture eligible open items.
    #[arg(long, value_enum)]
    bootstrap: CaptureBootstrap,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CaptureBootstrap {
    Baseline,
    ImportOpen,
}

fn collect_google_captures(
    connection: &CaptureConnectionArgs,
    resolved: &ResolvedGoogleClient,
) -> Result<(
    hive_capture::store::CaptureStore,
    hive_capture::IngestReport,
)> {
    use hive_capture::{BootstrapPolicy, store::CaptureStore};
    // Authentication and namespace validation precede this complete provider snapshot.
    let tasks = resolved.client.list_tasks(&connection.list_id)?;
    let mut store = CaptureStore::open(&connection.db_path)?;
    let policy = match connection.bootstrap {
        CaptureBootstrap::Baseline => BootstrapPolicy::Baseline,
        CaptureBootstrap::ImportOpen => BootstrapPolicy::ImportOpen,
    };
    let report = if let Some(identity) = &resolved.identity {
        store.ingest_verified(identity, &connection.list_id, &tasks, policy)?
    } else {
        store.ingest(
            connection
                .account_key
                .as_deref()
                .context("token-env capture requires --account-key")?,
            &connection.list_id,
            &tasks,
            policy,
        )?
    };
    Ok((store, report))
}

fn capture_delivery_result(report: &hive_capture::DeliveryReport) -> Result<()> {
    if report.blocked > 0 {
        bail!(
            "capture delivery requires review; inspect the queue and explicitly reconcile unknown outcomes"
        );
    }
    if report.calendar_pending > 0 {
        bail!(
            "dated tasks are retained as calendar_pending; Calendar routing is not implemented in this slice"
        );
    }
    Ok(())
}

fn captures_command(command: CapturesCommand) -> Result<()> {
    use hive_capture::{mindweaver::MindWeaver, store::CaptureStore};
    match command {
        CapturesCommand::Background { command } => captures_background::command(command)?,
        CapturesCommand::Auth { command } => captures_auth::auth_command(command)?,
        CapturesCommand::Lists { credentials } => {
            let lists = credentials.resolve_for_list()?.client.list_task_lists()?;
            let items: Vec<_> = lists
                .into_iter()
                .map(|list| serde_json::json!({"id":list.id,"title":list.title}))
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"schemaVersion":"1","kind":"google-task-lists","items":items})
                )?
            );
        }
        CapturesCommand::Poll {
            connection,
            credentials,
        } => {
            let resolved = credentials.resolve_for_capture(connection.account_key.as_deref())?;
            let (_, report) = collect_google_captures(&connection, &resolved)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"schemaVersion":"1","kind":"capture-poll-result","report":report})
                )?
            );
        }
        CapturesCommand::Status { db_path } => {
            let jobs = CaptureStore::inspect(db_path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"schemaVersion":"1","kind":"capture-queue-status","items":jobs})
                )?
            );
        }
        CapturesCommand::RecoverStore { db_path } => {
            let store = CaptureStore::open_existing(db_path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"schemaVersion":"1","kind":"capture-store-recovery","items":store.jobs()?})
                )?
            );
        }
        CapturesCommand::Deliver {
            db_path,
            mw_bin,
            limit,
        } => {
            if !(1..=1000).contains(&limit) {
                bail!("delivery limit must be 1..1000");
            }
            let mut store = CaptureStore::open_existing(db_path)?;
            let mw = MindWeaver::new(mw_bin)?;
            let report = mw.deliver_pending(&mut store, limit)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"schemaVersion":"1","kind":"capture-delivery-result","report":report})
                )?
            );
            capture_delivery_result(&report)?;
        }
        CapturesCommand::Run {
            connection,
            credentials,
            mw_bin,
            limit,
        } => {
            if !(1..=1000).contains(&limit) {
                bail!("delivery limit must be 1..1000");
            }
            let resolved = credentials.resolve_for_capture(connection.account_key.as_deref())?;
            let mw = MindWeaver::new(mw_bin)?;
            mw.binding()?; // Verify the local destination before collecting provider data.
            let (mut store, collected) = collect_google_captures(&connection, &resolved)?;
            let delivered = mw.deliver_pending(&mut store, limit)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"schemaVersion":"1","kind":"capture-cycle-result","collected":collected,"delivery":delivered})
                )?
            );
            capture_delivery_result(&delivered)?;
        }
        CapturesCommand::Reconcile {
            db_path,
            mw_bin,
            capture_id,
        } => {
            let mut store = CaptureStore::open_existing(db_path)?;
            let mw = MindWeaver::new(mw_bin)?;
            let report = mw.reconcile(&mut store, &capture_id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"schemaVersion":"1","kind":"capture-reconciliation-result","report":report})
                )?
            );
            capture_delivery_result(&report)?;
        }
    }
    Ok(())
}

#[derive(Debug, Subcommand)]
enum SyncCommand {
    /// Check the sync API `/v1/sync/state` endpoint.
    Doctor {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long, env = "HIVE_SYNC_API_URL")]
        endpoint: String,
        /// Device id used as the default Keychain account.
        #[arg(long, env = "HIVE_SYNC_DEVICE_ID")]
        device_id: Option<String>,
        #[command(flatten)]
        token_args: TokenArgs,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Register a device with the sync API.
    Register {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long, env = "HIVE_SYNC_API_URL")]
        endpoint: String,
        /// Device id to register.
        #[arg(long, env = "HIVE_SYNC_DEVICE_ID")]
        device_id: String,
        /// Human-readable device name.
        #[arg(long, default_value = "")]
        device_name: String,
        /// Platform label.
        #[arg(long, default_value = "")]
        platform: String,
        /// Client app version.
        #[arg(long, default_value = "hive-dev")]
        app_version: String,
        #[command(flatten)]
        token_args: TokenArgs,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Pull operations from the sync API by cursor.
    Pull {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long, env = "HIVE_SYNC_API_URL")]
        endpoint: String,
        /// Device id used as the default Keychain account.
        #[arg(long, env = "HIVE_SYNC_DEVICE_ID")]
        device_id: Option<String>,
        /// Cursor to pull after.
        #[arg(long, default_value = "0")]
        cursor: String,
        /// Maximum operations to pull.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[command(flatten)]
        token_args: TokenArgs,
    },
    /// Push operations from a JSON PushRequest file.
    PushFile {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long, env = "HIVE_SYNC_API_URL")]
        endpoint: String,
        /// Device id used as the default Keychain account.
        #[arg(long, env = "HIVE_SYNC_DEVICE_ID")]
        device_id: Option<String>,
        /// Path to a JSON file with the PushRequest shape.
        #[arg(long)]
        file: PathBuf,
        #[command(flatten)]
        token_args: TokenArgs,
    },
    /// Inspect local MindWeaver SQLite sync tables.
    LocalDoctor {
        /// Path to MindWeaver notes SQLite database.
        #[arg(long, env = "NOTES_DB_PATH")]
        db_path: PathBuf,
    },
    /// Run one desktop sync cycle using local MindWeaver SQLite sync tables.
    Run {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long, env = "HIVE_SYNC_API_URL")]
        endpoint: String,
        /// Path to MindWeaver notes SQLite database.
        #[arg(long, env = "NOTES_DB_PATH")]
        db_path: PathBuf,
        /// Device id.
        #[arg(long, env = "HIVE_SYNC_DEVICE_ID")]
        device_id: String,
        /// Human-readable device name.
        #[arg(long, default_value = "")]
        device_name: String,
        /// Platform label.
        #[arg(long, default_value = std::env::consts::OS)]
        platform: String,
        /// Client app version.
        #[arg(long, default_value = "hive-dev")]
        app_version: String,
        /// Max pending outbox rows to push.
        #[arg(long, default_value_t = 100)]
        outbox_limit: i64,
        /// Max remote operations to pull.
        #[arg(long, default_value_t = 100)]
        pull_limit: usize,
        /// Max retry attempts for transient sync failures per cycle.
        #[arg(long, env = "HIVE_SYNC_RETRY_ATTEMPTS", default_value_t = 3)]
        retry_attempts: usize,
        /// Initial retry backoff delay, e.g. 500ms, 2s, 1m.
        #[arg(long, env = "HIVE_SYNC_RETRY_BASE_DELAY", default_value = "500ms")]
        retry_base_delay: String,
        /// Maximum retry backoff delay, e.g. 5s, 1m.
        #[arg(long, env = "HIVE_SYNC_RETRY_MAX_DELAY", default_value = "5s")]
        retry_max_delay: String,
        /// Number of bounded sync cycles to run before exit.
        #[arg(long, env = "HIVE_SYNC_WORKER_ITERATIONS", default_value_t = 1)]
        worker_iterations: usize,
        /// Delay between worker sync cycles, e.g. 10s. Defaults to 15s for multi-cycle workers.
        #[arg(long, env = "HIVE_SYNC_WORKER_INTERVAL")]
        worker_interval: Option<String>,
        /// Run enough sync cycles to drain the currently pending local outbox.
        #[arg(long, env = "HIVE_SYNC_UNTIL_EMPTY")]
        until_empty: bool,
        /// Safety cap for --until-empty calculated sync cycles.
        #[arg(
            long,
            env = "HIVE_SYNC_UNTIL_EMPTY_MAX_ITERATIONS",
            default_value_t = 100
        )]
        until_empty_max_iterations: usize,
        #[command(flatten)]
        token_args: TokenArgs,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Review and export locally persisted sync conflicts.
    Conflicts {
        #[command(subcommand)]
        command: ConflictsCommand,
    },
    /// Review pulled remote note imports staged for manual Markdown import.
    Notes {
        #[command(subcommand)]
        command: NotesCommand,
    },
    /// Manage sync bearer token storage and validation.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
}

#[derive(Debug, Clone, Args)]
struct TokenArgs {
    /// Bearer token. Prefer env/secret tooling for real credentials.
    #[arg(long, env = "HIVE_SYNC_TOKEN")]
    token: Option<String>,
    /// Shell command that prints bearer token to stdout.
    #[arg(long, env = "HIVE_SYNC_TOKEN_COMMAND")]
    token_command: Option<String>,
    /// Load bearer token from macOS Keychain.
    #[arg(long, env = "HIVE_SYNC_TOKEN_FROM_KEYCHAIN")]
    token_from_keychain: bool,
    /// macOS Keychain service name for sync token.
    #[arg(
        long,
        env = "HIVE_SYNC_TOKEN_KEYCHAIN_SERVICE",
        default_value = DEFAULT_TOKEN_KEYCHAIN_SERVICE
    )]
    token_keychain_service: String,
    /// macOS Keychain account name for sync token (defaults to device-id).
    #[arg(long, env = "HIVE_SYNC_TOKEN_KEYCHAIN_ACCOUNT")]
    token_keychain_account: Option<String>,
}

#[derive(Debug, Subcommand)]
enum TokenCommand {
    /// Verify bearer token matches a device_id on the sync API.
    Check {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long, env = "HIVE_SYNC_API_URL")]
        endpoint: String,
        /// Device identifier to validate against bearer token.
        #[arg(long, env = "HIVE_SYNC_DEVICE_ID")]
        device_id: String,
        #[command(flatten)]
        token_args: TokenArgs,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Store bearer token in macOS Keychain.
    Store {
        /// Bearer token value to store.
        #[arg(long, env = "HIVE_SYNC_TOKEN")]
        token: Option<String>,
        /// Read bearer token from stdin.
        #[arg(long)]
        token_stdin: bool,
        /// Device identifier used as default keychain account.
        #[arg(long, env = "HIVE_SYNC_DEVICE_ID")]
        device_id: String,
        /// macOS Keychain service name.
        #[arg(
            long,
            env = "HIVE_SYNC_TOKEN_KEYCHAIN_SERVICE",
            default_value = DEFAULT_TOKEN_KEYCHAIN_SERVICE
        )]
        token_keychain_service: String,
        /// macOS Keychain account name (defaults to device-id).
        #[arg(long, env = "HIVE_SYNC_TOKEN_KEYCHAIN_ACCOUNT")]
        token_keychain_account: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ConflictsCommand {
    /// Review stale conflicts, optionally export artifacts and mark resolved.
    Review {
        /// Path to MindWeaver notes SQLite database.
        #[arg(long, env = "NOTES_DB_PATH")]
        db_path: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        /// Maximum conflicts to include.
        #[arg(long, default_value_t = 100)]
        limit: i64,
        /// Only include conflicts older than this many hours.
        #[arg(long, default_value_t = 168)]
        older_than_hours: i64,
        /// Include conflicts already marked resolved.
        #[arg(long)]
        include_resolved: bool,
        /// Write JSON review export to this exact file.
        #[arg(long)]
        export_file: Option<PathBuf>,
        /// Directory for autogenerated JSON review export file.
        #[arg(long)]
        export_dir: Option<PathBuf>,
        /// Directory for one JSON artifact per included conflict.
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
        /// Mark included unresolved conflicts resolved after successful export.
        #[arg(long)]
        mark_resolved: bool,
    },
}

#[derive(Debug, Subcommand)]
enum NotesCommand {
    /// Review staged remote note imports, optionally export inbox files and mark reviewed.
    Review {
        /// Path to MindWeaver notes SQLite database.
        #[arg(long, env = "NOTES_DB_PATH")]
        db_path: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        /// Maximum staged imports to include.
        #[arg(long, default_value_t = 100)]
        limit: i64,
        /// Include staged imports already marked reviewed.
        #[arg(long)]
        include_reviewed: bool,
        /// Directory for generated Markdown inbox files.
        #[arg(long)]
        export_dir: Option<PathBuf>,
        /// Mark included unreviewed imports reviewed after successful export.
        #[arg(long)]
        mark_reviewed: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Subcommand)]
enum PwaCommand {
    /// Run a package script in the local hive-pwa project.
    Run {
        #[arg(value_enum)]
        script: PwaScript,
    },
    /// Shortcut for `hive pwa run check`.
    Check,
    /// Shortcut for `hive pwa run build`.
    Build,
    /// Shortcut for `hive pwa run dev`.
    Dev,
    /// Shortcut for `hive pwa run preview`.
    Preview,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PwaScript {
    Check,
    Build,
    Dev,
    Preview,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ServeStore {
    Memory,
    Sqlite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RetryOptions {
    max_attempts: usize,
    base_delay: Duration,
    max_delay: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkerOptions {
    iterations: usize,
    interval: Duration,
    retry: RetryOptions,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "snake_case")]
struct DesktopWorkerResult {
    iterations_requested: usize,
    iterations_completed: usize,
    aggregate: hive_desktop::DesktopSyncResult,
    last: hive_desktop::DesktopSyncResult,
}

impl PwaScript {
    fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Build => "build",
            Self::Dev => "dev",
            Self::Preview => "preview",
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Commands::Doctor {
        json: false,
        root: None,
    }) {
        Commands::Version { short } => version_command(short),
        Commands::Calendar { command } => calendar::command(command),
        Commands::Tasks { command } => tasks::dispatch(move || tasks::command(command)).await,
        Commands::Captures { command } => {
            tokio::task::spawn_blocking(move || captures_command(command))
                .await
                .context("capture worker failed")?
        }
        Commands::Doctor { json, root } => doctor_command(json, root),
        Commands::Sync { command } => sync_command(command).await,
        Commands::Pwa { command } => pwa_command(command),
        Commands::Serve {
            addr,
            store,
            sqlite_path,
            device_tokens,
            permissive_cors,
        } => serve_command(addr, store, sqlite_path, device_tokens, permissive_cors).await,
    }
}

fn version_command(short: bool) -> Result<()> {
    if short {
        println!("{}", hive_core::VERSION);
    } else {
        println!("hive {}", hive_core::VERSION);
    }
    Ok(())
}

fn doctor_command(json: bool, root: Option<PathBuf>) -> Result<()> {
    let layout = ProjectLayout::discover(root)?;
    let report = hive_core::run_local_doctor(&layout);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Hive Mind doctor");
        for check in &report.checks {
            let marker = match check.status {
                DoctorStatus::Ok => "ok",
                DoctorStatus::Warn => "warn",
                DoctorStatus::Fail => "fail",
            };
            println!("{marker:>4}  {:<16} {}", check.name, check.message);
        }
    }

    if report.has_failures() {
        bail!("doctor found failing checks");
    }
    Ok(())
}

async fn sync_command(command: SyncCommand) -> Result<()> {
    match command {
        SyncCommand::Doctor {
            endpoint,
            device_id,
            token_args,
            json,
        } => {
            let token = resolve_sync_token(&token_args, device_id.as_deref().unwrap_or(""))?;
            let state = tokio::task::spawn_blocking(move || {
                hive_core::fetch_sync_state(&endpoint, token.as_deref())
            })
            .await??;
            if json {
                println!("{}", serde_json::to_string_pretty(&state)?);
            } else {
                println!("Hive sync API reachable");
                println!("server_time:   {}", state.server_time);
                println!("latest_cursor: {}", state.latest_cursor);
            }
            Ok(())
        }
        SyncCommand::Register {
            endpoint,
            device_id,
            device_name,
            platform,
            app_version,
            token_args,
            json,
        } => {
            let token = resolve_sync_token(&token_args, &device_id)?;
            let req = hive_core::DeviceRegistration {
                device_id,
                device_name,
                platform,
                app_version,
            };
            let resp = tokio::task::spawn_blocking(move || {
                hive_core::register_device(&endpoint, token.as_deref(), &req)
            })
            .await??;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                println!("registered: {}", resp.device_id);
            }
            Ok(())
        }
        SyncCommand::Pull {
            endpoint,
            device_id,
            cursor,
            limit,
            token_args,
        } => {
            let token = resolve_sync_token(&token_args, device_id.as_deref().unwrap_or(""))?;
            let resp = tokio::task::spawn_blocking(move || {
                hive_core::pull_operations(&endpoint, token.as_deref(), &cursor, limit)
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            Ok(())
        }
        SyncCommand::PushFile {
            endpoint,
            device_id,
            file,
            token_args,
        } => {
            let token = resolve_sync_token(&token_args, device_id.as_deref().unwrap_or(""))?;
            let resp = tokio::task::spawn_blocking(move || {
                let raw = std::fs::read_to_string(&file)
                    .with_context(|| format!("read push request {}", file.display()))?;
                let req: hive_core::PushRequest = serde_json::from_str(&raw)
                    .with_context(|| format!("decode push request {}", file.display()))?;
                hive_core::push_operations(&endpoint, token.as_deref(), &req)
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            Ok(())
        }
        SyncCommand::LocalDoctor { db_path } => {
            let report = tokio::task::spawn_blocking(move || {
                let store = hive_desktop::DesktopStore::open(&db_path)?;
                store.diagnostics()
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        SyncCommand::Run {
            endpoint,
            db_path,
            device_id,
            device_name,
            platform,
            app_version,
            outbox_limit,
            pull_limit,
            retry_attempts,
            retry_base_delay,
            retry_max_delay,
            worker_iterations,
            worker_interval,
            until_empty,
            until_empty_max_iterations,
            token_args,
            json,
        } => {
            let token = resolve_sync_token(&token_args, &device_id)?;
            let retry_base_delay = parse_duration_arg(&retry_base_delay, "retry-base-delay")?;
            let retry_max_delay = parse_duration_arg(&retry_max_delay, "retry-max-delay")?;
            let worker_interval = worker_interval
                .as_deref()
                .map(|value| parse_duration_arg(value, "worker-interval"))
                .transpose()?;
            let result = tokio::task::spawn_blocking(move || {
                let store = hive_desktop::DesktopStore::open(&db_path)?;
                let worker = build_worker_options(
                    &store,
                    outbox_limit,
                    retry_attempts,
                    retry_base_delay,
                    retry_max_delay,
                    worker_iterations,
                    worker_interval,
                    until_empty,
                    until_empty_max_iterations,
                )?;
                run_desktop_sync_worker(
                    &store,
                    &endpoint,
                    token.as_deref(),
                    &device_id,
                    &device_name,
                    &platform,
                    &app_version,
                    outbox_limit,
                    pull_limit,
                    worker,
                )
            })
            .await??;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print_desktop_worker_result(&result);
            }
            Ok(())
        }
        SyncCommand::Conflicts { command } => conflicts_command(command).await,
        SyncCommand::Notes { command } => notes_command(command).await,
        SyncCommand::Token { command } => token_command(command).await,
    }
}

fn build_worker_options(
    store: &hive_desktop::DesktopStore,
    outbox_limit: i64,
    retry_attempts: usize,
    retry_base_delay: Duration,
    retry_max_delay: Duration,
    worker_iterations: usize,
    worker_interval: Option<Duration>,
    until_empty: bool,
    until_empty_max_iterations: usize,
) -> Result<WorkerOptions> {
    let retry = normalize_retry_options(RetryOptions {
        max_attempts: retry_attempts,
        base_delay: retry_base_delay,
        max_delay: retry_max_delay,
    });
    let mut iterations = worker_iterations.max(1);
    let mut interval = worker_interval.unwrap_or_else(|| Duration::from_secs(15));

    if until_empty {
        let diagnostics = store
            .diagnostics()
            .context("read local sync diagnostics for until-empty")?;
        let outbox_limit = if outbox_limit <= 0 { 100 } else { outbox_limit };
        let calculated = if diagnostics.pending_outbox_count > 0 {
            ((diagnostics.pending_outbox_count + outbox_limit - 1) / outbox_limit) as usize
        } else {
            1
        };
        let max_iterations = until_empty_max_iterations.max(1);
        if calculated > max_iterations {
            bail!(
                "until-empty needs {} iteration(s) for {} pending op(s) at outbox limit {}; raise --until-empty-max-iterations to continue",
                calculated,
                diagnostics.pending_outbox_count,
                outbox_limit
            );
        }
        iterations = calculated.max(1);
        if worker_interval.is_none() {
            interval = Duration::ZERO;
        }
    }

    Ok(WorkerOptions {
        iterations,
        interval,
        retry,
    })
}

fn run_desktop_sync_worker(
    store: &hive_desktop::DesktopStore,
    endpoint: &str,
    token: Option<&str>,
    device_id: &str,
    device_name: &str,
    platform: &str,
    app_version: &str,
    outbox_limit: i64,
    pull_limit: usize,
    worker: WorkerOptions,
) -> Result<DesktopWorkerResult> {
    let mut result = DesktopWorkerResult {
        iterations_requested: worker.iterations,
        ..Default::default()
    };
    for index in 0..worker.iterations {
        let cycle = run_desktop_sync_with_retry(
            store,
            endpoint,
            token,
            device_id,
            device_name,
            platform,
            app_version,
            outbox_limit,
            pull_limit,
            worker.retry,
        )
        .with_context(|| format!("sync cycle {}/{}", index + 1, worker.iterations))?;
        result.iterations_completed += 1;
        result.last = cycle.clone();
        result.aggregate = combine_desktop_sync_results(&result.aggregate, &cycle);

        if index + 1 == worker.iterations || worker.interval.is_zero() {
            continue;
        }
        thread::sleep(worker.interval);
    }
    Ok(result)
}

fn run_desktop_sync_with_retry(
    store: &hive_desktop::DesktopStore,
    endpoint: &str,
    token: Option<&str>,
    device_id: &str,
    device_name: &str,
    platform: &str,
    app_version: &str,
    outbox_limit: i64,
    pull_limit: usize,
    retry: RetryOptions,
) -> Result<hive_desktop::DesktopSyncResult> {
    let retry = normalize_retry_options(retry);
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 1..=retry.max_attempts {
        match hive_desktop::run_desktop_sync_once(
            store,
            endpoint,
            token,
            device_id,
            device_name,
            platform,
            app_version,
            outbox_limit,
            pull_limit,
        ) {
            Ok(result) => return Ok(result),
            Err(err) => {
                let transient = is_transient_sync_error(&err);
                if !transient || attempt == retry.max_attempts {
                    return Err(err);
                }
                let delay = next_backoff_delay(retry, attempt);
                last_error = Some(err);
                thread::sleep(delay);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("sync retry loop exited unexpectedly")))
}

fn combine_desktop_sync_results(
    current: &hive_desktop::DesktopSyncResult,
    next: &hive_desktop::DesktopSyncResult,
) -> hive_desktop::DesktopSyncResult {
    hive_desktop::DesktopSyncResult {
        pushed_accepted: current.pushed_accepted + next.pushed_accepted,
        pushed_rejected: current.pushed_rejected + next.pushed_rejected,
        conflicts_logged: current.conflicts_logged + next.conflicts_logged,
        pulled_applied: current.pulled_applied + next.pulled_applied,
        pulled_note_operations_staged: current.pulled_note_operations_staged
            + next.pulled_note_operations_staged,
        pulled_note_operations_skipped: current.pulled_note_operations_skipped
            + next.pulled_note_operations_skipped,
        final_cursor: next.final_cursor.clone(),
    }
}

fn normalize_retry_options(mut retry: RetryOptions) -> RetryOptions {
    if retry.max_attempts == 0 {
        retry.max_attempts = 3;
    }
    if retry.base_delay.is_zero() {
        retry.base_delay = Duration::from_millis(500);
    }
    if retry.max_delay.is_zero() {
        retry.max_delay = Duration::from_secs(5);
    }
    if retry.max_delay < retry.base_delay {
        retry.max_delay = retry.base_delay;
    }
    retry
}

fn next_backoff_delay(retry: RetryOptions, attempt: usize) -> Duration {
    let shift = attempt.saturating_sub(1).min(20) as u32;
    let factor = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
    retry.base_delay.saturating_mul(factor).min(retry.max_delay)
}

fn is_transient_sync_error(err: &anyhow::Error) -> bool {
    let message = err
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    (429..=429).any(|status| message.contains(&format!("status {status}")))
        || (500..=599).any(|status| message.contains(&format!("status {status}")))
        || [
            "connection refused",
            "connection reset",
            "broken pipe",
            "timed out",
            "timeout",
            "temporary failure",
            "unexpected eof",
            "eof",
        ]
        .iter()
        .any(|needle| message.contains(needle))
}

fn parse_duration_arg(raw: &str, name: &str) -> Result<Duration> {
    parse_duration(raw).with_context(|| {
        format!("parse --{name} duration {raw:?}; expected e.g. 500ms, 2s, 1m, 1h")
    })
}

fn parse_duration(raw: &str) -> Result<Duration> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("duration is required");
    }
    let split_at = raw
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(raw.len());
    let (number, unit) = raw.split_at(split_at);
    if number.is_empty() {
        bail!("duration must start with a number");
    }
    let value = number.parse::<u64>().context("parse duration number")?;
    match unit.trim().to_ascii_lowercase().as_str() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => Ok(Duration::from_secs(value)),
        "ms" | "msec" | "msecs" | "millisecond" | "milliseconds" => {
            Ok(Duration::from_millis(value))
        }
        "m" | "min" | "mins" | "minute" | "minutes" => Ok(Duration::from_secs(value * 60)),
        "h" | "hr" | "hrs" | "hour" | "hours" => Ok(Duration::from_secs(value * 60 * 60)),
        _ => bail!("unsupported duration unit {unit:?}"),
    }
}

fn print_desktop_worker_result(result: &DesktopWorkerResult) {
    println!(
        "worker completed:     {}/{}",
        result.iterations_completed, result.iterations_requested
    );
    println!("pushed accepted:      {}", result.aggregate.pushed_accepted);
    println!("pushed rejected:      {}", result.aggregate.pushed_rejected);
    println!(
        "conflicts logged:     {}",
        result.aggregate.conflicts_logged
    );
    println!("pulled applied:       {}", result.aggregate.pulled_applied);
    println!(
        "pulled notes staged:  {}",
        result.aggregate.pulled_note_operations_staged
    );
    println!(
        "pulled notes skipped: {}",
        result.aggregate.pulled_note_operations_skipped
    );
    println!("final cursor:         {}", result.aggregate.final_cursor);
}

async fn token_command(command: TokenCommand) -> Result<()> {
    match command {
        TokenCommand::Check {
            endpoint,
            device_id,
            token_args,
            format,
        } => {
            let token = resolve_sync_token(&token_args, &device_id)?
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "token is required (set --token, --token-command, or --token-from-keychain)"
                    )
                })?;
            let report = tokio::task::spawn_blocking(move || {
                hive_core::check_token_for_device(&endpoint, &token, &device_id)
            })
            .await??;
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
                OutputFormat::Text => {
                    println!("sync token check");
                    println!("endpoint:    {}", report.endpoint);
                    println!("device_id:   {}", report.device_id);
                    println!("http status: {}", report.http_status);
                    println!("status:      {}", report.status);
                    if !report.message.trim().is_empty() {
                        println!("message:     {}", report.message);
                    }
                }
            }
            if !report.registered {
                bail!(
                    "token check failed for device_id={:?} (status={} http={})",
                    report.device_id,
                    report.status,
                    report.http_status
                );
            }
            Ok(())
        }
        TokenCommand::Store {
            token,
            token_stdin,
            device_id,
            token_keychain_service,
            token_keychain_account,
        } => {
            let token = resolve_store_token(token, token_stdin)?;
            let service = default_token_keychain_service(&token_keychain_service);
            let account = token_keychain_account
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(device_id.trim())
                .to_string();
            if account.is_empty() {
                bail!("keychain account is required (set --token-keychain-account or --device-id)");
            }
            tokio::task::spawn_blocking({
                let service = service.clone();
                let account = account.clone();
                move || store_token_in_keychain(&service, &account, &token)
            })
            .await??;
            println!(
                "stored sync token in Keychain service={:?} account={:?}",
                service, account
            );
            println!(
                "use with: hive sync run --device-id {:?} --token-from-keychain",
                account
            );
            Ok(())
        }
    }
}

fn resolve_sync_token(args: &TokenArgs, device_id: &str) -> Result<Option<String>> {
    resolve_sync_token_with_runner(
        args.token.as_deref(),
        args.token_command.as_deref(),
        args.token_from_keychain,
        &args.token_keychain_service,
        args.token_keychain_account.as_deref(),
        device_id,
        run_command_capture,
    )
}

fn resolve_sync_token_with_runner<F>(
    raw_token: Option<&str>,
    token_command: Option<&str>,
    token_from_keychain: bool,
    keychain_service: &str,
    keychain_account: Option<&str>,
    device_id: &str,
    mut runner: F,
) -> Result<Option<String>>
where
    F: FnMut(&str, &[&str]) -> Result<Vec<u8>>,
{
    if let Some(token) = trim_non_empty(raw_token) {
        return Ok(Some(token.to_string()));
    }

    let token_command = trim_non_empty(token_command);
    if token_from_keychain && token_command.is_some() {
        bail!("choose only one token source: --token-command or --token-from-keychain");
    }

    if token_from_keychain {
        if !cfg!(target_os = "macos") {
            bail!("--token-from-keychain is only supported on macOS");
        }
        let service = default_token_keychain_service(keychain_service);
        let account = keychain_account
            .and_then(|value| trim_non_empty(Some(value)))
            .unwrap_or_else(|| device_id.trim())
            .trim()
            .to_string();
        if account.is_empty() {
            bail!(
                "token keychain account is required (set --token-keychain-account or --device-id)"
            );
        }
        return fetch_token_from_keychain_with_runner(&service, &account, runner).map(Some);
    }

    if let Some(command) = token_command {
        let output = runner("/bin/sh", &["-c", command]).context("token-command failed")?;
        let token = String::from_utf8_lossy(&output).trim().to_string();
        if token.is_empty() {
            bail!("token-command produced empty output");
        }
        return Ok(Some(token));
    }

    Ok(None)
}

fn fetch_token_from_keychain_with_runner<F>(
    service: &str,
    account: &str,
    mut runner: F,
) -> Result<String>
where
    F: FnMut(&str, &[&str]) -> Result<Vec<u8>>,
{
    let output = runner(
        "security",
        &["find-generic-password", "-w", "-s", service, "-a", account],
    )
    .with_context(|| format!("read token from keychain service={service:?} account={account:?}"))?;
    let token = String::from_utf8_lossy(&output).trim().to_string();
    if token.is_empty() {
        bail!("empty token from keychain service={service:?} account={account:?}");
    }
    Ok(token)
}

fn store_token_in_keychain(service: &str, account: &str, token: &str) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("keychain token storage is only supported on macOS");
    }
    let token = token.trim();
    if token.is_empty() {
        bail!("token is required");
    }
    run_command_capture(
        "security",
        &[
            "add-generic-password",
            "-U",
            "-s",
            service,
            "-a",
            account,
            "-w",
            token,
        ],
    )
    .with_context(|| format!("store token in keychain service={service:?} account={account:?}"))?;
    Ok(())
}

fn run_command_capture(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("run {program}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let combined = match (stdout.is_empty(), stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => stdout,
            (true, false) => stderr,
            (false, false) => format!("{stdout}\n{stderr}"),
        };
        bail!(
            "{program} failed with status {} (output: {combined})",
            output.status
        );
    }
    Ok(output.stdout)
}

fn resolve_store_token(token: Option<String>, token_stdin: bool) -> Result<String> {
    let mut token = token.unwrap_or_default().trim().to_string();
    if token_stdin {
        let mut stdin_token = String::new();
        std::io::stdin()
            .read_to_string(&mut stdin_token)
            .context("read token from stdin")?;
        token = stdin_token.trim().to_string();
    }
    if token.is_empty() {
        bail!("token is required (set --token or --token-stdin)");
    }
    Ok(token)
}

fn default_token_keychain_service(value: &str) -> String {
    if value.trim().is_empty() {
        DEFAULT_TOKEN_KEYCHAIN_SERVICE.to_string()
    } else {
        value.trim().to_string()
    }
}

fn trim_non_empty<'a>(value: Option<&'a str>) -> Option<&'a str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

async fn conflicts_command(command: ConflictsCommand) -> Result<()> {
    match command {
        ConflictsCommand::Review {
            db_path,
            format,
            limit,
            older_than_hours,
            include_resolved,
            export_file,
            export_dir,
            artifacts_dir,
            mark_resolved,
        } => {
            let report = tokio::task::spawn_blocking(move || {
                let store = hive_desktop::DesktopStore::open(&db_path)?;
                store.review_conflicts(hive_desktop::ConflictReviewOptions {
                    limit,
                    older_than_seconds: older_than_hours * 60 * 60,
                    include_resolved,
                    export_file,
                    export_dir,
                    artifacts_dir,
                    mark_resolved,
                })
            })
            .await??;
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
                OutputFormat::Text => print_conflict_review_text(&report),
            }
            Ok(())
        }
    }
}

async fn notes_command(command: NotesCommand) -> Result<()> {
    match command {
        NotesCommand::Review {
            db_path,
            format,
            limit,
            include_reviewed,
            export_dir,
            mark_reviewed,
        } => {
            let report = tokio::task::spawn_blocking(move || {
                let store = hive_desktop::DesktopStore::open(&db_path)?;
                store.review_note_imports(hive_desktop::NoteImportReviewOptions {
                    limit,
                    include_reviewed,
                    export_dir,
                    mark_reviewed,
                })
            })
            .await??;
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
                OutputFormat::Text => print_note_import_review_text(&report),
            }
            Ok(())
        }
    }
}

fn print_note_import_review_text(report: &hive_desktop::NoteImportReviewReport) {
    println!("sync note import review ({})", report.generated_at);
    println!("include reviewed: {}", report.reviewed_only);
    println!("limit:            {}", report.limit);
    println!("imports:          {}", report.import_count);
    if !report.export_dir.trim().is_empty() {
        println!("export dir:       {}", report.export_dir);
    }
    if !report.export_files.is_empty() {
        println!("exported files:   {}", report.export_files.len());
    }
    if report.reviewed_count > 0 {
        println!("marked reviewed:  {}", report.reviewed_count);
    }
    println!();
    println!("Entries");
    if report.imports.is_empty() {
        println!("  - none");
        return;
    }
    for row in &report.imports {
        let status = if row.reviewed_at.as_deref().unwrap_or("").trim().is_empty() {
            "unreviewed"
        } else {
            "reviewed"
        };
        println!(
            "  - id={} op={} type={} path={} title={} received={} status={}",
            row.id, row.op_id, row.op_type, row.entity_key, row.title, row.received_at, status
        );
    }
}

fn print_conflict_review_text(report: &hive_desktop::ConflictReviewReport) {
    println!("sync conflict review ({})", report.generated_at);
    println!(
        "cutoff:      {} (older than {})",
        report.cutoff_time, report.older_than
    );
    println!("unresolved:  {}", report.unresolved_only);
    println!("limit:       {}", report.limit);
    println!("conflicts:   {}", report.conflict_count);
    if !report.export_file.trim().is_empty() {
        println!("export file: {}", report.export_file);
    }
    if !report.artifact_files.is_empty() {
        println!("artifacts:   {}", report.artifact_files.len());
    }
    if report.resolved_count > 0 {
        println!("resolved:    {}", report.resolved_count);
    }
    println!();
    println!("Entries");
    if report.conflicts.is_empty() {
        println!("  - none");
        return;
    }
    for conflict in &report.conflicts {
        let status = if conflict
            .resolved_at
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            "unresolved"
        } else {
            "resolved"
        };
        println!(
            "  - id={} type={} key={} reason={} created={} status={}",
            conflict.id,
            conflict.entity_type,
            conflict.entity_key,
            conflict.reason,
            conflict.created_at,
            status
        );
    }
}

fn pwa_command(command: PwaCommand) -> Result<()> {
    let script = match command {
        PwaCommand::Run { script } => script,
        PwaCommand::Check => PwaScript::Check,
        PwaCommand::Build => PwaScript::Build,
        PwaCommand::Dev => PwaScript::Dev,
        PwaCommand::Preview => PwaScript::Preview,
    };
    run_pwa_script(script)
}

fn run_pwa_script(script: PwaScript) -> Result<()> {
    let layout = ProjectLayout::discover(None)?;
    if !layout.pwa_dir.join("package.json").is_file() {
        bail!(
            "hive-pwa/package.json not found at {}",
            layout.pwa_dir.display()
        );
    }

    let status = Command::new("npm")
        .arg("run")
        .arg(script.as_str())
        .current_dir(&layout.pwa_dir)
        .status()
        .with_context(|| format!("run npm script {}", script.as_str()))?;

    if !status.success() {
        bail!("npm run {} failed with status {status}", script.as_str());
    }
    Ok(())
}

async fn serve_command(
    addr: String,
    store: ServeStore,
    sqlite_path: Option<PathBuf>,
    device_tokens: Option<String>,
    permissive_cors: bool,
) -> Result<()> {
    let device_tokens = match device_tokens {
        Some(raw) => hive_server::parse_device_tokens(&raw)?,
        None => Default::default(),
    };
    let store = match store {
        ServeStore::Memory => hive_server::StoreConfig::Memory,
        ServeStore::Sqlite => {
            let path = sqlite_path
                .ok_or_else(|| anyhow::anyhow!("--sqlite-path is required with --store sqlite"))?;
            hive_server::StoreConfig::Sqlite {
                path: path.display().to_string(),
            }
        }
    };
    println!("hive-sync-api listening on {addr}");
    hive_server::serve(hive_server::ServerConfig {
        addr,
        device_tokens,
        permissive_cors,
        store,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_sync_token_uses_explicit_token() {
        let mut called = false;
        let token = resolve_sync_token_with_runner(
            Some(" abc123 "),
            None,
            false,
            "",
            None,
            "desktop",
            |_program, _args| {
                called = true;
                Ok(Vec::new())
            },
        )
        .expect("resolve token");

        assert_eq!(token.as_deref(), Some("abc123"));
        assert!(!called, "runner should not be called for explicit token");
    }

    #[test]
    fn resolve_sync_token_uses_token_command() {
        let token = resolve_sync_token_with_runner(
            None,
            Some("echo token-from-command"),
            false,
            "",
            None,
            "desktop",
            |program, args| {
                assert_eq!(program, "/bin/sh");
                assert_eq!(args, &["-c", "echo token-from-command"]);
                Ok(b"token-from-command\n".to_vec())
            },
        )
        .expect("resolve token from command");

        assert_eq!(token.as_deref(), Some("token-from-command"));
    }

    #[test]
    fn resolve_sync_token_rejects_command_and_keychain_conflict() {
        let err = resolve_sync_token_with_runner(
            None,
            Some("echo token"),
            true,
            "",
            None,
            "desktop",
            |_program, _args| Ok(Vec::new()),
        )
        .expect_err("expected conflicting token source error");

        assert!(err.to_string().contains("choose only one token source"));
    }

    #[test]
    fn resolve_sync_token_propagates_command_error() {
        let err = resolve_sync_token_with_runner(
            None,
            Some("bad-cmd"),
            false,
            "",
            None,
            "desktop",
            |_program, _args| Err(anyhow::anyhow!("exit status 1 (output: boom)")),
        )
        .expect_err("expected token command error");

        assert!(err.to_string().contains("token-command failed"));
    }

    #[test]
    fn resolve_sync_token_uses_keychain() {
        if !cfg!(target_os = "macos") {
            return;
        }

        let token = resolve_sync_token_with_runner(
            None,
            None,
            true,
            "mw/hive-sync",
            Some("desktop"),
            "desktop",
            |program, args| {
                assert_eq!(program, "security");
                assert_eq!(
                    args,
                    &[
                        "find-generic-password",
                        "-w",
                        "-s",
                        "mw/hive-sync",
                        "-a",
                        "desktop"
                    ]
                );
                Ok(b"token-from-keychain\n".to_vec())
            },
        )
        .expect("resolve token from keychain");

        assert_eq!(token.as_deref(), Some("token-from-keychain"));
    }

    #[test]
    fn parse_duration_supports_common_units() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("3m").unwrap(), Duration::from_secs(180));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn normalize_retry_options_fills_defaults_and_caps_max_delay() {
        let retry = normalize_retry_options(RetryOptions {
            max_attempts: 0,
            base_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(1),
        });

        assert_eq!(retry.max_attempts, 3);
        assert_eq!(retry.max_delay, Duration::from_secs(10));
    }

    #[test]
    fn next_backoff_delay_doubles_until_max() {
        let retry = RetryOptions {
            max_attempts: 4,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(250),
        };

        assert_eq!(next_backoff_delay(retry, 1), Duration::from_millis(100));
        assert_eq!(next_backoff_delay(retry, 2), Duration::from_millis(200));
        assert_eq!(next_backoff_delay(retry, 3), Duration::from_millis(250));
    }

    #[test]
    fn detects_transient_sync_errors() {
        assert!(is_transient_sync_error(&anyhow::anyhow!(
            "push request failed: status 503 body=busy"
        )));
        assert!(is_transient_sync_error(&anyhow::anyhow!(
            "connection refused"
        )));
        assert!(!is_transient_sync_error(&anyhow::anyhow!(
            "push request failed: status 403 body=forbidden"
        )));
    }
}
