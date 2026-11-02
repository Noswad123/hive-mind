#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SOURCE: &str = "---\nid: fixture\ndomains: [task]\ntask_active: true\ntask_scope: project\n---\n\n## Todo\n### Do Now\n### Inbox\n- [ ] Example deadline\n  - due:2028-02-29 start:2028-01-01 est:30\n### Blocked\n";

struct Fixture {
    temp: tempfile::TempDir,
    repo: std::path::PathBuf,
    commit: String,
    parser: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        fs::write(repo.join("tasks.md"), SOURCE).unwrap();
        git(&repo, &["add", "tasks.md"]);
        git(
            &repo,
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
        let commit = git(&repo, &["rev-parse", "HEAD"]);
        let parser = temp.path().join("fixture-mw");
        let mut item = json!({
            "id":"fixture:11:0", "stableId":null, "identityKind":"positional",
            "sourceRevision":format!("sha256:{:x}", Sha256::digest(SOURCE.as_bytes())),
            "sourceId":"fixture", "sourcePath":"tasks.md", "line":11,
            "text":"Example deadline", "done":false,
            "metadata":{"due":"2028-02-29","raw":"due:2028-02-29 start:2028-01-01 est:30",
                "start":"2028-01-01","estimate":"30"}
        })
        .to_string();
        // Test-controlled JSON; the stub intentionally has no dependency on Python or jq.
        assert!(!item.contains('\''));
        item = item.replace('%', "%%");
        let script = format!(
            "#!/bin/sh\nset -eu\n\
             test \"$*\" = 'todos snapshot --json'\n\
             test -z \"${{HIVE_GOOGLE_ACCESS_TOKEN+x}}\"\n\
             test -z \"${{NVIM+x}}\"\n\
             test -f \"$MW_CONFIG\"\n\
             test -f \"$NOTES_DIR/tasks.md\"\n\
             printf '{{\"schemaVersion\":\"1\",\"source\":\"saved-markdown\",\"notesDir\":\"%s\",\"dashboardPath\":\"%s\",\"items\":[{item}]}}' \"$NOTES_DIR\" \"$DASHBOARD_PATH\"\n"
        );
        fs::write(&parser, script).unwrap();
        fs::set_permissions(&parser, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            temp,
            repo,
            commit,
            parser,
        }
    }

    fn preview(&self, parser: &Path) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_hive"))
            .args(["calendar", "preview", "--repo"])
            .arg(&self.repo)
            .args(["--commit", &self.commit, "--mw-bin"])
            .arg(parser)
            .env("HOME", self.temp.path())
            .env("NOTES_DIR", self.temp.path().join("not-the-source"))
            .env("MW_CONFIG", self.temp.path().join("poison-config"))
            .env("HIVE_GOOGLE_ACCESS_TOKEN", "fixture-secret-must-not-cross")
            .env("NVIM", "fixture-editor-must-not-cross")
            .output()
            .unwrap()
    }

    fn recommit(&mut self, content: &str) {
        fs::write(self.repo.join("tasks.md"), content).unwrap();
        git(&self.repo, &["add", "tasks.md"]);
        git(
            &self.repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@invalid",
                "commit",
                "--quiet",
                "-m",
                "Update fixture",
            ],
        );
        self.commit = git(&self.repo, &["rev-parse", "HEAD"]);
    }
}

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

#[test]
fn calendar_help_and_invalid_publication_flags_are_credential_free() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["calendar", "--help"],
        vec!["calendar", "preview", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_hive"))
            .args(args)
            .env("HOME", temp.path())
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    for arg in ["publish", "apply", "login"] {
        assert!(
            !Command::new(env!("CARGO_BIN_EXE_hive"))
                .args(["calendar", arg])
                .env("HOME", temp.path())
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn preview_uses_the_commit_and_never_assigns_identity_or_changes_source() {
    let f = Fixture::new();
    let working = SOURCE.replace("2028-02-29", "2030-01-01");
    fs::write(f.repo.join("tasks.md"), &working).unwrap();
    fs::write(f.repo.join("untracked.md"), "not committed").unwrap();
    let status = git(&f.repo, &["status", "--porcelain=v1"]);
    let output = f.preview(&f.parser);
    assert!(output.status.success(), "{:?}", output.stderr);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["kind"], "calendar-due-preview");
    assert_eq!(value["commit"], f.commit);
    assert_eq!(value["publicationReady"], false);
    assert_eq!(value["counts"]["validDue"], 1);
    let item = &value["items"][0];
    assert_eq!(item["identityKind"], "positional");
    assert!(item.get("stableId").is_none());
    assert_eq!(item["publicationReady"], false);
    assert_eq!(item["prospectiveEvent"]["start"]["date"], "2028-02-29");
    assert_eq!(item["prospectiveEvent"]["end"]["date"], "2028-03-01");
    assert_eq!(item["prospectiveEvent"]["transparency"], "transparent");
    assert_eq!(
        fs::read_to_string(f.repo.join("tasks.md")).unwrap(),
        working
    );
    assert_eq!(git(&f.repo, &["status", "--porcelain=v1"]), status);
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), f.commit);
}

#[test]
#[ignore = "requires explicit MW_TEST_BIN; uses a temporary Git repository, never live notes"]
fn real_mw_calendar_preview_is_commit_pinned_and_database_free() {
    let mut f = Fixture::new();
    let parser = std::env::var_os("MW_TEST_BIN").expect("set MW_TEST_BIN explicitly");
    fs::write(
        f.repo.join("tasks.md"),
        "uncommitted content is not the source",
    )
    .unwrap();
    let output = f.preview(Path::new(&parser));
    assert!(output.status.success(), "{:?}", output.stderr);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["commit"], f.commit);
    assert_eq!(value["publicationReady"], false);
    assert_eq!(value["counts"]["validDue"], 1);
    assert_eq!(
        value["items"][0]["prospectiveEvent"]["summary"],
        "Example deadline"
    );
    assert_eq!(
        value["items"][0]["prospectiveEvent"]["start"]["date"],
        "2028-02-29"
    );
    assert_eq!(
        value["items"][0]["prospectiveEvent"]["end"]["date"],
        "2028-03-01"
    );
    assert!(!f.temp.path().join(".config").exists());
    assert!(!f.temp.path().join(".local").exists());

    // MW normalizes inline area metadata. Calendar must validate the literal
    // committed due token rather than accepting the normalized title as evidence.
    let malformed = SOURCE
        .replace("Example deadline", "Ship due: area:Code 2028-02-29")
        .replace("due:2028-02-29 start:", "start:");
    f.recommit(&malformed);
    let output = f.preview(Path::new(&parser));
    assert!(output.status.success(), "{:?}", output.stderr);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["counts"]["validDue"], 0);
    assert_eq!(value["counts"]["invalidDue"], 1);
    assert!(value["items"][0].get("prospectiveEvent").is_none());
    assert!(
        value["items"][0]["diagnostics"]
            .as_array()
            .unwrap()
            .contains(&json!("date_invalid"))
    );

    f.recommit(&SOURCE.replace("due:", "Due :"));
    let output = f.preview(Path::new(&parser));
    assert!(output.status.success(), "{:?}", output.stderr);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["counts"]["validDue"], 0);
    assert_eq!(value["counts"]["invalidDue"], 1);
    assert!(value["items"][0].get("prospectiveEvent").is_none());
    assert!(
        value["items"][0]["diagnostics"]
            .as_array()
            .unwrap()
            .contains(&json!("parser_due_mismatch"))
    );
}
