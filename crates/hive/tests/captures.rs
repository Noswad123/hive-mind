use hive_capture::{
    BootstrapPolicy, DeliveryState, DestinationBinding, GoogleTask, store::CaptureStore,
};
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

struct Sandbox {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for child in ["home", "notes", "data", "state", "config"] {
            fs::create_dir(root.join(child)).unwrap();
        }
        fs::write(
            root.join("config.toml"),
            format!(
                "notes_dir = {}\ninbox_path = {}\ndashboard_path = {}\ndb_path = {}\n",
                root.join("notes").display(),
                root.join("notes/inbox.md").display(),
                root.join("notes/dashboard.md").display(),
                root.join("must-not-create/notes.db").display()
            ),
        )
        .unwrap();
        Self { _dir: dir, root }
    }

    fn command(&self, binary: &str, args: &[&str]) -> Command {
        let mut command = Command::new(binary);
        command
            .args(args)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env("MW_CONFIG", self.root.join("config.toml"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .current_dir(&self.root);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_hive"), args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn queue(&self) -> PathBuf {
        self.root.join("captures.sqlite")
    }
}

fn task(id: &str, title: &str) -> GoogleTask {
    GoogleTask {
        id: id.into(),
        title: title.into(),
        status: "needsAction".into(),
        ..Default::default()
    }
}

#[test]
fn help_and_read_only_status_do_not_create_a_queue() {
    let s = Sandbox::new();
    assert!(s.run(&["captures", "--help"]).status.success());
    assert!(s.run(&["captures", "poll", "--help"]).status.success());
    let status = s.run(&[
        "captures",
        "status",
        "--db-path",
        s.queue().to_str().unwrap(),
    ]);
    assert!(!status.status.success());
    assert!(!s.queue().exists());
    assert!(!s.root.join("notes/inbox.md").exists());
}

#[test]
fn invalid_token_names_and_values_fail_without_network_or_writer() {
    let s = Sandbox::new();
    let queue = s.queue();
    for name in [
        "LANG",
        "PATH",
        "HOME",
        "MW_CONFIG",
        "NOTES_DIR",
        "token-value",
        "HIVE_GOOGLE_BAD\nNAME",
    ] {
        let output = s.run(&[
            "captures",
            "run",
            "--db-path",
            queue.to_str().unwrap(),
            "--account-key",
            "personal",
            "--list-id",
            "list",
            "--bootstrap",
            "import-open",
            "--access-token-env",
            name,
            "--mw-bin",
            "/missing-mw",
        ]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("HIVE_GOOGLE_"));
        assert!(!queue.exists());
    }
    let mut command = s.command(
        env!("CARGO_BIN_EXE_hive"),
        &[
            "captures",
            "lists",
            "--access-token-env",
            "HIVE_GOOGLE_ACCESS_TOKEN",
        ],
    );
    command.env("HIVE_GOOGLE_ACCESS_TOKEN", "private-token\nDo-Not-Log");
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid Google access token"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-token"));
    assert!(!queue.exists());
}

#[test]
fn missing_authorization_and_bootstrap_policy_have_no_filesystem_effects() {
    let s = Sandbox::new();
    let output = s.run(&[
        "captures",
        "poll",
        "--db-path",
        s.queue().to_str().unwrap(),
        "--account-key",
        "personal",
        "--list-id",
        "list",
        "--bootstrap",
        "baseline",
        "--access-token-env",
        "HIVE_GOOGLE_ACCESS_TOKEN",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("access token is unavailable"));
    let output = s.run(&[
        "captures",
        "poll",
        "--db-path",
        s.queue().to_str().unwrap(),
        "--account-key",
        "personal",
        "--list-id",
        "list",
        "--access-token-env",
        "HIVE_GOOGLE_ACCESS_TOKEN",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--bootstrap"));
    assert!(!s.queue().exists());
}

#[test]
fn unsupported_and_calendar_work_cannot_look_like_an_empty_successful_delivery() {
    let s = Sandbox::new();
    let mut unsupported = task("with-notes", "Preserve description");
    unsupported.notes = Some("important context".into());
    let mut dated = task("dated", "Needs calendar routing");
    dated.due = Some("2026-11-01T00:00:00.000Z".into());
    let mut store = CaptureStore::open(s.queue()).unwrap();
    store
        .ingest(
            "personal",
            "list",
            &[unsupported, dated],
            BootstrapPolicy::ImportOpen,
        )
        .unwrap();
    drop(store);
    let output = s.run(&[
        "captures",
        "deliver",
        "--db-path",
        s.queue().to_str().unwrap(),
        "--mw-bin",
        env!("CARGO_BIN_EXE_hive"),
    ]);
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["report"]["unsupported"], 1);
    assert_eq!(report["report"]["calendarPending"], 1);
    assert_eq!(report["report"]["blocked"], 1);
    assert_eq!(report["report"]["delivered"], 0);
    assert!(!s.root.join("notes/inbox.md").exists());
}

#[test]
fn queue_status_is_read_only_and_pending_state_survives_restart() {
    let s = Sandbox::new();
    let mut store = CaptureStore::open(s.queue()).unwrap();
    store
        .ingest(
            "personal",
            "list",
            &[task("one", "Same title"), task("two", "Same title")],
            BootstrapPolicy::ImportOpen,
        )
        .unwrap();
    drop(store);
    let before = fs::read(s.queue()).unwrap();
    let status = s.ok(&[
        "captures",
        "status",
        "--db-path",
        s.queue().to_str().unwrap(),
    ]);
    assert_eq!(status["kind"], "capture-queue-status");
    assert_eq!(status["items"].as_array().unwrap().len(), 2);
    assert_eq!(status["items"][0]["state"], "pending");
    assert_ne!(
        status["items"][0]["captureId"],
        status["items"][1]["captureId"]
    );
    assert_eq!(fs::read(s.queue()).unwrap(), before);
}

#[test]
fn invalid_delivery_limits_and_unrelated_databases_are_not_modified() {
    let s = Sandbox::new();
    let output = s.run(&[
        "captures",
        "deliver",
        "--db-path",
        s.queue().to_str().unwrap(),
        "--mw-bin",
        "/missing",
        "--limit",
        "0",
    ]);
    assert!(!output.status.success());
    assert!(!s.queue().exists());
    fs::write(s.queue(), "not a capture database").unwrap();
    let output = s.run(&[
        "captures",
        "deliver",
        "--db-path",
        s.queue().to_str().unwrap(),
        "--mw-bin",
        "/missing",
    ]);
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(s.queue()).unwrap(),
        "not a capture database"
    );
}

#[test]
fn writable_commands_recover_a_hot_queue_without_retrying_unknown_claims() {
    for action in ["deliver", "reconcile", "recover-store"] {
        let s = Sandbox::new();
        let mut store = CaptureStore::open(s.queue()).unwrap();
        store
            .ingest(
                "personal",
                "list",
                &[task("interrupted", "Retain claim")],
                BootstrapPolicy::ImportOpen,
            )
            .unwrap();
        let job = store.jobs().unwrap().remove(0);
        let binding = DestinationBinding {
            notes_dir: s.root.join("notes").to_str().unwrap().into(),
            inbox_path: s.root.join("notes/inbox.md").to_str().unwrap().into(),
            dashboard_path: s.root.join("notes/dashboard.md").to_str().unwrap().into(),
        };
        store.claim(&job.capture_id, &binding).unwrap();
        drop(store);
        // Reuse the explicitly gated integration-test process as a crash fixture.
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "real_mw_delivery_and_ambiguous_outcome_reconciliation_never_duplicate",
                "--ignored",
            ])
            .env("HIVE_CAPTURE_CLI_HOT_DB", s.queue())
            .output()
            .unwrap();
        assert_eq!(
            child.status.code(),
            Some(73),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        let journal = PathBuf::from(format!("{}-journal", s.queue().display()));
        assert!(journal.metadata().unwrap().len() > 512);
        let status = s.run(&[
            "captures",
            "status",
            "--db-path",
            s.queue().to_str().unwrap(),
        ]);
        assert!(!status.status.success()); // Read-only status must not recover or delete it.
        assert!(journal.exists());
        let queue = s.queue();
        let mut args = vec!["captures", action, "--db-path", queue.to_str().unwrap()];
        if action != "recover-store" {
            args.extend(["--mw-bin", env!("CARGO_BIN_EXE_hive")]);
        }
        if action == "reconcile" {
            args.extend(["--capture-id", &job.capture_id]);
        }
        let output = s.run(&args);
        assert_eq!(
            output.status.success(),
            action == "recover-store",
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let _: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!journal.exists());
        let jobs = CaptureStore::inspect(s.queue()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].attempts, 1);
        assert_eq!(jobs[0].title, "Retain claim");
        assert!(jobs[0].receipt.is_none());
        assert_eq!(
            jobs[0].state,
            if action == "reconcile" {
                DeliveryState::NeedsReview
            } else {
                DeliveryState::Delivering
            }
        );
        assert!(!s.root.join("notes/inbox.md").exists());
    }
}

#[test]
#[ignore = "requires an explicit MW_TEST_BIN; uses only sandbox notes and configuration"]
fn real_mw_delivery_and_ambiguous_outcome_reconciliation_never_duplicate() {
    if let Some(path) = std::env::var_os("HIVE_CAPTURE_CLI_HOT_DB") {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA cache_size=1; BEGIN IMMEDIATE; CREATE TABLE crash_spill(data BLOB);").unwrap();
        for _ in 0..32 {
            conn.execute("INSERT INTO crash_spill VALUES (zeroblob(65536))", [])
                .unwrap();
        }
        std::process::exit(73); // Deliberately bypass rollback/destructors in the fixture child.
    }
    let mw = std::env::var("MW_TEST_BIN")
        .expect("set MW_TEST_BIN to a trusted compatible absolute mw binary");
    assert!(std::path::Path::new(&mw).is_absolute());
    let s = Sandbox::new();
    let tasks = [
        task("phone-one", "Same title"),
        task("phone-two", "Same title"),
    ];
    let mut store = CaptureStore::open(s.queue()).unwrap();
    store
        .ingest("personal", "list", &tasks, BootstrapPolicy::ImportOpen)
        .unwrap();
    drop(store); // Pending delivery survives the desktop being stopped.
    let delivered = s.ok(&[
        "captures",
        "deliver",
        "--db-path",
        s.queue().to_str().unwrap(),
        "--mw-bin",
        &mw,
    ]);
    assert_eq!(delivered["report"]["delivered"], 2);
    let inbox = s.root.join("notes/inbox.md");
    let source = fs::read(&inbox).unwrap();
    let projection = fs::read(s.root.join("notes/dashboard.md")).unwrap();
    let mut store = CaptureStore::open(s.queue()).unwrap();
    store
        .ingest("personal", "list", &tasks, BootstrapPolicy::ImportOpen)
        .unwrap();
    drop(store);
    let repeated = s.ok(&[
        "captures",
        "deliver",
        "--db-path",
        s.queue().to_str().unwrap(),
        "--mw-bin",
        &mw,
    ]);
    assert_eq!(repeated["report"]["delivered"], 0);
    assert_eq!(fs::read(&inbox).unwrap(), source);
    assert_eq!(
        fs::read(s.root.join("notes/dashboard.md")).unwrap(),
        projection
    );

    let mut store = CaptureStore::open(s.queue()).unwrap();
    let mut more = tasks.to_vec();
    more.push(task("interrupted", "Source committed before receipt"));
    store
        .ingest("personal", "list", &more, BootstrapPolicy::ImportOpen)
        .unwrap();
    let candidate = store
        .jobs()
        .unwrap()
        .into_iter()
        .find(|j| j.provider_id == "interrupted")
        .unwrap();
    let binding = DestinationBinding {
        notes_dir: s.root.join("notes").to_str().unwrap().into(),
        inbox_path: inbox.to_str().unwrap().into(),
        dashboard_path: s.root.join("notes/dashboard.md").to_str().unwrap().into(),
    };
    store.claim(&candidate.capture_id, &binding).unwrap();
    drop(store);
    let write = s
        .command(
            &mw,
            &[
                "todos",
                "add",
                "--capture-id",
                &candidate.capture_id,
                "--json",
                "--",
                &candidate.title,
            ],
        )
        .output()
        .unwrap();
    assert!(
        write.status.success(),
        "{}",
        String::from_utf8_lossy(&write.stderr)
    );
    let source_after_commit = fs::read(&inbox).unwrap();
    assert!(
        !s.run(&[
            "captures",
            "deliver",
            "--db-path",
            s.queue().to_str().unwrap(),
            "--mw-bin",
            &mw
        ])
        .status
        .success()
    );
    assert_eq!(fs::read(&inbox).unwrap(), source_after_commit);
    let reconciled = s.ok(&[
        "captures",
        "reconcile",
        "--db-path",
        s.queue().to_str().unwrap(),
        "--mw-bin",
        &mw,
        "--capture-id",
        &candidate.capture_id,
    ]);
    assert_eq!(reconciled["report"]["reconciled"], 1);
    assert_eq!(fs::read(&inbox).unwrap(), source_after_commit);
    let status = CaptureStore::inspect(s.queue()).unwrap();
    assert!(
        status
            .iter()
            .all(|j| j.state == DeliveryState::Delivered && j.receipt.is_some())
    );
    assert!(!s.root.join("must-not-create").exists());
}
