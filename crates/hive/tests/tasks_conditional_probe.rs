use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hive"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn probe_command_is_only_available_in_opt_in_build() {
    let output = cli(&["tasks", "--help"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).contains("conditional-probe"),
        cfg!(feature = "tasks-conditional-probe")
    );
    let output = cli(&["tasks", "conditional-probe", "--help"]);
    assert_eq!(
        output.status.success(),
        cfg!(feature = "tasks-conditional-probe")
    );
}

#[cfg(feature = "tasks-conditional-probe")]
#[test]
fn help_and_invalid_cli_flags_do_not_create_journal_or_access_auth() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().canonicalize().unwrap().join("never-created.jsonl");
    for verb in ["inspect", "run"] {
        let help = cli(&["tasks", "conditional-probe", verb, "--help"]);
        assert!(help.status.success());
        for forbidden in [
            "--endpoint",
            "--token",
            "--env",
            "--live",
            "--queue",
            "--handoff",
        ] {
            let output = cli(&[
                "tasks",
                "conditional-probe",
                verb,
                forbidden,
                "canary",
                "--journal",
                path.to_str().unwrap(),
            ]);
            assert!(!output.status.success());
            assert!(!path.exists());
        }
        let missing = cli(&["tasks", "conditional-probe", verb]);
        assert!(!missing.status.success());
    }
    // All otherwise complete arguments, but no acknowledgment. This must fail in
    // preflight before native Keychain construction, OAuth, or journal creation.
    let output = cli(&[
        "tasks",
        "conditional-probe",
        "run",
        "--connection",
        "fixture",
        "--expected-account-key",
        &format!("google-sub-v1:{}", "a".repeat(64)),
        "--list-id",
        "list",
        "--task-id",
        "task",
        "--journal",
        path.to_str().unwrap(),
        "--expected-etag",
        "\"e0\"",
        "--expected-original-due",
        "2026-11-10",
        "--chosen-due",
        "2026-11-11",
        "--expected-baseline-sha256",
        &"a".repeat(64),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--acknowledge-disposable required"));
    assert!(!path.exists());
}
