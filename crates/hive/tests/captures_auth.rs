//! Process-level auth boundary checks. Deliberately never invoke native storage
//! with a valid connection or launch browser/Google requests.
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
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("home")).unwrap();
        Self { _dir: dir, root }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hive"))
            .args(args)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env(
                "HIVE_GOOGLE_ACCESS_TOKEN",
                "ambient-canary-token-DO-NOT-LOG",
            )
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    fn rejects(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let message = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(!message.contains("ambient-canary-token"));
        assert!(!self.root.join("state").exists());
        assert!(!self.root.join("captures.sqlite").exists());
        message
    }
}

#[test]
fn auth_help_has_no_credential_or_filesystem_side_effects() {
    let s = Sandbox::new();
    for args in [
        vec!["captures", "auth", "--help"],
        vec!["captures", "auth", "login", "--help"],
        vec!["captures", "auth", "status", "--help"],
        vec!["captures", "auth", "logout", "--help"],
    ] {
        assert!(s.run(&args).status.success());
    }
    assert!(!s.root.join("state").exists());
}

#[test]
fn credential_selection_is_explicit_and_exclusive_despite_an_ambient_token() {
    let s = Sandbox::new();
    let missing = s.rejects(&["captures", "lists"]);
    assert!(missing.contains("--connection") && missing.contains("--access-token-env"));
    s.rejects(&[
        "captures",
        "lists",
        "--connection",
        "personal",
        "--access-token-env",
        "HIVE_GOOGLE_ACCESS_TOKEN",
    ]);
    let invalid = s.rejects(&["captures", "lists", "--connection", "INVALID!"]);
    assert!(invalid.contains("invalid connection name"));
}

#[test]
fn login_rejects_missing_and_non_desktop_files_before_native_storage() {
    let s = Sandbox::new();
    let missing = s.rejects(&[
        "captures",
        "auth",
        "login",
        "--connection",
        "personal",
        "--client-json",
        "missing.json",
    ]);
    assert!(missing.contains("invalid Google Desktop OAuth client file"));
    for contents in [
        r#"{"web":{"client_id":"canary.apps.googleusercontent.com","client_secret":"client-canary-DO-NOT-LOG"}}"#,
        r#"{"installed":{"client_id":"invalid","client_secret":"client-canary-DO-NOT-LOG"}}"#,
        "{malformed client-canary-DO-NOT-LOG",
    ] {
        fs::write(s.root.join("client.json"), contents).unwrap();
        let message = s.rejects(&[
            "captures",
            "auth",
            "login",
            "--connection",
            "personal",
            "--client-json",
            "client.json",
        ]);
        assert!(!message.contains("client-canary"));
    }
}

#[test]
fn invalid_connection_and_missing_flags_never_reach_status_logout_or_browser() {
    let s = Sandbox::new();
    for action in ["status", "logout"] {
        let message = s.rejects(&["captures", "auth", action, "--connection", "../INVALID"]);
        assert!(message.contains("invalid connection name"));
        s.rejects(&["captures", "auth", action]);
    }
    s.rejects(&["captures", "auth", "login", "--connection", "personal"]);
}

#[test]
fn saved_capture_cannot_choose_an_operator_namespace_or_fall_back_to_environment() {
    let s = Sandbox::new();
    s.rejects(&[
        "captures",
        "poll",
        "--db-path",
        "captures.sqlite",
        "--connection",
        "personal",
        "--account-key",
        "operator",
        "--list-id",
        "list",
        "--bootstrap",
        "baseline",
    ]);
    let invalid = s.rejects(&[
        "captures",
        "poll",
        "--db-path",
        "captures.sqlite",
        "--connection",
        "INVALID!",
        "--list-id",
        "list",
        "--bootstrap",
        "baseline",
    ]);
    assert!(invalid.contains("invalid connection name"));
    let absent_namespace = s.rejects(&[
        "captures",
        "poll",
        "--db-path",
        "captures.sqlite",
        "--access-token-env",
        "HIVE_GOOGLE_ACCESS_TOKEN",
        "--list-id",
        "list",
        "--bootstrap",
        "baseline",
    ]);
    assert!(absent_namespace.contains("account-key is required"));
}

#[test]
fn unverified_tokens_cannot_enter_verified_identity_namespaces() {
    let s = Sandbox::new();
    let message = s.rejects(&[
        "captures",
        "poll",
        "--db-path",
        "captures.sqlite",
        "--access-token-env",
        "HIVE_GOOGLE_ACCESS_TOKEN",
        "--account-key",
        "google-sub-v1:forged",
        "--list-id",
        "list",
        "--bootstrap",
        "import-open",
    ]);
    assert!(message.contains("reserved Google identity namespace"));
}
