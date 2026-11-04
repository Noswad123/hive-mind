#![cfg(unix)]
use hive_capture::{
    BootstrapPolicy, DestinationBinding, GoogleTask,
    handoff::{TaskHandoff, export_handoff},
    store::CaptureStore,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    queue: PathBuf,
    parser: PathBuf,
    commit: String,
    capture_id: String,
    account: String,
    receipt: Value,
}

const NOTE_PREFIX: &str = "---\nid: source-note\ndomains: [task]\ntask_active: true\ntask_scope: project\n---\n\n## Todo\n### Do Now\n\n### Inbox\n";

fn provider_task() -> Value {
    json!({"kind":"tasks#task", "id":"task-one", "title":"Buy milk", "status":"needsAction",
        "etag":"\"fixture-etag\"", "updated":"2026-11-03T12:00:00.000Z",
        "selfLink":"https://www.googleapis.com/tasks/v1/lists/list-one/tasks/task-one", "position":"00000000000000000001",
        "webViewLink":"https://tasks.google.com/task/fixture-task-one"})
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap().trim().into()
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        let queue = root.join("queue.sqlite");
        let mut store = CaptureStore::open(&queue).unwrap();
        let identity = hive_core::GoogleIdentity::new("fixture-subject").unwrap();
        let account = identity.namespace().unwrap();
        let task: GoogleTask = serde_json::from_value(provider_task()).unwrap();
        store
            .ingest_verified(&identity, "list-one", &[task], BootstrapPolicy::ImportOpen)
            .unwrap();
        let capture_id = store.jobs().unwrap()[0].capture_id.clone();
        let binding = DestinationBinding {
            notes_dir: repo.to_str().unwrap().into(),
            inbox_path: repo.join("inbox.md").to_str().unwrap().into(),
            dashboard_path: repo.join("dashboard.md").to_str().unwrap().into(),
        };
        store.claim(&capture_id, &binding).unwrap();
        let receipt = json!({"schemaVersion":"1","method":"todos-add","binding":binding,
            "result":{"schemaVersion":"1","alreadyCaptured":false,"projectionUpdated":true,
            "task":{"id":format!("task:{capture_id}"),"stableId":capture_id,"identityKind":"stable",
                "sourceRevision":format!("sha256:{}", "a".repeat(64)),"sourceId":"inbox-source",
                "sourcePath":"inbox.md","done":false,"text":"Buy milk","todoSection":"Inbox"}}});
        store.complete(&capture_id, &receipt).unwrap();
        drop(store);
        let mut f = Self {
            parser: root.join("mw"),
            commit: String::new(),
            root,
            repo,
            queue,
            capture_id,
            account,
            receipt,
            _temp: temp,
        };
        f.source("tasks.md", "2026-11-10", "2026-11-10", "", false);
        f
    }

    fn source(&mut self, path: &str, date: &str, parser_date: &str, raw: &str, done: bool) {
        for old in ["tasks.md", "moved.md"] {
            if self.repo.join(old).exists() {
                fs::remove_file(self.repo.join(old)).unwrap();
            }
        }
        let text = format!(
            "{NOTE_PREFIX}- [{}] Renamed in source\n  - due:{date}\n  <!-- mw-task:v1:{} -->\n### Blocked\n",
            if done { "x" } else { " " },
            self.capture_id
        );
        fs::write(self.repo.join(path), &text).unwrap();
        git(&self.repo, &["add", "--all"]);
        git(
            &self.repo,
            &["commit", "--quiet", "--allow-empty", "-m", "fixture"],
        );
        self.commit = git(&self.repo, &["rev-parse", "HEAD"]);
        let item = json!({"id":format!("task:{}",self.capture_id),"stableId":self.capture_id,
            "identityKind":"stable","sourceRevision":format!("sha256:{:x}",Sha256::digest(text.as_bytes())),
            "sourceId":"source-note","sourcePath":path,"line":NOTE_PREFIX.lines().count()+1,"text":"Renamed in source","done":done,
            "metadata":{"due":parser_date,"raw":if raw.is_empty() {format!("due:{date}")} else {raw.into()},"start":"","estimate":""}});
        self.stub(&[item]);
    }

    fn stub(&self, items: &[Value]) {
        let items = serde_json::to_string(items).unwrap();
        assert!(!items.contains('\''));
        fs::write(&self.parser, format!("#!/bin/sh\nset -eu\ntest \"$*\" = 'todos snapshot --json'\ntest -z \"${{HIVE_GOOGLE_ACCESS_TOKEN+x}}\"\nprintf '{{\"schemaVersion\":\"1\",\"source\":\"saved-markdown\",\"notesDir\":\"%s\",\"dashboardPath\":\"%s\",\"items\":{items}}}' \"$NOTES_DIR\" \"$DASHBOARD_PATH\"\n")).unwrap();
        fs::set_permissions(&self.parser, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn item(&self, line: usize, stable: &str, due: &str, raw: &str) -> Value {
        json!({"id":format!("task:{stable}"),"stableId":stable,"identityKind":"stable",
            "sourceRevision":"", "sourceId":"source-note","sourcePath":"tasks.md","line":line,
            "text":"Fixture task","done":false,"metadata":{"due":due,"raw":raw,"start":"","estimate":""}})
    }

    fn evidence_source(&mut self, text: &str, mut items: Vec<Value>) {
        fs::write(self.repo.join("tasks.md"), text).unwrap();
        git(&self.repo, &["add", "tasks.md"]);
        git(
            &self.repo,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "evidence fixture",
            ],
        );
        self.commit = git(&self.repo, &["rev-parse", "HEAD"]);
        for item in &mut items {
            item["sourceRevision"] = json!(format!("sha256:{:x}", Sha256::digest(text.as_bytes())));
        }
        self.stub(&items);
    }

    fn historical_sources(&mut self, marker: &str) {
        let inactive = NOTE_PREFIX
            .replace("id: source-note", "id: inactive-note")
            .replace("task_active: true", "task_active: false");
        fs::write(self.repo.join("inactive.md"), format!("{inactive}- [x] Historical parent\n  - [x] Historical child\n    <!-- mw-task:v1:{marker} -->\n### Blocked\n")).unwrap();
        fs::write(self.repo.join("archive.md"), format!("---\nid: mw-archive-cccccccccccccccccccccccccccccccc\ntask_active: false\n---\n<!-- mw-archive-ledger:v1 -->\n<!-- mw-archive-receipt:v1:7b227068617365223a22636f6d706c65746564227d -->\n- [x] Archived completed parent\n  - [x] Archived completed child\n    <!-- mw-task:v1:{marker} -->\n\n<!-- mw-archive-end:v1 -->\n")).unwrap();
        git(&self.repo, &["add", "inactive.md", "archive.md"]);
        git(
            &self.repo,
            &["commit", "--quiet", "-m", "historical fixture"],
        );
        self.commit = git(&self.repo, &["rev-parse", "HEAD"]);
    }

    fn handoff_output(&self, accept: bool) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hive"));
        cmd.args(["tasks", "handoff", "--queue"])
            .arg(&self.queue)
            .args([
                "--capture-id",
                &self.capture_id,
                "--source-repo-id",
                "notes-fixture",
            ]);
        if accept {
            cmd.arg("--accept-current-baseline");
        }
        cmd.env("HOME", &self.root).output().unwrap()
    }

    fn handoff(&self) -> Value {
        let output = self.handoff_output(true);
        assert!(output.status.success(), "{:?}", output.stderr);
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn observation(&self) -> Value {
        json!({"schemaVersion":"1","kind":"google-task-observation","sourceRepoId":"notes-fixture",
            "accountKey":self.account,"listId":"list-one","task":provider_task()})
    }

    fn plan_output(&self, handoff: &Value, observation: &Value) -> Output {
        fs::write(
            self.root.join("handoff.json"),
            serde_json::to_vec(handoff).unwrap(),
        )
        .unwrap();
        fs::write(
            self.root.join("observation.json"),
            serde_json::to_vec(observation).unwrap(),
        )
        .unwrap();
        Command::new(env!("CARGO_BIN_EXE_hive"))
            .args(["tasks", "due-plan", "--repo"])
            .arg(&self.repo)
            .args(["--commit", &self.commit, "--mw-bin"])
            .arg(&self.parser)
            .args(["--source-repo-id", "notes-fixture", "--handoff"])
            .arg(self.root.join("handoff.json"))
            .arg("--observation")
            .arg(self.root.join("observation.json"))
            .env("HOME", &self.root)
            .env("HIVE_GOOGLE_ACCESS_TOKEN", "must-not-cross")
            .output()
            .unwrap()
    }

    fn plan(&self, handoff: &Value, observation: &Value) -> Value {
        let out = self.plan_output(handoff, observation);
        assert!(out.status.success(), "{:?}", out.stderr);
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

#[test]
fn handoff_requires_acceptance_and_exports_only_portable_selected_evidence_read_only() {
    let f = Fixture::new();
    let before = fs::read(&f.queue).unwrap();
    assert!(!f.handoff_output(false).status.success());
    let h = f.handoff();
    assert_eq!(h["captureId"], f.capture_id);
    assert_eq!(h["receipt"]["method"], "todos-add");
    assert!(h.get("binding").is_none());
    assert!(!h.to_string().contains(f.root.to_str().unwrap()));
    assert_eq!(fs::read(&f.queue).unwrap(), before);
    assert!(!f.queue.with_extension("sqlite-journal").exists());
    let (mut job, _) = CaptureStore::inspect_selected(&f.queue, &f.capture_id).unwrap();
    job.receipt = Some(
        json!({"schemaVersion":"1","method":"snapshot-reconciliation",
        "binding":f.receipt["binding"],"task":f.receipt["result"]["task"]}),
    );
    assert!(export_handoff(&job, "notes-fixture", true).is_ok());
    assert!(export_handoff(&job, "notes-fixture", false).is_err());
    for change in [
        "missing",
        "identity",
        "revision",
        "binding",
        "completed",
        "changed",
        "review",
        "observation",
    ] {
        let (mut job, _) = CaptureStore::inspect_selected(&f.queue, &f.capture_id).unwrap();
        match change {
            "missing" => job.receipt = None,
            "identity" => {
                job.receipt.as_mut().unwrap()["result"]["task"]["stableId"] = json!("b".repeat(32))
            }
            "revision" => {
                job.receipt.as_mut().unwrap()["result"]["task"]["sourceRevision"] =
                    json!("sha256:wrong")
            }
            "binding" => job.receipt.as_mut().unwrap()["binding"]["inboxPath"] = json!("/wrong"),
            "completed" => job.receipt.as_mut().unwrap()["result"]["task"]["done"] = json!(true),
            "changed" => job.provider_changed = true,
            "review" => job.state = hive_capture::DeliveryState::NeedsReview,
            _ => job.observation.id = "wrong".into(),
        }
        assert!(
            export_handoff(&job, "notes-fixture", true).is_err(),
            "{change}"
        );
    }
}

#[test]
fn eligible_patch_is_due_only_exact_commit_pinned_and_identity_survives_moves() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    fs::write(f.repo.join("tasks.md"), "- [x] dirty due:2030-01-01\n").unwrap();
    let status = git(&f.repo, &["status", "--porcelain=v1"]);
    let p = f.plan(&h, &o);
    assert_eq!(p["decision"], "set_first_due");
    assert_eq!(p["patch"], json!({"due":"2026-11-10T00:00:00.000Z"}));
    for field in [
        "publicationReady",
        "remoteVerified",
        "pushVerified",
        "conditionalWriteSafe",
        "hiveWriteProven",
    ] {
        assert_eq!(p[field], false);
    }
    assert_eq!(p["observationTrust"], "operator-asserted");
    assert_eq!(p["commit"], f.commit);
    assert_eq!(git(&f.repo, &["status", "--porcelain=v1"]), status);
    f.source("moved.md", "2026-11-10", "2026-11-10", "", false);
    let moved = f.plan(&h, &o);
    assert_eq!(moved["decision"], "set_first_due");
    assert_eq!(moved["source"]["path"], "moved.md");
}

#[test]
fn mapping_baseline_and_supplied_remote_conflicts_fail_closed() {
    let f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    for field in [
        "sourceRepoId",
        "accountKey",
        "listId",
        "providerId",
        "captureId",
    ] {
        let mut bad = h.clone();
        bad[field] = json!("wrong");
        assert_eq!(f.plan(&bad, &o)["decision"], "blocked", "{field}");
    }
    let mut bad = h.clone();
    bad["acceptedCurrentBaseline"] = json!(false);
    assert_eq!(f.plan(&bad, &o)["decision"], "blocked");
    for (field, value) in [
        ("title", json!("Changed")),
        ("status", json!("completed")),
        ("deleted", json!(true)),
        ("hidden", json!(true)),
        ("notes", json!("New notes")),
        ("parent", json!("parent")),
        ("assignmentInfo", json!({"x":true})),
        ("futureField", json!(true)),
    ] {
        let mut bad = o.clone();
        bad["task"][field] = value;
        assert_eq!(f.plan(&h, &bad)["decision"], "blocked", "{field}");
    }
    for field in ["accountKey", "listId", "sourceRepoId"] {
        let mut bad = o.clone();
        bad[field] = json!("wrong");
        assert_eq!(f.plan(&h, &bad)["decision"], "blocked");
    }
    let mut remote = o.clone();
    remote["task"]["etag"] = json!("new");
    remote["task"]["updated"] = json!("later");
    assert_eq!(f.plan(&h, &remote)["decision"], "set_first_due");
    remote["task"]["due"] = json!("2026-11-10T00:00:00.000Z");
    let same = f.plan(&h, &remote);
    assert_eq!(same["decision"], "already_satisfied");
    assert_eq!(same["hiveWriteProven"], false);
    assert!(same.get("patch").is_none());
    remote["task"]["due"] = json!("2026-11-11T00:00:00.000Z");
    assert_eq!(f.plan(&h, &remote)["decision"], "blocked");
}

#[test]
fn literal_due_validation_completion_and_missing_identity_are_independent_of_calendar() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    for (date, parsed, raw, expected) in [
        ("2028-02-29", "2028-02-29", "", "set_first_due"),
        ("9999-12-31", "9999-12-31", "", "set_first_due"),
        ("2026-02-29", "2026-02-29", "", "blocked"),
        ("2026-13-01", "2026-13-01", "", "blocked"),
        ("0000-01-01", "0000-01-01", "", "blocked"),
        ("2026-11-10", "2026-11-11", "", "blocked"),
        ("2026-11-10", "2026-11-10", "due:2026-11-11", "blocked"),
        ("", "", "", "blocked"),
        ("2026-11-10 due:2026-11-11", "2026-11-10", "", "blocked"),
    ] {
        f.source("tasks.md", date, parsed, raw, false);
        assert_eq!(f.plan(&h, &o)["decision"], expected, "{date} / {raw}");
    }
    f.source("tasks.md", "2026-11-10", "2026-11-10", "", true);
    assert_eq!(f.plan(&h, &o)["decision"], "blocked");
    f.stub(&[]);
    assert_eq!(f.plan(&h, &o)["decision"], "blocked");
}

#[test]
fn standard_google_metadata_is_validated_but_is_not_semantic_or_identity_evidence() {
    let f = Fixture::new();
    let h = f.handoff();
    assert_eq!(h["baseline"]["kind"], "tasks#task");
    assert_eq!(h["baseline"]["position"], "00000000000000000001");
    assert_eq!(
        h["baseline"]["webViewLink"],
        "https://tasks.google.com/task/fixture-task-one"
    );
    let mut o = f.observation();
    o["task"]["position"] = json!("00000000000000000009");
    o["task"]["selfLink"] =
        json!("https://tasks.googleapis.com/tasks/v1/lists/another-list/tasks/another-task");
    o["task"]["webViewLink"] = json!("https://tasks.google.com/task/changed-display-link");
    let p = f.plan(&h, &o);
    assert_eq!(p["decision"], "set_first_due");
    assert_eq!(p["providerId"], "task-one");
    for (field, value) in [
        ("kind", json!("tasks#taskList")),
        ("position", json!(123)),
        ("position", json!("bad\nvalue")),
        (
            "selfLink",
            json!("https://evil.invalid/tasks/v1/lists/L/tasks/T"),
        ),
        (
            "selfLink",
            json!("https://www.googleapis.com/tasks/v1/lists/L/tasks/T?access_token=secret"),
        ),
        ("futureField", json!("unknown")),
        ("webViewLink", json!(123)),
        ("webViewLink", json!("not a URL")),
        ("webViewLink", json!("javascript:alert(1)")),
        ("webViewLink", json!("http://tasks.google.com/task/T")),
        ("webViewLink", json!("https://evil.invalid/task/T")),
        (
            "webViewLink",
            json!("https://user:password@tasks.google.com/task/T"),
        ),
        ("webViewLink", json!("https://tasks.google.com:444/task/T")),
        ("webViewLink", json!("https://tasks.google.com/task/T\n")),
    ] {
        let mut bad = f.observation();
        bad["task"][field] = value.clone();
        assert_eq!(f.plan(&h, &bad)["decision"], "blocked", "{field}");
        let (mut job, _) = CaptureStore::inspect_selected(&f.queue, &f.capture_id).unwrap();
        job.observation.extra.insert(field.into(), value);
        assert!(
            export_handoff(&job, "notes-fixture", true).is_err(),
            "{field}"
        );
    }
}

#[test]
fn frozen_inbox_aliases_use_the_same_read_only_canonical_checks_as_the_writer() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    fs::create_dir(f.repo.join("section")).unwrap();
    fs::write(f.repo.join("inbox.md"), "isolated capture source").unwrap();
    symlink("inbox.md", f.repo.join("alias.md")).unwrap();
    for alias in ["section/../inbox.md", "alias.md"] {
        let (mut job, _) = CaptureStore::inspect_selected(&f.queue, &f.capture_id).unwrap();
        job.binding.as_mut().unwrap().inbox_path = f.repo.join(alias).to_str().unwrap().into();
        job.receipt.as_mut().unwrap()["binding"] = serde_json::to_value(&job.binding).unwrap();
        let before = fs::read(f.repo.join("inbox.md")).unwrap();
        assert!(
            export_handoff(&job, "notes-fixture", true).is_ok(),
            "{alias}"
        );
        assert_eq!(fs::read(f.repo.join("inbox.md")).unwrap(), before);
    }
    fs::create_dir_all(f.root.join("outside/section")).unwrap();
    fs::write(f.root.join("outside/inbox.md"), "different Inbox").unwrap();
    symlink(
        f.root.join("outside/section"),
        f.repo.join("linked-section"),
    )
    .unwrap();
    let (mut job, _) = CaptureStore::inspect_selected(&f.queue, &f.capture_id).unwrap();
    job.binding.as_mut().unwrap().inbox_path = f
        .repo
        .join("linked-section/../inbox.md")
        .to_str()
        .unwrap()
        .into();
    job.receipt.as_mut().unwrap()["binding"] = serde_json::to_value(&job.binding).unwrap();
    assert!(export_handoff(&job, "notes-fixture", true).is_err());
}

#[test]
#[ignore = "requires explicit MW_TEST_BIN; reads only isolated temporary committed notes"]
fn real_mw_canonical_marker_and_due_metadata_produce_a_due_only_plan() {
    let mut f = Fixture::new();
    f.historical_sources(&"b".repeat(32));
    let parser = std::env::var_os("MW_TEST_BIN").expect("set trusted MW_TEST_BIN explicitly");
    f.parser = PathBuf::from(parser);
    let original = fs::read(f.repo.join("tasks.md")).unwrap();
    let index = fs::read(f.repo.join(".git/index")).unwrap();
    let p = f.plan(&f.handoff(), &f.observation());
    assert_eq!(p["decision"], "set_first_due", "{p}");
    assert_eq!(p["patch"], json!({"due":"2026-11-10T00:00:00.000Z"}));
    assert_eq!(p["source"]["metadata"]["raw"], "due:2026-11-10");
    assert_eq!(fs::read(f.repo.join("tasks.md")).unwrap(), original);
    assert_eq!(fs::read(f.repo.join(".git/index")).unwrap(), index);
    assert!(!f.repo.join(".mw").exists());
    let capture = f.capture_id.clone();
    f.historical_sources(&capture);
    let p = f.plan(&f.handoff(), &f.observation());
    assert_eq!(p["decision"], "blocked");
    assert_eq!(p["reason"], "committed source marker is ambiguous");
}

#[test]
fn indented_siblings_blank_lines_and_headings_bound_metadata_to_its_own_task() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    let other = "a".repeat(32);
    let body = format!(
        "  - [ ] First task\n\n    - due:2026-11-10\n\n    <!-- mw-task:v1:{} -->\n\n  - [ ] Sibling task\n    - due:2026-11-11\n    <!-- mw-task:v1:{other} -->\n### Blocked\n",
        f.capture_id
    );
    let text = format!("{NOTE_PREFIX}{body}");
    let line = NOTE_PREFIX.lines().count() + 1;
    let sibling_line = line + 6;
    let items = vec![
        f.item(line, &f.capture_id, "2026-11-10", "due:2026-11-10"),
        f.item(sibling_line, &other, "2026-11-11", "due:2026-11-11"),
    ];
    f.evidence_source(&text, items);
    let p = f.plan(&h, &o);
    assert_eq!(p["decision"], "set_first_due", "{p}");
    assert!(
        !p["source"]["literalMetadata"]
            .as_str()
            .unwrap()
            .contains("2026-11-11")
    );

    let text = format!(
        "{NOTE_PREFIX}- [ ] First task\n  - due:2026-11-10\n  <!-- mw-task:v1:{} -->\n\n  ### Blocked\n    - due:2026-11-11\n",
        f.capture_id
    );
    let item = f.item(line, &f.capture_id, "2026-11-10", "due:2026-11-10");
    f.evidence_source(&text, vec![item]);
    assert_eq!(f.plan(&h, &o)["decision"], "set_first_due");
}

#[test]
fn nested_child_marker_never_supplies_parent_identity() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    let text = format!(
        "{NOTE_PREFIX}- [ ] Parent task\n  - [ ] Child task\n    - due:2026-11-10\n    <!-- mw-task:v1:{} -->\n### Blocked\n",
        f.capture_id
    );
    let line = NOTE_PREFIX.lines().count() + 1;
    // A misleading parser assertion cannot attach the child's marker to the parent.
    let parent = f.item(line, &f.capture_id, "2026-11-10", "due:2026-11-10");
    f.evidence_source(&text, vec![parent]);
    assert!(!f.plan_output(&h, &o).status.success());
}

#[test]
fn malformed_inline_orphan_and_duplicate_markers_fail_closed_without_legacy_support() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    let id = f.capture_id.clone();
    for body in [
        format!("- [ ] Task <!-- mw-task:{id} -->\n  - due:2026-11-10\n"),
        format!("- [ ] Task <!-- mw-task:v1:{id} -->\n  - due:2026-11-10\n"),
        format!("- [ ] Task\n  - due:2026-11-10\n  <!-- mw-task:{id} -->\n"),
        format!("- [ ] Task\n  - due:2026-11-10\n  <!-- mw-task:v2:{id} -->\n"),
        format!("- [ ] Task\n  - due:2026-11-10\n<!-- mw-task:v1:{id} -->\n"),
        format!(
            "- [ ] Task\n  - due:2026-11-10\n  <!-- mw-task:v1:{id} -->\n  <!-- mw-task:v1:{id} -->\n"
        ),
        format!("- [ ] Task\n  - due:2026-11-10\n  ### Blocked\n    <!-- mw-task:v1:{id} -->\n"),
    ] {
        let item = f.item(
            NOTE_PREFIX.lines().count() + 1,
            &id,
            "2026-11-10",
            "due:2026-11-10",
        );
        f.evidence_source(&format!("{NOTE_PREFIX}{body}### Blocked\n"), vec![item]);
        assert!(!f.plan_output(&h, &o).status.success(), "{body}");
    }
}

#[test]
fn frontmatter_and_fenced_examples_are_not_task_markers_or_due_evidence() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    let prefix = NOTE_PREFIX.replace(
        "domains:",
        "description: '<!-- mw-task:example -->'\ndomains:",
    );
    let line = prefix.lines().count() + 1;
    let text = format!(
        "{prefix}- [ ] Task\n  - due:2026-11-10\n  <!-- mw-task:v1:{} -->\n### Blocked\n\n```markdown\n- [ ] Example\n  - due:2026-11-11\n  <!-- mw-task:invalid-example -->\n```\n",
        f.capture_id
    );
    let item = f.item(line, &f.capture_id, "2026-11-10", "due:2026-11-10");
    f.evidence_source(&text, vec![item]);
    assert_eq!(f.plan(&h, &o)["decision"], "set_first_due");
}

#[test]
fn malformed_or_unknown_json_and_missing_queue_never_create_state() {
    let f = Fixture::new();
    let missing = f.root.join("missing.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_hive"))
        .args(["tasks", "handoff", "--queue"])
        .arg(&missing)
        .args([
            "--capture-id",
            &f.capture_id,
            "--source-repo-id",
            "notes-fixture",
            "--accept-current-baseline",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!missing.exists());
    let mut h = f.handoff();
    h["unexpected"] = json!(true);
    assert!(!f.plan_output(&h, &f.observation()).status.success());
    let mut o = f.observation();
    o["unexpected"] = json!(true);
    assert!(!f.plan_output(&f.handoff(), &o).status.success());
    let mut h = f.handoff();
    h["receipt"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<TaskHandoff>(h).is_err());
    let mut oversized = f.handoff();
    oversized["baseline"]["title"] = json!("x".repeat(65_537));
    assert!(!f.plan_output(&oversized, &f.observation()).status.success());
    let connection = rusqlite::Connection::open(&f.queue).unwrap();
    connection
        .execute("UPDATE identity_bindings SET subject='wrong'", [])
        .unwrap();
    drop(connection);
    assert!(!f.handoff_output(true).status.success());
}

#[test]
fn reverted_provider_edit_remains_sticky_review_and_cannot_be_handed_off() {
    let f = Fixture::new();
    let identity = hive_core::GoogleIdentity::new("fixture-subject").unwrap();
    let mut store = CaptureStore::open_existing(&f.queue).unwrap();
    let original = store.jobs().unwrap()[0].observation.clone();
    let mut changed = original.clone();
    changed.title = "Edited on phone".into();
    for task in [changed, original] {
        store
            .ingest_verified(&identity, "list-one", &[task], BootstrapPolicy::ImportOpen)
            .unwrap();
    }
    drop(store);
    let before = fs::read(&f.queue).unwrap();
    assert!(!f.handoff_output(true).status.success());
    assert_eq!(fs::read(&f.queue).unwrap(), before);
}

#[test]
fn duplicate_committed_marker_blocks_even_when_parser_omits_the_duplicate() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    fs::write(
        f.repo.join("duplicate.md"),
        format!("{NOTE_PREFIX}- [ ] Same title\n  - due:2026-11-10\n  <!-- mw-task:v1:{} -->\n### Blocked\n", f.capture_id),
    )
    .unwrap();
    git(&f.repo, &["add", "duplicate.md"]);
    git(&f.repo, &["commit", "--quiet", "-m", "duplicate fixture"]);
    f.commit = git(&f.repo, &["rev-parse", "HEAD"]);
    assert_eq!(f.plan(&h, &o)["decision"], "blocked");
}

#[test]
fn unrelated_inactive_and_archive_history_is_inventory_not_active_task_grammar() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    f.historical_sources(&"b".repeat(32));
    assert_eq!(f.plan(&h, &o)["decision"], "set_first_due");
    let capture = f.capture_id.clone();
    f.historical_sources(&capture);
    let p = f.plan(&h, &o);
    assert_eq!(p["decision"], "blocked");
    assert_eq!(p["reason"], "committed source marker is ambiguous");
}

#[test]
fn active_note_history_outside_todo_does_not_inherit_the_last_task_owner() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    let active = fs::read_to_string(f.repo.join("tasks.md")).unwrap();
    for marker in ["b".repeat(32), f.capture_id.clone()] {
        let text = format!(
            "{active}\n## History\n- [x] Historical parent\n  - [x] Historical child\n    <!-- mw-task:v1:{marker} -->\n  <!-- mw-task:old-malformed-history -->\n"
        );
        let item = f.item(
            NOTE_PREFIX.lines().count() + 1,
            &f.capture_id,
            "2026-11-10",
            "due:2026-11-10",
        );
        f.evidence_source(&text, vec![item]);
        let p = f.plan(&h, &o);
        assert_eq!(
            p["decision"],
            if marker == f.capture_id {
                "blocked"
            } else {
                "set_first_due"
            }
        );
    }
}

#[test]
fn selected_inspection_rejects_corrupt_sql_mapping_and_ignores_other_records() {
    let f = Fixture::new();
    let conn = rusqlite::Connection::open(&f.queue).unwrap();
    conn.execute(
        "INSERT INTO jobs VALUES ('other', 'other', 'other', 'other', 'not JSON')",
        [],
    )
    .unwrap();
    assert!(f.handoff_output(true).status.success());
    conn.execute(
        "UPDATE jobs SET provider_id='wrong' WHERE capture_id=?1",
        [&f.capture_id],
    )
    .unwrap();
    assert!(!f.handoff_output(true).status.success());
}

#[test]
fn committed_indented_due_metadata_is_required_not_just_a_parser_assertion() {
    let mut f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    let text = format!(
        "{NOTE_PREFIX}- [ ] Buy milk\n  - due:2026-11-10\n  <!-- mw-task:v1:{} -->\n### Blocked\n",
        f.capture_id
    );
    fs::write(f.repo.join("tasks.md"), &text).unwrap();
    git(&f.repo, &["add", "tasks.md"]);
    git(&f.repo, &["commit", "--quiet", "-m", "metadata fixture"]);
    f.commit = git(&f.repo, &["rev-parse", "HEAD"]);
    let item = json!({"id":format!("task:{}", f.capture_id),"stableId":f.capture_id,"identityKind":"stable",
        "sourceRevision":format!("sha256:{:x}",Sha256::digest(text.as_bytes())),"sourceId":"source-note",
        "sourcePath":"tasks.md","line":NOTE_PREFIX.lines().count()+1,"text":"Buy milk","done":false,
        "metadata":{"due":"2026-11-10","raw":"due:2026-11-10","start":"","estimate":""}});
    f.stub(std::slice::from_ref(&item));
    assert_eq!(f.plan(&h, &o)["decision"], "set_first_due");
    let mut bad = item.clone();
    bad["metadata"]["raw"] = json!("");
    f.stub(&[bad]);
    assert_eq!(f.plan(&h, &o)["decision"], "blocked");
    let mut bad = item.clone();
    bad["stableId"] = json!("a".repeat(32));
    bad["id"] = json!(format!("task:{}", "a".repeat(32)));
    f.stub(&[bad]);
    assert_eq!(f.plan(&h, &o)["decision"], "blocked");
    f.stub(&[item.clone(), item]);
    assert!(!f.plan_output(&h, &o).status.success());

    let undated = format!(
        "{NOTE_PREFIX}- [ ] Buy milk\n  <!-- mw-task:v1:{} -->\n### Blocked\n",
        f.capture_id
    );
    fs::write(f.repo.join("tasks.md"), &undated).unwrap();
    git(&f.repo, &["add", "tasks.md"]);
    git(&f.repo, &["commit", "--quiet", "-m", "undated fixture"]);
    f.commit = git(&f.repo, &["rev-parse", "HEAD"]);
    let undated_item = json!({"id":format!("task:{}", f.capture_id),"stableId":f.capture_id,"identityKind":"stable",
        "sourceRevision":format!("sha256:{:x}",Sha256::digest(undated.as_bytes())),"sourceId":"source-note",
        "sourcePath":"tasks.md","line":NOTE_PREFIX.lines().count()+1,"text":"Buy milk","done":false,
        "metadata":{"due":"","raw":"","start":"","estimate":""}});
    f.stub(&[undated_item]);
    let plan = f.plan(&h, &o);
    assert_eq!(plan["decision"], "blocked");
    assert_eq!(plan["reason"], "desired due is absent or duplicated");
}

#[test]
fn tasks_auth_and_observe_help_expose_only_explicit_saved_connection_interface() {
    for args in [
        vec!["tasks", "--help"],
        vec!["tasks", "auth", "--help"],
        vec!["tasks", "auth", "login", "--help"],
        vec!["tasks", "auth", "status", "--help"],
        vec!["tasks", "auth", "logout", "--help"],
        vec!["tasks", "observe", "--help"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_hive"))
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {:?}", out.stderr);
        let help = String::from_utf8(out.stdout).unwrap();
        if args.contains(&"observe") {
            assert!(help.contains("--connection"));
            assert!(help.contains("--handoff"));
            for forbidden in [
                "--access-token-env",
                "--account-key",
                "--list-id",
                "--task-id",
                "--queue",
            ] {
                assert!(!help.contains(forbidden));
            }
        }
    }
}

#[test]
fn invalid_observe_and_auth_arguments_fail_without_native_credential_setup() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.json");
    for args in [
        vec!["tasks", "observe"],
        vec!["tasks", "observe", "--connection", "work"],
        vec![
            "tasks",
            "observe",
            "--connection",
            "../invalid",
            "--handoff",
            "/missing.json",
        ],
        vec![
            "tasks",
            "observe",
            "--connection",
            "work",
            "--handoff",
            "relative.json",
        ],
        vec![
            "tasks",
            "observe",
            "--connection",
            "work",
            "--handoff",
            missing.to_str().unwrap(),
        ],
        vec!["tasks", "auth", "status", "--connection", "../invalid"],
        vec!["tasks", "auth", "logout", "--connection", "../invalid"],
        vec![
            "tasks",
            "auth",
            "login",
            "--connection",
            "work",
            "--client-json",
            missing.to_str().unwrap(),
        ],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_hive"))
            .args(&args)
            .env("HOME", temp.path())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}");
        assert!(out.stdout.is_empty());
    }
    for forbidden in [
        "--access-token-env",
        "--account-key",
        "--list-id",
        "--task-id",
        "--provider-id",
        "--subject",
        "--queue",
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_hive"))
            .args([
                "tasks",
                "observe",
                "--connection",
                "work",
                "--handoff",
                "/unused.json",
                forbidden,
                "canary",
            ])
            .env("HOME", temp.path())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{forbidden}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument"));
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn offline_planner_rejects_live_wrapper_and_forged_provenance_without_upgrading_trust() {
    let f = Fixture::new();
    let h = f.handoff();
    let o = f.observation();
    let wrapper = json!({
        "schemaVersion":"1", "kind":"google-task-live-observation",
        "sourceRepoId":"notes-fixture", "captureId":f.capture_id,
        "accountKey":f.account, "listId":"list-one", "providerId":"task-one",
        "remoteReadVerifiedDuringCollection":true, "portableEvidenceAuthenticated":false,
        "observation":o
    });
    assert!(!f.plan_output(&h, &wrapper).status.success());
    let mut forged = f.observation();
    forged["remoteVerified"] = json!(true);
    forged["remoteReadVerifiedDuringCollection"] = json!(true);
    assert!(!f.plan_output(&h, &forged).status.success());
    let plan = f.plan(&h, &wrapper["observation"]);
    assert_eq!(plan["decision"], "set_first_due");
    assert_eq!(plan["remoteVerified"], false);
    assert_eq!(plan["observationTrust"], "operator-asserted");
}

#[test]
fn fake_cli_runs_across_processes_without_credentials_or_source_writes() {
    let f = Fixture::new();
    let state = f.root.join("fake-state.git");
    let world = f.root.join("fake-world.json");
    let fixture = f.root.join("fake-fixture.json");
    let handoff = f.root.join("fake-handoff.json");
    fs::write(&fixture, serde_json::to_vec(&json!({"schemaVersion":"1","worldId":"synthetic-world","sourceRepoId":"notes-fixture","accountKey":f.account,"listId":"list-one","task":provider_task()})).unwrap()).unwrap();
    fs::write(&handoff, serde_json::to_vec(&f.handoff()).unwrap()).unwrap();
    let source_status = git(&f.repo, &["status", "--porcelain=v1"]);
    let queue_before = fs::read(&f.queue).unwrap();
    let run = |args: &[&str]| -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_hive"))
            .args(["tasks", "fake"])
            .args(args)
            .env_clear()
            .env("HOME", &f.root)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let v: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(v["executionMode"], "fake-only");
        for flag in [
            "googleConditionalWriteValidated",
            "productionReady",
            "publicationReady",
            "pushVerified",
            "remoteVerified",
            "hiveWriteProven",
        ] {
            assert_eq!(v[flag], false);
        }
        v
    };
    let state = state.to_str().unwrap();
    let world = world.to_str().unwrap();
    run(&[
        "init",
        "--state-repo",
        state,
        "--world",
        world,
        "--fixture",
        fixture.to_str().unwrap(),
    ]);
    let execute = [
        "execute-first-due",
        "--state-repo",
        state,
        "--world",
        world,
        "--repo",
        f.repo.to_str().unwrap(),
        "--commit",
        &f.commit,
        "--mw-bin",
        f.parser.to_str().unwrap(),
        "--source-repo-id",
        "notes-fixture",
        "--handoff",
        handoff.to_str().unwrap(),
    ];
    let applied = run(&execute);
    assert_eq!(applied["phase"], "Applied");
    assert_eq!(applied["durablePatchCount"], 1);
    let retried = run(&execute);
    assert_eq!(retried["durablePatchCount"], 1);
    let inspected = run(&["inspect", "--state-repo", state]);
    assert_eq!(inspected["operations"].as_array().unwrap().len(), 1);
    fs::remove_file(&f.parser).unwrap();
    let recovered = run(&[
        "recover",
        "--state-repo",
        state,
        "--world",
        world,
        "--operation-id",
        applied["operationId"].as_str().unwrap(),
    ]);
    assert_eq!(recovered["phase"], "Applied");
    assert_eq!(recovered["durablePatchCount"], 1);
    assert_eq!(git(&f.repo, &["status", "--porcelain=v1"]), source_status);
    assert_eq!(fs::read(&f.queue).unwrap(), queue_before);
}

#[test]
fn fake_help_has_no_effects_and_no_live_dispatch_options() {
    let temp = tempfile::tempdir().unwrap();
    for subcommand in [
        None,
        Some("init"),
        Some("execute-first-due"),
        Some("recover"),
        Some("inspect"),
    ] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hive"));
        cmd.args(["tasks", "fake"]);
        if let Some(subcommand) = subcommand {
            cmd.arg(subcommand);
        }
        let out = cmd
            .arg("--help")
            .env("HOME", temp.path())
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        let help = String::from_utf8(out.stdout).unwrap();
        for forbidden in [
            "--connection",
            "--endpoint",
            "--bearer",
            "--live",
            "--queue",
            "--push",
        ] {
            assert!(!help.contains(forbidden));
        }
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}
