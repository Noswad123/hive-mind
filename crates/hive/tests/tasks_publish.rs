use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hive"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn default_publisher_help_exposes_only_narrow_commands_and_explicit_apply_approval() {
    let out = cli(&["tasks", "publish", "--help"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for command in ["init", "prepare", "apply", "recover", "inspect"] {
        assert!(text.contains(command));
    }
    for command in ["delete", "reschedule", "complete", "force", "reset"] {
        assert!(
            !text
                .lines()
                .filter_map(|line| line.split_whitespace().next())
                .any(|word| word == command)
        );
    }
    let out = cli(&["tasks", "publish", "apply", "--help"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for flag in [
        "--operation-id",
        "--expected-state-oid",
        "--expected-intent-sha256",
        "--acknowledge-live-write",
    ] {
        assert!(text.contains(flag));
    }
    for flag in [
        "--token",
        "--endpoint",
        "--list-id",
        "--task-id",
        "--connection",
    ] {
        assert!(!text.contains(flag));
    }
}

#[test]
fn missing_local_state_or_approval_fails_without_credential_setup() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("absent.git");
    let out = cli(&[
        "tasks",
        "publish",
        "apply",
        "--state",
        state.to_str().unwrap(),
        "--operation-id",
        &"a".repeat(64),
        "--expected-state-oid",
        &"b".repeat(40),
        "--expected-intent-sha256",
        &"c".repeat(64),
    ]);
    assert!(!out.status.success());
    assert!(!state.exists());
    let text = String::from_utf8(out.stderr).unwrap().to_lowercase();
    assert!(!text.contains("keychain"));
    assert!(!text.contains("oauth"));
}
