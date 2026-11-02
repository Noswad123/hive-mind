#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use hive_calendar::{CalendarDate, Ledger, Lifecycle, ProspectiveEvent, ProviderAck, payload_hash};
use serde_json::Value;

const SOURCE: &str = "---\nid: fixture\ndomains: [task]\ntask_active: true\ntask_scope: project\n---\n\n## Todo\n### Do Now\n### Inbox\n- [ ] Example deadline\n  - due:2028-02-29\n### Blocked\n";

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "fixture Git command failed");
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn commit(repo: &Path) -> String {
    git(repo, &["add", "--all"]);
    git(
        repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "commit",
            "--quiet",
            "-m",
            "Fixture",
        ],
    );
    git(repo, &["rev-parse", "HEAD"])
}

struct Fixture {
    temp: tempfile::TempDir,
    source: std::path::PathBuf,
    state: std::path::PathBuf,
    parser: std::path::PathBuf,
    source_commit: String,
    state_commit: String,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let source = temp.path().join("source");
        let state = temp.path().join("state");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&state).unwrap();
        git(&source, &["init", "--quiet"]);
        git(&state, &["init", "--quiet"]);
        fs::write(source.join("tasks.md"), SOURCE).unwrap();
        fs::write(
            state.join("seed.txt"),
            "An explicit committed base, not a ledger.",
        )
        .unwrap();
        let source_commit = commit(&source);
        let state_commit = commit(&state);
        let parser = temp.path().join("fixture-mw");
        fs::write(
            &parser,
            r##"#!/bin/sh
set -eu
test "$*" = 'todos snapshot --json'
test -z "${HIVE_GOOGLE_ACCESS_TOKEN+x}"
name=tasks.md
if [ ! -f "$NOTES_DIR/$name" ]; then name=moved.md; fi
if [ -x /usr/bin/shasum ]; then
  hash=$(/usr/bin/shasum -a 256 "$NOTES_DIR/$name")
else
  hash=$(/usr/bin/sha256sum "$NOTES_DIR/$name")
fi
hash=${hash%% *}
printf '{"schemaVersion":"1","source":"saved-markdown","notesDir":"%s","dashboardPath":"%s","items":[{"id":"fixture:11:0","stableId":null,"identityKind":"positional","sourceRevision":"sha256:%s","sourceId":"fixture","sourcePath":"%s","line":11,"text":"Example deadline","done":false,"metadata":{"due":"2028-02-29","raw":"due:2028-02-29","start":"","estimate":""}}]}' "$NOTES_DIR" "$DASHBOARD_PATH" "$hash" "$name"
"##,
        )
        .unwrap();
        fs::set_permissions(&parser, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            temp,
            source,
            state,
            parser,
            source_commit,
            state_commit,
        }
    }

    fn command(&self, operation: &str, path: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hive"));
        cmd.args(["calendar", operation, "--repo"])
            .arg(&self.source)
            .args(["--commit", &self.source_commit, "--mw-bin"])
            .arg(&self.parser)
            .args([
                "--source-repo-id",
                "fixture-repo",
                "--calendar-id",
                "calendar@example.invalid",
                "--account-id",
            ])
            .arg(format!("google-sub-v1:{}", "a".repeat(64)))
            .arg("--ledger-repo")
            .arg(&self.state)
            .args([
                "--ledger-commit",
                &self.state_commit,
                "--ledger-path",
                "calendar.json",
                "--source-path",
                path,
                "--source-line",
                "11",
            ])
            .env("HOME", self.temp.path())
            .env("MW_CONFIG", "/not-a-real-user-config")
            .env("HIVE_GOOGLE_ACCESS_TOKEN", "fixture-secret-not-forwarded");
        cmd
    }

    fn initialize(&self) -> Value {
        json_output(
            self.command("enroll", "tasks.md")
                .arg("--initialize-ledger"),
        )
    }

    fn save_fixture_ledger(&mut self, ledger: &Ledger) {
        fs::write(
            self.state.join("calendar.json"),
            serde_json::to_vec(ledger).unwrap(),
        )
        .unwrap();
        self.state_commit = commit(&self.state);
    }

    fn move_task(&mut self) {
        fs::rename(self.source.join("tasks.md"), self.source.join("moved.md")).unwrap();
        self.source_commit = commit(&self.source);
    }
}

fn json_output(command: &mut Command) -> Value {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn published_ledger(candidate: &Value) -> Ledger {
    let mut ledger: Ledger = serde_json::from_value(candidate["candidateLedger"].clone()).unwrap();
    ledger.entries[0].lifecycle = Lifecycle::Active;
    ledger.entries[0].provider_ack = Some(ProviderAck {
        event_id: "abcde12345".into(),
        etag: "\"fixture-etag\"".into(),
        last_payload_hash: payload_hash(&ProspectiveEvent {
            summary: "Example deadline".into(),
            start: CalendarDate {
                date: "2028-02-29".into(),
            },
            end: CalendarDate {
                date: "2028-03-01".into(),
            },
            availability: "Available",
            transparency: "transparent",
        }),
    });
    ledger
}

#[test]
fn enrollment_is_explicit_candidate_only_and_assigns_uuid_not_source_marker() {
    let f = Fixture::new();
    let output = f.command("enroll", "tasks.md").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let candidate = f.initialize();
    assert_eq!(candidate["kind"], "calendar-ledger-candidate");
    assert_eq!(candidate["operation"], "enroll");
    assert_eq!(candidate["base"]["commit"], f.state_commit);
    assert_eq!(candidate["base"]["condition"], "path_absent_at_commit");
    assert_eq!(candidate["base"]["gitBlobID"], Value::Null);
    assert_eq!(candidate["publicationReady"], false);
    assert_eq!(candidate["ledgerHeadVerified"], false);
    let id = candidate["logicalId"].as_str().unwrap();
    assert_eq!(id.len(), 36);
    assert_eq!(&id[14..15], "4");
    assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    assert_eq!(
        candidate["candidateLedger"]["entries"][0]["sourceAck"]["stableId"],
        Value::Null
    );
    assert!(!f.state.join("calendar.json").exists());
    assert_eq!(git(&f.state, &["rev-parse", "HEAD"]), f.state_commit);
    assert_eq!(
        fs::read_to_string(f.source.join("tasks.md")).unwrap(),
        SOURCE
    );
}

#[test]
fn existing_ledger_never_initializes_or_enrolls_its_task_again() {
    let mut f = Fixture::new();
    let ledger: Ledger = serde_json::from_value(f.initialize()["candidateLedger"].clone()).unwrap();
    f.save_fixture_ledger(&ledger);
    for initialize in [false, true] {
        let mut command = f.command("enroll", "tasks.md");
        if initialize {
            command.arg("--initialize-ledger");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    assert_eq!(git(&f.state, &["rev-parse", "HEAD"]), f.state_commit);
    assert_eq!(git(&f.state, &["status", "--porcelain=v1"]), "");
}

#[test]
fn reack_moves_markerless_source_and_preserves_uuid_and_provider_evidence() {
    let mut f = Fixture::new();
    let candidate = f.initialize();
    let ledger = published_ledger(&candidate);
    f.save_fixture_ledger(&ledger);
    f.move_task();
    let candidate = json_output(f.command("reack", "moved.md").args([
        "--old-source-path",
        "tasks.md",
        "--old-source-line",
        "11",
    ]));
    assert_eq!(candidate["operation"], "reack");
    assert_eq!(candidate["logicalId"], ledger.entries[0].logical_id);
    assert_eq!(candidate["base"]["commit"], f.state_commit);
    assert_eq!(candidate["base"]["condition"], "existing_blob_at_commit");
    let updated: Ledger = serde_json::from_value(candidate["candidateLedger"].clone()).unwrap();
    assert_eq!(updated.entries[0].logical_id, ledger.entries[0].logical_id);
    assert_eq!(
        updated.entries[0].provider_ack,
        ledger.entries[0].provider_ack
    );
    assert_eq!(updated.entries[0].lifecycle, ledger.entries[0].lifecycle);
    assert_eq!(updated.entries[0].source_ack.path, "moved.md");
    assert_eq!(updated.source_watermark, f.source_commit);
    assert_eq!(git(&f.state, &["rev-parse", "HEAD"]), f.state_commit);
    assert_eq!(
        fs::read_to_string(f.source.join("moved.md")).unwrap(),
        SOURCE
    );
}

#[test]
fn reack_cannot_replace_forged_old_evidence_or_roll_back_the_watermark() {
    let mut f = Fixture::new();
    let mut ledger = published_ledger(&f.initialize());
    ledger.entries[0].source_ack.expected.text = "Forged original evidence".into();
    f.save_fixture_ledger(&ledger);
    let output = f
        .command("reack", "tasks.md")
        .args(["--old-source-path", "tasks.md", "--old-source-line", "11"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(git(&f.state, &["rev-parse", "HEAD"]), f.state_commit);

    let mut f = Fixture::new();
    let mut ledger = published_ledger(&f.initialize());
    let old_source = f.source_commit.clone();
    f.move_task();
    ledger.source_watermark = f.source_commit.clone();
    f.save_fixture_ledger(&ledger);
    f.source_commit = old_source;
    let output = f
        .command("reack", "tasks.md")
        .args(["--old-source-path", "tasks.md", "--old-source-line", "11"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(git(&f.state, &["rev-parse", "HEAD"]), f.state_commit);
}

#[test]
#[ignore = "requires explicit MW_TEST_BIN; candidate-only tests in temporary repositories"]
fn real_mw_enrollment_and_reack_preserve_markerless_identity() {
    let mut f = Fixture::new();
    f.parser = std::env::var_os("MW_TEST_BIN")
        .expect("set MW_TEST_BIN explicitly")
        .into();
    let candidate = f.initialize();
    let ledger = published_ledger(&candidate);
    f.save_fixture_ledger(&ledger);
    f.move_task();
    let moved = json_output(f.command("reack", "moved.md").args([
        "--old-source-path",
        "tasks.md",
        "--old-source-line",
        "11",
    ]));
    let accepted: Ledger = serde_json::from_value(moved["candidateLedger"].clone()).unwrap();
    assert_eq!(accepted.entries[0].logical_id, ledger.entries[0].logical_id);
    assert_eq!(
        accepted.entries[0].provider_ack,
        ledger.entries[0].provider_ack
    );
    f.save_fixture_ledger(&accepted);

    fs::write(f.source.join("moved.md"), SOURCE.replace("- [ ]", "- [x]")).unwrap();
    f.source_commit = commit(&f.source);
    let completed = json_output(f.command("reack", "moved.md").args([
        "--old-source-path",
        "moved.md",
        "--old-source-line",
        "11",
    ]));
    let completed_ledger: Ledger =
        serde_json::from_value(completed["candidateLedger"].clone()).unwrap();
    assert!(completed_ledger.entries[0].source_ack.expected.done);
    assert_eq!(
        completed_ledger.entries[0].provider_ack,
        ledger.entries[0].provider_ack
    );
    assert_eq!(completed_ledger.entries[0].lifecycle, Lifecycle::Active);

    fs::write(
        f.source.join("moved.md"),
        SOURCE
            .replace("- [ ]", "- [x]")
            .replace("2028-02-29", "2028-02-30"),
    )
    .unwrap();
    f.source_commit = commit(&f.source);
    let output = f
        .command("reack", "moved.md")
        .args(["--old-source-path", "moved.md", "--old-source-line", "11"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
