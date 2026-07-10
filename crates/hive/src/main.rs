use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use hive_core::{DoctorStatus, ProjectLayout};

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
        /// Comma-separated device token map: desktop=token,phone=token.
        #[arg(long, env = "HIVE_SYNC_DEVICE_TOKENS")]
        device_tokens: Option<String>,
        /// Allow permissive CORS for browser/PWA development.
        #[arg(long, env = "HIVE_SYNC_PERMISSIVE_CORS")]
        permissive_cors: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SyncCommand {
    /// Check the sync API `/v1/sync/state` endpoint.
    Doctor {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long)]
        endpoint: String,
        /// Bearer token. Prefer env/secret tooling for real credentials.
        #[arg(long, env = "HIVE_SYNC_TOKEN")]
        token: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Register a device with the sync API.
    Register {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long)]
        endpoint: String,
        /// Device id to register.
        #[arg(long)]
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
        /// Bearer token. Prefer env/secret tooling for real credentials.
        #[arg(long, env = "HIVE_SYNC_TOKEN")]
        token: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Pull operations from the sync API by cursor.
    Pull {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long)]
        endpoint: String,
        /// Cursor to pull after.
        #[arg(long, default_value = "0")]
        cursor: String,
        /// Maximum operations to pull.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Bearer token. Prefer env/secret tooling for real credentials.
        #[arg(long, env = "HIVE_SYNC_TOKEN")]
        token: Option<String>,
    },
    /// Push operations from a JSON PushRequest file.
    PushFile {
        /// Sync API base URL, e.g. http://127.0.0.1:8080.
        #[arg(long)]
        endpoint: String,
        /// Path to a JSON file with the PushRequest shape.
        #[arg(long)]
        file: PathBuf,
        /// Bearer token. Prefer env/secret tooling for real credentials.
        #[arg(long, env = "HIVE_SYNC_TOKEN")]
        token: Option<String>,
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
        #[arg(long)]
        endpoint: String,
        /// Path to MindWeaver notes SQLite database.
        #[arg(long, env = "NOTES_DB_PATH")]
        db_path: PathBuf,
        /// Device id.
        #[arg(long)]
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
        /// Bearer token. Prefer env/secret tooling for real credentials.
        #[arg(long, env = "HIVE_SYNC_TOKEN")]
        token: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
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
        Commands::Doctor { json, root } => doctor_command(json, root),
        Commands::Sync { command } => sync_command(command).await,
        Commands::Pwa { command } => pwa_command(command),
        Commands::Serve {
            addr,
            device_tokens,
            permissive_cors,
        } => serve_command(addr, device_tokens, permissive_cors).await,
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
            token,
            json,
        } => {
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
            token,
            json,
        } => {
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
            cursor,
            limit,
            token,
        } => {
            let resp = tokio::task::spawn_blocking(move || {
                hive_core::pull_operations(&endpoint, token.as_deref(), &cursor, limit)
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            Ok(())
        }
        SyncCommand::PushFile {
            endpoint,
            file,
            token,
        } => {
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
            token,
            json,
        } => {
            let result = tokio::task::spawn_blocking(move || {
                let store = hive_desktop::DesktopStore::open(&db_path)?;
                hive_desktop::run_desktop_sync_once(
                    &store,
                    &endpoint,
                    token.as_deref(),
                    &device_id,
                    &device_name,
                    &platform,
                    &app_version,
                    outbox_limit,
                    pull_limit,
                )
            })
            .await??;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("pushed accepted:      {}", result.pushed_accepted);
                println!("pushed rejected:      {}", result.pushed_rejected);
                println!("conflicts logged:     {}", result.conflicts_logged);
                println!("pulled applied:       {}", result.pulled_applied);
                println!(
                    "pulled notes skipped: {}",
                    result.pulled_note_operations_skipped
                );
                println!("final cursor:         {}", result.final_cursor);
            }
            Ok(())
        }
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
    device_tokens: Option<String>,
    permissive_cors: bool,
) -> Result<()> {
    let device_tokens = match device_tokens {
        Some(raw) => hive_server::parse_device_tokens(&raw)?,
        None => Default::default(),
    };
    println!("hive-sync-api listening on {addr}");
    hive_server::serve(hive_server::ServerConfig {
        addr,
        device_tokens,
        permissive_cors,
    })
    .await
}
