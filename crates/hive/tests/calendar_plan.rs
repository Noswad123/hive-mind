#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use hive_calendar::{
    Binding, Ledger, LedgerEntry, Lifecycle, OperationKind, OperationPhase, PendingOperation,
    ProviderAck, RemoteDisposition, SourceAck, Tombstone,
};
use serde_json::Value;

const SOURCE: &str = "---\nid: fixture\ndomains: [task]\ntask_active: true\ntask_scope: project\n---\n\n## Todo\n### Do Now\n### Inbox\n- [ ] Example deadline\n  - due:2028-02-29\n### Blocked\n";
const LOGICAL_ID: &str = "11111111-1111-4111-8111-111111111111";

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

fn commit_file(repo: &Path, name: &str, content: &[u8]) -> String {
    fs::write(repo.join(name), content).unwrap();
    git(repo, &["add", "--", name]);
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
    commit: String,
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
        let commit = commit_file(&source, "tasks.md", SOURCE.as_bytes());
        let parser = temp.path().join("fixture-mw");
        fs::write(
            &parser,
            r##"#!/bin/sh
set -eu
test "$*" = 'todos snapshot --json'
test -z "${HIVE_GOOGLE_ACCESS_TOKEN+x}"
if [ -x /usr/bin/shasum ]; then
  hash=$(/usr/bin/shasum -a 256 "$NOTES_DIR/tasks.md")
else
  hash=$(/usr/bin/sha256sum "$NOTES_DIR/tasks.md")
fi
hash=${hash%% *}
printf '{"schemaVersion":"1","source":"saved-markdown","notesDir":"%s","dashboardPath":"%s","items":[{"id":"fixture:11:0","stableId":null,"identityKind":"positional","sourceRevision":"sha256:%s","sourceId":"fixture","sourcePath":"tasks.md","line":11,"text":"Example deadline","done":false,"metadata":{"due":"2028-02-29","raw":"due:2028-02-29","start":"","estimate":""}}]}' "$NOTES_DIR" "$DASHBOARD_PATH" "$hash"
"##,
        )
        .unwrap();
        fs::set_permissions(&parser, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            temp,
            source,
            state,
            parser,
            commit,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hive"));
        cmd.args(["calendar", "plan", "--repo"])
            .arg(&self.source)
            .args(["--commit", &self.commit, "--mw-bin"])
            .arg(&self.parser)
            .args([
                "--source-repo-id",
                "fixture-repo",
                "--calendar-id",
                "calendar@example.invalid",
                "--account-id",
            ])
            .arg(format!("google-sub-v1:{}", "a".repeat(64)))
            .env("HOME", self.temp.path())
            .env("MW_CONFIG", "/not-a-real-user-config")
            .env("HIVE_GOOGLE_ACCESS_TOKEN", "fixture-secret-not-forwarded");
        cmd
    }

    fn bootstrap(&self) -> Value {
        output_json(self.command().arg("--bootstrap-preview"))
    }

    fn ledger(&self, bootstrap: &Value) -> Ledger {
        let observation = &bootstrap["unenrolledObservations"][0];
        Ledger {
            schema_version: "1".into(),
            binding: serde_json::from_value::<Binding>(bootstrap["binding"].clone()).unwrap(),
            source_watermark: self.commit.clone(),
            entries: vec![LedgerEntry {
                logical_id: LOGICAL_ID.into(),
                lifecycle: Lifecycle::Active,
                source_ack: serde_json::from_value::<SourceAck>(observation["sourceAck"].clone())
                    .unwrap(),
                provider_ack: Some(ProviderAck {
                    event_id: "abcde12345".into(),
                    etag: "\"fixture-etag\"".into(),
                    last_payload_hash: observation["desiredPayloadHash"].as_str().unwrap().into(),
                }),
                pending: None,
                tombstone: None,
            }],
        }
    }

    fn commit_ledger(&self, ledger: &Ledger) -> String {
        commit_file(
            &self.state,
            "calendar.json",
            &serde_json::to_vec(ledger).unwrap(),
        )
    }

    fn committed_command(&self, ledger_commit: &str) -> Command {
        let mut cmd = self.command();
        cmd.arg("--ledger-repo").arg(&self.state).args([
            "--ledger-commit",
            ledger_commit,
            "--ledger-path",
            "calendar.json",
        ]);
        cmd
    }
}

fn output_json(command: &mut Command) -> Value {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn ledger_or_explicit_bootstrap_is_required_and_exclusive() {
    let f = Fixture::new();
    for extra in [
        vec![],
        vec!["--ledger-commit", &f.commit],
        vec!["--ledger-repo", "/missing"],
        vec!["--bootstrap-preview", "--ledger-repo", "/missing"],
    ] {
        let output = f.command().args(extra).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let bootstrap = f.bootstrap();
    assert_eq!(bootstrap["mode"], "bootstrap-preview");
    assert_eq!(bootstrap["publicationReady"], false);
    assert_eq!(bootstrap["remoteVerified"], false);
    assert_eq!(bootstrap["ledgerHeadVerified"], false);
    assert_eq!(bootstrap["counts"]["unenrolledObservations"], 1);
    assert_eq!(
        bootstrap["unenrolledObservations"][0]["sourceAck"]["stableId"],
        Value::Null
    );
    assert_eq!(git(&f.state, &["status", "--porcelain=v1"]), "");
}

#[test]
fn committed_markerless_mapping_is_advisory_and_ignores_dirty_state() {
    let f = Fixture::new();
    let ledger = f.ledger(&f.bootstrap());
    let commit = f.commit_ledger(&ledger);
    fs::write(f.state.join("calendar.json"), "not the committed ledger").unwrap();
    fs::write(f.source.join("tasks.md"), "not the committed source").unwrap();
    let source_status = git(&f.source, &["status", "--porcelain=v1"]);
    let state_status = git(&f.state, &["status", "--porcelain=v1"]);
    let plan = output_json(&mut f.committed_command(&commit));
    assert_eq!(plan["mode"], "ledger");
    assert_eq!(plan["ledger"]["commit"], commit);
    assert_eq!(plan["entries"][0]["logicalId"], LOGICAL_ID);
    assert_eq!(plan["entries"][0]["decision"], "noop");
    assert_eq!(plan["entries"][0]["publicationReady"], false);
    assert_eq!(plan["remoteVerified"], false);
    assert_eq!(plan["ledgerHeadVerified"], false);
    assert_eq!(plan["counts"]["unenrolledObservations"], 0);
    assert_eq!(git(&f.source, &["status", "--porcelain=v1"]), source_status);
    assert_eq!(git(&f.state, &["status", "--porcelain=v1"]), state_status);
    assert_eq!(git(&f.state, &["rev-parse", "HEAD"]), commit);
}

#[test]
fn pending_create_and_retirement_never_propose_creation_or_resurrection() {
    let f = Fixture::new();
    let mut ledger = f.ledger(&f.bootstrap());
    let entry = &mut ledger.entries[0];
    let hash = entry
        .provider_ack
        .as_ref()
        .unwrap()
        .last_payload_hash
        .clone();
    entry.provider_ack = None;
    entry.lifecycle = Lifecycle::Enrolled;
    entry.pending = Some(PendingOperation {
        operation_id: "22222222-2222-4222-8222-222222222222".into(),
        kind: OperationKind::Create,
        phase: OperationPhase::Uncertain,
        intended_source: entry.source_ack.clone(),
        target_event_id: "abcde12345".into(),
        base_etag: None,
        desired_payload_hash: Some(hash),
    });
    let commit = f.commit_ledger(&ledger);
    let plan = output_json(&mut f.committed_command(&commit));
    assert_eq!(plan["entries"][0]["decision"], "blocked");
    assert_eq!(plan["entries"][0]["prospectiveEvent"], Value::Null);

    ledger = f.ledger(&f.bootstrap());
    ledger.entries[0].lifecycle = Lifecycle::Retired;
    ledger.entries[0].tombstone = Some(Tombstone {
        reason: "Explicitly stopped managing this event".into(),
        source_commit: f.commit.clone(),
        operation_id: None,
        remote_disposition: RemoteDisposition::Retained,
    });
    let commit = f.commit_ledger(&ledger);
    let plan = output_json(&mut f.committed_command(&commit));
    assert_eq!(plan["entries"][0]["decision"], "retired");
    assert_eq!(plan["counts"]["unenrolledObservations"], 0);
    assert_eq!(plan["entries"][0]["prospectiveEvent"], Value::Null);
}

#[test]
fn changed_source_requires_review_and_bad_state_never_bootstraps() {
    let mut f = Fixture::new();
    let ledger = f.ledger(&f.bootstrap());
    let commit = f.commit_ledger(&ledger);
    f.commit = commit_file(
        &f.source,
        "tasks.md",
        format!("{SOURCE}\nAn unrelated edit.\n").as_bytes(),
    );
    let plan = output_json(&mut f.committed_command(&commit));
    assert_eq!(plan["entries"][0]["decision"], "review");
    assert_eq!(plan["counts"]["unenrolledObservations"], 1);

    let invalid = commit_file(&f.state, "calendar.json", b"{invalid fixture-secret}");
    let output = f.committed_command(&invalid).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-secret"));
    let output = f
        .command()
        .arg("--ledger-repo")
        .arg(&f.state)
        .args(["--ledger-commit", &commit, "--ledger-path", "missing.json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn crlf_bootstrap_observation_round_trips_without_rewriting_literal_bytes() {
    let mut f = Fixture::new();
    let crlf = SOURCE.replace('\n', "\r\n");
    f.commit = commit_file(&f.source, "tasks.md", crlf.as_bytes());
    let bootstrap = f.bootstrap();
    let literal = bootstrap["unenrolledObservations"][0]["sourceAck"]["expected"]["literalText"]
        .as_str()
        .unwrap();
    assert!(literal.ends_with('\r'));
    let ledger = f.ledger(&bootstrap);
    let commit = f.commit_ledger(&ledger);
    let plan = output_json(&mut f.committed_command(&commit));
    assert_eq!(plan["entries"][0]["decision"], "noop");
    assert_eq!(
        plan["entries"][0]["sourceAck"]["expected"]["literalText"],
        literal
    );
    assert_eq!(fs::read_to_string(f.source.join("tasks.md")).unwrap(), crlf);
}

#[test]
#[ignore = "requires explicit MW_TEST_BIN; temporary source and ledger repositories only"]
fn real_mw_markerless_ledger_round_trip() {
    let mut f = Fixture::new();
    f.parser = std::env::var_os("MW_TEST_BIN")
        .expect("set MW_TEST_BIN explicitly")
        .into();
    let bootstrap = f.bootstrap();
    let ledger = f.ledger(&bootstrap);
    let commit = f.commit_ledger(&ledger);
    let plan = output_json(&mut f.committed_command(&commit));
    assert_eq!(plan["entries"][0]["decision"], "noop");
    assert_eq!(plan["publicationReady"], false);
    assert_eq!(plan["entries"][0]["sourceAck"]["stableId"], Value::Null);
    assert_eq!(
        fs::read_to_string(f.source.join("tasks.md")).unwrap(),
        SOURCE
    );

    let crlf = SOURCE.replace('\n', "\r\n");
    f.commit = commit_file(&f.source, "tasks.md", crlf.as_bytes());
    let bootstrap = f.bootstrap();
    let ledger = f.ledger(&bootstrap);
    assert!(
        ledger.entries[0]
            .source_ack
            .expected
            .literal_text
            .ends_with('\r')
    );
    let commit = f.commit_ledger(&ledger);
    let plan = output_json(&mut f.committed_command(&commit));
    assert_eq!(plan["entries"][0]["decision"], "noop");
    assert_eq!(fs::read_to_string(f.source.join("tasks.md")).unwrap(), crlf);
}
