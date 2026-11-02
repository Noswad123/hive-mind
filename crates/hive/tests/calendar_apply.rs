#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use serde_json::Value;
use sha2::{Digest, Sha256};

const STATE_REF: &str = "refs/heads/hive-calendar-state";
const LEDGER_PATH: &str = "state/calendar.json";
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

struct Artifact {
    path: std::path::PathBuf,
    digest: String,
    source_commit: String,
    value: Value,
}

struct Fixture {
    temp: tempfile::TempDir,
    source: std::path::PathBuf,
    state: std::path::PathBuf,
    parser: std::path::PathBuf,
    source_commit: String,
    seed: String,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let source = temp.path().join("source");
        let state = temp.path().join("state.git");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--quiet"]);
        fs::write(source.join("tasks.md"), SOURCE).unwrap();
        let source_commit = commit(&source);
        let clone = Command::new("/usr/bin/git")
            .args(["clone", "--quiet", "--bare", "--no-hardlinks"])
            .arg(&source)
            .arg(&state)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(clone.status.success());
        let parser = temp.path().join("fixture-mw");
        fs::write(&parser, r##"#!/bin/sh
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
"##).unwrap();
        fs::set_permissions(&parser, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            temp,
            source,
            state,
            parser,
            seed: source_commit.clone(),
            source_commit,
        }
    }

    fn common(&self, command: &str, source_commit: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hive"));
        cmd.args(["calendar", command, "--repo"])
            .arg(&self.source)
            .args(["--commit", source_commit, "--mw-bin"])
            .arg(&self.parser)
            .args([
                "--source-repo-id",
                "apply-fixture",
                "--calendar-id",
                "calendar@example.invalid",
                "--account-id",
            ])
            .arg(format!("google-sub-v1:{}", "a".repeat(64)))
            .arg("--ledger-repo")
            .arg(&self.state)
            .args(["--ledger-path", LEDGER_PATH])
            .env("HOME", self.temp.path())
            .env("MW_CONFIG", "/not-a-user-config")
            .env("HIVE_GOOGLE_ACCESS_TOKEN", "fixture-secret-not-forwarded");
        cmd
    }

    fn artifact(&self, name: &str, bytes: Vec<u8>) -> Artifact {
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let path = self.temp.path().join(name);
        fs::write(&path, &bytes).unwrap();
        Artifact {
            source_commit: value["sourceCommit"].as_str().unwrap().into(),
            digest: format!("{:x}", Sha256::digest(&bytes)),
            path,
            value,
        }
    }

    fn initial(&self) -> Artifact {
        let out = self
            .common("enroll", &self.source_commit)
            .args([
                "--ledger-commit",
                &self.seed,
                "--source-path",
                "tasks.md",
                "--source-line",
                "11",
                "--initialize-ledger",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{:?}", out.stderr);
        self.artifact("initial.json", out.stdout)
    }

    fn persistence(&self, operation: &str, artifact: &Artifact, create: bool) -> Command {
        let mut cmd = self.common(operation, &artifact.source_commit);
        cmd.args(["--state-ref", STATE_REF, "--candidate"])
            .arg(&artifact.path)
            .args(["--candidate-sha256", &artifact.digest]);
        if create {
            cmd.arg("--create-ref");
        }
        cmd
    }

    fn reack(&self, base: &str) -> Artifact {
        let out = self
            .common("reack", &self.source_commit)
            .args([
                "--ledger-commit",
                base,
                "--source-path",
                "moved.md",
                "--source-line",
                "11",
                "--old-source-path",
                "tasks.md",
                "--old-source-line",
                "11",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{:?}", out.stderr);
        self.artifact("reack.json", out.stdout)
    }
}

fn json_output(command: &mut Command) -> Value {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn exercise_round_trip(mut f: Fixture) {
    let initial = f.initial();
    let objects = git(&f.state, &["count-objects", "-v"]);
    let refs = git(&f.state, &["for-each-ref"]);
    let config = fs::read(f.state.join("config")).unwrap();
    let index = fs::read(f.source.join(".git/index")).unwrap();
    let verified = json_output(&mut f.persistence("verify-candidate", &initial, true));
    assert_eq!(verified["status"], "verified");
    assert_eq!(git(&f.state, &["count-objects", "-v"]), objects);
    assert_eq!(git(&f.state, &["for-each-ref"]), refs);
    assert_eq!(fs::read(f.state.join("config")).unwrap(), config);

    let applied = json_output(&mut f.persistence("apply", &initial, true));
    assert_eq!(applied["status"], "applied");
    assert_eq!(applied["commit"], verified["commit"]);
    assert_eq!(applied["publicationReady"], false);
    assert_eq!(applied["remoteVerified"], false);
    let first_commit = applied["commit"].as_str().unwrap().to_owned();
    assert_eq!(git(&f.state, &["rev-parse", STATE_REF]), first_commit);
    assert_eq!(
        git(&f.state, &["show", &format!("{first_commit}:tasks.md")]),
        SOURCE.trim()
    );
    let objects = git(&f.state, &["count-objects", "-v"]);
    let retry = json_output(&mut f.persistence("apply", &initial, true));
    assert_eq!(retry["status"], "already_applied");
    assert_eq!(retry["localRefUpdated"], false);
    assert_eq!(git(&f.state, &["count-objects", "-v"]), objects);
    assert_eq!(fs::read(f.source.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read_to_string(f.source.join("tasks.md")).unwrap(),
        SOURCE
    );

    fs::rename(f.source.join("tasks.md"), f.source.join("moved.md")).unwrap();
    f.source_commit = commit(&f.source);
    let index = fs::read(f.source.join(".git/index")).unwrap();
    let updated = f.reack(&first_commit);
    let verify_update = json_output(&mut f.persistence("verify-candidate", &updated, false));
    let applied_update = json_output(&mut f.persistence("apply", &updated, false));
    assert_eq!(applied_update["commit"], verify_update["commit"]);
    let second_commit = applied_update["commit"].as_str().unwrap();
    assert_ne!(first_commit, second_commit);
    let ledger: Value = serde_json::from_str(&git(
        &f.state,
        &["show", &format!("{second_commit}:{LEDGER_PATH}")],
    ))
    .unwrap();
    assert_eq!(
        ledger["entries"][0]["logicalId"],
        initial.value["logicalId"]
    );
    assert_eq!(ledger["entries"][0]["sourceAck"]["path"], "moved.md");
    assert_eq!(ledger["entries"][0]["providerAck"], Value::Null);
    let objects = git(&f.state, &["count-objects", "-v"]);
    let stale = f.persistence("apply", &initial, true).output().unwrap();
    assert!(!stale.status.success());
    assert!(stale.stdout.is_empty());
    assert_eq!(git(&f.state, &["rev-parse", STATE_REF]), second_commit);
    assert_eq!(git(&f.state, &["count-objects", "-v"]), objects);
    assert_eq!(fs::read(f.source.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read_to_string(f.source.join("moved.md")).unwrap(),
        SOURCE
    );
}

#[test]
fn public_verify_apply_retry_and_reack_are_local_and_compare_whole_heads() {
    exercise_round_trip(Fixture::new());
}

#[test]
fn independent_hash_and_transition_validation_refuse_without_state_writes() {
    let f = Fixture::new();
    let mut artifact = f.initial();
    artifact.digest = "0".repeat(64);
    let objects = git(&f.state, &["count-objects", "-v"]);
    let refs = git(&f.state, &["for-each-ref"]);
    let output = f.persistence("apply", &artifact, true).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    let original = f.initial();
    let mut changed = original.value;
    changed["candidateLedger"]["entries"][0]["sourceAck"]["expected"]["text"] =
        "Forged source".into();
    let tampered = f.artifact("tampered.json", serde_json::to_vec(&changed).unwrap());
    let output = f.persistence("apply", &tampered, true).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(git(&f.state, &["for-each-ref"]), refs);
    assert_eq!(git(&f.state, &["count-objects", "-v"]), objects);
}

#[test]
fn an_existing_foreign_ref_is_not_adopted_during_initialization() {
    let f = Fixture::new();
    let artifact = f.initial();
    git(&f.state, &["update-ref", STATE_REF, &f.seed]);
    let refs = git(&f.state, &["for-each-ref"]);
    let output = f.persistence("apply", &artifact, true).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(git(&f.state, &["for-each-ref"]), refs);
}

#[test]
#[ignore = "requires explicit MW_TEST_BIN; applies only in dedicated temporary bare repositories"]
fn real_mw_local_ledger_persistence_round_trip() {
    let mut f = Fixture::new();
    f.parser = std::env::var_os("MW_TEST_BIN")
        .expect("set MW_TEST_BIN explicitly")
        .into();
    exercise_round_trip(f);
}
