use hive_capture::{
    handoff::{ReceiptEvidence, TaskHandoff},
    store::deterministic_capture_id,
};
use hive_google_auth::{AuthProfile, GoogleIdentity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn cli(args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hive"));
    c.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .env("HIVE_HOSTED_TASKS_WRITE_RECORD", "invalid-secret-canary")
        .env("GOOGLE_ACCESS_TOKEN", "generic-bearer-canary")
        .env("GITHUB_TOKEN", "github-secret-canary")
        .args(args);
    c
}
fn safe(out: &Output) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for s in [
        "invalid-secret-canary",
        "generic-bearer-canary",
        "raw-subject-canary",
        "refresh-canary",
        "client-secret-canary",
        "private-title-canary",
        "private-source-canary",
        "github-secret-canary",
    ] {
        assert!(!text.contains(s));
    }
    assert!(!text.to_lowercase().contains("keychain"));
}
#[test]
fn default_help_and_rejected_flags_never_reach_environment_or_native_auth() {
    let out = cli(&["tasks", "hosted", "--help"]).output().unwrap();
    assert!(out.status.success());
    safe(&out);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("check"));
    assert!(text.contains("plan"));
    for name in ["apply", "adopt", "publish", "recover", "install"] {
        assert!(
            !text
                .lines()
                .filter_map(|l| l.split_whitespace().next())
                .any(|w| w == name)
        );
    }
    assert!(text.contains("verify-main"));
    for op in ["check", "plan", "verify-main"] {
        let out = cli(&["tasks", "hosted", op, "--help"]).output().unwrap();
        assert!(out.status.success());
        safe(&out);
        let text = String::from_utf8(out.stdout).unwrap();
        for flag in ["--binding", "--repo", "--mw-bin", "--commit"] {
            assert!(text.contains(flag));
        }
        for flag in [
            "--endpoint",
            "--token",
            "--connection",
            "--observation",
            "--plan",
            "--state",
            "--apply",
            "--account",
            "--list-id",
            "--task-id",
            "--remote-url",
        ] {
            assert!(!text.contains(flag));
            let out = cli(&["tasks", "hosted", op, flag, "forged"])
                .output()
                .unwrap();
            assert!(!out.status.success());
            safe(&out);
        }
    }
    let out = cli(&["tasks", "hosted", "apply"]).output().unwrap();
    assert!(!out.status.success());
    safe(&out);
}
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    parser: PathBuf,
    binding: PathBuf,
    event: PathBuf,
    sha: String,
    record: String,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("source");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--template=", "-b", "main"]);
        let identity = GoogleIdentity::new("raw-subject-canary").unwrap();
        let account = identity.namespace().unwrap();
        let capture = deterministic_capture_id(&account, "exact-list", "exact-task").unwrap();
        let h = TaskHandoff {
            schema_version: "1".into(),
            kind: "google-task-handoff".into(),
            source_repo_id: "opaque-source".into(),
            capture_id: capture.clone(),
            account_key: account.clone(),
            list_id: "exact-list".into(),
            provider_id: "exact-task".into(),
            accepted_current_baseline: true,
            baseline: serde_json::from_value(
                json!({"id":"exact-task","title":"private-title-canary","status":"needsAction"}),
            )
            .unwrap(),
            receipt: ReceiptEvidence {
                method: "todos-add".into(),
                sha256: "a".repeat(64),
                stable_id: capture.clone(),
                source_id: "note".into(),
                source_path: "tasks.md".into(),
                source_revision: format!("sha256:{}", "b".repeat(64)),
            },
        };
        let text = format!(
            "---\nid: note\ndomains: [task]\ntask_active: true\ntask_scope: project\n---\n\n## Todo\n### Inbox\n- [ ] private-source-canary\n  - due:2026-11-10\n  <!-- mw-task:v1:{capture} -->\n"
        );
        fs::write(repo.join("tasks.md"), &text).unwrap();
        git(&repo, &["add", "tasks.md"]);
        git(&repo, &["commit", "-m", "fixture"]);
        let sha = git(&repo, &["rev-parse", "HEAD"]);
        let item = json!({"id":format!("task:{capture}"),"stableId":capture,"identityKind":"stable","sourceRevision":format!("sha256:{:x}",Sha256::digest(text.as_bytes())),
            "sourceId":"note","sourcePath":"tasks.md","line":10,"text":"private-source-canary","done":false,"metadata":{"due":"2026-11-10","raw":"due:2026-11-10","start":"","estimate":""}});
        let parser = root.join("mw-stub");
        fs::write(&parser,format!("#!/bin/sh\nset -eu\ntest \"${{HIVE_HOSTED_TASKS_WRITE_RECORD+x}}\" != x\ntest \"${{GOOGLE_ACCESS_TOKEN+x}}\" != x\nprintf '{{\"schemaVersion\":\"1\",\"source\":\"saved-markdown\",\"notesDir\":\"%s\",\"dashboardPath\":\"%s\",\"items\":[{item}]}}' \"$NOTES_DIR\" \"$DASHBOARD_PATH\"\n")).unwrap();
        fs::set_permissions(&parser, fs::Permissions::from_mode(0o700)).unwrap();
        let binding = root.join("binding.json");
        let event = root.join("event.json");
        fs::write(&binding,json!({"schemaVersion":"1","kind":"google-tasks-hosted-binding","expectedRepositoryId":"123","expectedRepository":"owner/repo",
            "sourceRepoId":"opaque-source","connection":"work","expectedAccountNamespace":account,"trustedParserSha256":format!("{:x}",Sha256::digest(fs::read(&parser).unwrap())),
            "reviewedTaskHandoff":h,"handoffSha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&serde_json::to_value(&h).unwrap()).unwrap()))}).to_string()).unwrap();
        fs::write(&event,json!({"ref":"refs/heads/main","deleted":false,"after":sha,"repository":{"id":123,"full_name":"owner/repo"}}).to_string()).unwrap();
        let record=json!({"version":1,"connection":"work","identity":identity,"generation":1,"grant":{"client":{"client_id":"synthetic.apps.googleusercontent.com","client_secret":"client-secret-canary"},"refresh_token":"refresh-canary","scopes":AuthProfile::TasksWrite.scopes()}}).to_string();
        Self {
            _temp: temp,
            root,
            repo,
            parser,
            binding,
            event,
            sha,
            record,
        }
    }
    fn command(&self, op: &str) -> Command {
        let mut c = cli(&["tasks", "hosted", op]);
        c.arg("--binding")
            .arg(&self.binding)
            .arg("--repo")
            .arg(&self.repo)
            .arg("--mw-bin")
            .arg(&self.parser)
            .arg("--commit")
            .arg(&self.sha)
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_EVENT_NAME", "push")
            .env("GITHUB_REF", "refs/heads/main")
            .env("GITHUB_SHA", &self.sha)
            .env("GITHUB_REPOSITORY", "owner/repo")
            .env("GITHUB_REPOSITORY_ID", "123")
            .env("GITHUB_EVENT_PATH", &self.event);
        c
    }
}
#[test]
fn subprocess_check_is_default_redacted_readonly_and_child_environment_is_sanitized() {
    let f = Fixture::new();
    let before = git(&f.repo, &["show-ref"]);
    git(&f.repo, &["checkout", "--detach", &f.sha]);
    fs::write(f.repo.join("tasks.md"), "dirty ignored").unwrap();
    let out = f
        .command("check")
        .env("HIVE_HOSTED_TASKS_WRITE_RECORD", &f.record)
        .output()
        .unwrap();
    safe(&out);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    for flag in [
        "redacted",
        "secretPresent",
        "structureValid",
        "pushClaimsAdmitted",
    ] {
        assert_eq!(v[flag], true);
    }
    for flag in [
        "currentMainVerified",
        "pushVerified",
        "githubVerified",
        "remoteReadVerifiedDuringCollection",
        "portableEvidenceAuthenticated",
        "publicationReady",
        "conditionalWriteSafe",
        "hiveWriteProven",
        "hostedExecutionEnabled",
    ] {
        assert_eq!(v[flag], false);
    }
    let text = String::from_utf8(out.stdout).unwrap();
    for s in [
        &f.record,
        "exact-list",
        "exact-task",
        "2026-11-10",
        f.root.to_str().unwrap(),
    ] {
        assert!(!text.contains(s));
    }
    assert_eq!(before, git(&f.repo, &["show-ref"]));
    assert_eq!(
        fs::read_to_string(f.repo.join("tasks.md")).unwrap(),
        "dirty ignored"
    );
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 4);
    // Even a plan fails locally before looking at invalid secret bytes or constructing HTTP.
    let out = f
        .command("plan")
        .env("GITHUB_EVENT_NAME", "pull_request")
        .output()
        .unwrap();
    safe(&out);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("push_admission_failed"));
    let out = f.command("check").output().unwrap();
    safe(&out);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("credential_structure_invalid"));
    let out = f
        .command("check")
        .env_remove("HIVE_HOSTED_TASKS_WRITE_RECORD")
        .output()
        .unwrap();
    safe(&out);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("credential_unavailable"));
    // verify-main uses only the fixed GitHub variable, never the Google record.
    // Missing/invalid context and missing token cannot open any provider socket.
    let out = f
        .command("verify-main")
        .env_remove("GITHUB_TOKEN")
        .env("HIVE_HOSTED_TASKS_WRITE_RECORD", &f.record)
        .output()
        .unwrap();
    safe(&out);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("credential_unavailable"));
    let out = f
        .command("verify-main")
        .env("GITHUB_EVENT_NAME", "pull_request")
        .env("GITHUB_TOKEN", "github-secret-canary")
        .output()
        .unwrap();
    safe(&out);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("push_admission_failed"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("github-secret-canary"));
}
