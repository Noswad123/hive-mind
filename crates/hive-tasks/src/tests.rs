use super::*;
use hive_capture::{handoff::ReceiptEvidence, store::deterministic_capture_id};
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    _temp: tempfile::TempDir,
    state: PathBuf,
    world: PathBuf,
    repo: PathBuf,
    parser: PathBuf,
    commit: String,
    handoff: TaskHandoff,
    fixture: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("source");
        fs::create_dir(&repo).unwrap();
        storage::git(&repo, &["init", "--template=", "."], b"").unwrap();
        let account = hive_core::GoogleIdentity::new("synthetic-subject")
            .unwrap()
            .namespace()
            .unwrap();
        let capture = deterministic_capture_id(&account, "fake-list", "fake-task").unwrap();
        let baseline: GoogleTask = serde_json::from_value(
            json!({"id":"fake-task","title":"Synthetic task","status":"needsAction"}),
        )
        .unwrap();
        let handoff = TaskHandoff {
            schema_version: "1".into(),
            kind: "google-task-handoff".into(),
            source_repo_id: "synthetic-repo".into(),
            capture_id: capture.clone(),
            account_key: account.clone(),
            list_id: "fake-list".into(),
            provider_id: "fake-task".into(),
            accepted_current_baseline: true,
            baseline: baseline.clone(),
            receipt: ReceiptEvidence {
                method: "todos-add".into(),
                sha256: "a".repeat(64),
                stable_id: capture.clone(),
                source_id: "synthetic-note".into(),
                source_path: "tasks.md".into(),
                source_revision: format!("sha256:{}", "b".repeat(64)),
            },
        };
        let text = format!(
            "---\nid: synthetic-note\ndomains: [task]\ntask_active: true\ntask_scope: project\n---\n\n## Todo\n### Inbox\n- [ ] Renamed source\n  - due:2026-11-10\n  <!-- mw-task:v1:{capture} -->\n"
        );
        fs::write(repo.join("tasks.md"), &text).unwrap();
        storage::git(&repo, &["add", "tasks.md"], b"").unwrap();
        storage::git(&repo, &["commit", "-m", "synthetic fixture"], b"").unwrap();
        let commit = storage::git(&repo, &["rev-parse", "HEAD"], b"").unwrap();
        let item = json!({"id":format!("task:{capture}"),"stableId":capture,"identityKind":"stable","sourceRevision":format!("sha256:{}", hash(text.as_bytes())),"sourceId":"synthetic-note","sourcePath":"tasks.md","line":10,"text":"Renamed source","done":false,"metadata":{"due":"2026-11-10","raw":"due:2026-11-10","start":"","estimate":""}});
        let parser = root.join("mw");
        fs::write(&parser, format!("#!/bin/sh\nset -eu\ntest \"$*\" = 'todos snapshot --json'\ntest -z \"${{HIVE_GOOGLE_ACCESS_TOKEN+x}}\"\nprintf '{{\"schemaVersion\":\"1\",\"source\":\"saved-markdown\",\"notesDir\":\"%s\",\"dashboardPath\":\"%s\",\"items\":[{item}]}}' \"$NOTES_DIR\" \"$DASHBOARD_PATH\"\n")).unwrap();
        fs::set_permissions(&parser, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = root.join("fixture.json");
        fs::write(&fixture, serde_json::to_vec(&json!({"schemaVersion":"1","worldId":"synthetic-world","sourceRepoId":"synthetic-repo","accountKey":account,"listId":"fake-list","task":baseline})).unwrap()).unwrap();
        let f = Self {
            _temp: temp,
            state: root.join("state.git"),
            world: root.join("world.json"),
            repo,
            parser,
            commit,
            handoff,
            fixture,
        };
        initialize(&f.state, &f.world, &f.fixture).unwrap();
        f
    }
    fn source(&self) -> SourceOptions<'_> {
        SourceOptions {
            repo: &self.repo,
            commit: &self.commit,
            mw_bin: &self.parser,
            source_repo_id: "synthetic-repo",
        }
    }
    fn session(&self) -> FakeSession {
        FakeSession::open(&self.state, &self.world).unwrap()
    }
    fn state(&self) -> State {
        storage::load(&self.state).unwrap().1
    }
    fn world(&self) -> World {
        storage::read(&self.world).unwrap()
    }
    fn mutate(&self, f: impl FnOnce(&mut World)) {
        let mut w = self.world();
        f(&mut w);
        storage::atomic(&self.world, &w).unwrap();
    }
    fn interrupt(&self, stop: Stop) {
        assert!(
            self.session()
                .execute(self.source(), &self.handoff, stop)
                .is_err()
        );
    }
    fn id(&self) -> String {
        self.state()
            .operations
            .last()
            .unwrap()
            .intent
            .operation_id
            .clone()
    }
}

fn flags(report: &Value) {
    assert_eq!(report["executionMode"], "fake-only");
    assert_eq!(report["provider"], "durable-fake");
    for flag in [
        "googleConditionalWriteValidated",
        "productionReady",
        "publicationReady",
        "pushVerified",
        "remoteVerified",
        "hiveWriteProven",
    ] {
        assert_eq!(report[flag], false, "{flag}");
    }
}

#[test]
fn due_only_exact_commit_retry_and_terminal_recovery() {
    let f = Fixture::new();
    let before = f.handoff.baseline.clone();
    fs::write(
        f.repo.join("tasks.md"),
        "dirty worktree must not authorize\n",
    )
    .unwrap();
    let output = f
        .session()
        .execute_first_due(f.source(), &f.handoff)
        .unwrap();
    flags(&output);
    assert_eq!(output["phase"], "Applied");
    assert_eq!(output["fakeApplied"], true);
    let mut after = f.world().task.unwrap();
    assert_eq!(after.due.take().unwrap(), "2026-11-10T00:00:00.000Z");
    assert_eq!(after, before);
    assert_eq!(
        serde_json::to_value(&f.state().operations[0].intent.patch).unwrap(),
        json!({"due":"2026-11-10T00:00:00.000Z"})
    );
    assert_eq!(f.state().operations[0].intent.commit, f.commit);
    f.session()
        .execute_first_due(f.source(), &f.handoff)
        .unwrap();
    f.session().recover(&f.id()).unwrap();
    assert_eq!(f.world().durable_patch_count, 1);
    assert_eq!(f.state().operations.len(), 1);
}

#[test]
fn crash_boundaries_restart_and_no_replay() {
    for (stop, before, recovered, patches) in [
        (
            Stop::AfterPrepared,
            Phase::Prepared,
            Phase::NotDispatched,
            0,
        ),
        (
            Stop::AttemptingCas,
            Phase::Prepared,
            Phase::NotDispatched,
            0,
        ),
        (
            Stop::AfterAttempting,
            Phase::Attempting,
            Phase::Uncertain,
            0,
        ),
        (
            Stop::AfterMutationLostResponse,
            Phase::Attempting,
            Phase::RecoveredSatisfied,
            1,
        ),
        (
            Stop::BeforeOutcome,
            Phase::Attempting,
            Phase::RecoveredSatisfied,
            1,
        ),
        (
            Stop::OutcomeCas,
            Phase::Attempting,
            Phase::RecoveredSatisfied,
            1,
        ),
    ] {
        let f = Fixture::new();
        f.interrupt(stop);
        assert_eq!(f.state().operations[0].phase(), before);
        assert_eq!(f.world().durable_patch_count, patches);
        // Source and parser are not needed after restart.
        fs::remove_file(&f.parser).unwrap();
        fs::remove_dir_all(&f.repo).unwrap();
        let report = f.session().recover(&f.id()).unwrap();
        flags(&report);
        assert_eq!(f.state().operations[0].phase(), recovered);
        assert_eq!(report["fakeApplied"], false);
        if recovered == Phase::RecoveredSatisfied {
            assert_eq!(report["attribution"], "UNKNOWN");
        }
        f.session().recover(&f.id()).unwrap();
        assert_eq!(f.world().durable_patch_count, patches);
    }
}

#[test]
fn interruption_before_intent_has_no_state_or_world_effect() {
    let f = Fixture::new();
    f.interrupt(Stop::BeforeIntent);
    assert!(f.state().operations.is_empty());
    assert_eq!(f.world().durable_patch_count, 0);
}

#[test]
fn fake_revision_race_rejects_without_mutation() {
    let f = Fixture::new();
    let report = f
        .session()
        .execute(f.source(), &f.handoff, Stop::RevisionRace)
        .unwrap();
    assert_eq!(report["phase"], "RejectedConflict");
    assert_eq!(f.world().durable_patch_count, 0);
    assert!(f.world().task.unwrap().due.is_none());
}

#[test]
fn competing_sessions_and_other_state_cannot_dispatch_same_world() {
    let f = Fixture::new();
    let mut session = f.session();
    assert!(FakeSession::open(&f.state, &f.world).is_err());
    let other = f.state.with_file_name("other.git");
    fs::create_dir(&other).unwrap();
    storage::git(&other, &["init", "--bare", "--template=", "."], b"").unwrap();
    storage::create(&other.join("hive-fake-lock"), b"").unwrap();
    storage::save(&other, None, &f.state()).unwrap();
    assert!(FakeSession::open(&other, &f.world).is_err());
    session.execute_first_due(f.source(), &f.handoff).unwrap();
    drop(session);
    assert!(FakeSession::open(&other, &f.world).is_err());
    assert_eq!(f.world().durable_patch_count, 1);
}

#[test]
fn unresolved_pending_cannot_be_bypassed_with_new_commit_or_handoff() {
    let f = Fixture::new();
    f.interrupt(Stop::AfterAttempting);
    let old_id = f.id();
    storage::git(
        &f.repo,
        &["commit", "--allow-empty", "-m", "different source"],
        b"",
    )
    .unwrap();
    let commit = storage::git(&f.repo, &["rev-parse", "HEAD"], b"").unwrap();
    let mut source = f.source();
    source.commit = &commit;
    assert!(f.session().execute_first_due(source, &f.handoff).is_err());
    assert_eq!(f.session().recover(&old_id).unwrap()["phase"], "Uncertain");
    assert!(
        f.session()
            .execute_first_due(f.source(), &f.handoff)
            .is_err()
    );
    assert_eq!(f.state().operations.len(), 1);
    assert_eq!(f.world().durable_patch_count, 0);
}

#[test]
fn recovery_conflicts_stay_blocking_and_missing_never_recreates() {
    for change in ["due", "title", "lifecycle", "identity", "missing"] {
        let f = Fixture::new();
        f.interrupt(Stop::AfterAttempting);
        f.mutate(|w| match change {
            "due" => w.task.as_mut().unwrap().due = Some("2026-11-11T00:00:00.000Z".into()),
            "title" => w.task.as_mut().unwrap().title = "changed".into(),
            "lifecycle" => w.task.as_mut().unwrap().status = "completed".into(),
            "identity" => w.task.as_mut().unwrap().id = "wrong".into(),
            _ => w.task = None,
        });
        let report = f.session().recover(&f.id()).unwrap();
        assert_eq!(
            report["phase"],
            if change == "missing" {
                "Uncertain"
            } else {
                "ConflictNeedsReview"
            }
        );
        assert!(
            f.session()
                .execute_first_due(f.source(), &f.handoff)
                .is_err()
        );
        f.session().recover(&f.id()).unwrap();
        assert_eq!(f.world().durable_patch_count, 0);
    }
}

#[test]
fn matching_recovery_without_our_mutation_has_unknown_attribution() {
    let f = Fixture::new();
    f.interrupt(Stop::AfterAttempting);
    f.mutate(|w| w.task.as_mut().unwrap().due = Some("2026-11-10T00:00:00Z".into()));
    let report = f.session().recover(&f.id()).unwrap();
    assert_eq!(report["phase"], "RecoveredSatisfied");
    assert_eq!(report["attribution"], "UNKNOWN");
    assert_eq!(report["fakeApplied"], false);
    assert_eq!(f.world().durable_patch_count, 0);
}

#[test]
fn hundreds_of_unchanged_recoveries_do_not_commit_or_exhaust_terminal_recovery() {
    let f = Fixture::new();
    f.interrupt(Stop::AfterAttempting);
    let id = f.id();
    let mut session = f.session();
    assert_eq!(session.recover(&id).unwrap()["phase"], "Uncertain");
    let unchanged_ref = storage::check_repo(&f.state).unwrap();
    let unchanged_world = fs::read(&f.world).unwrap();
    for _ in 0..2 {
        for _ in 0..150 {
            let report = session.recover(&id).unwrap();
            assert_eq!(report["phase"], "Uncertain");
            assert_eq!(report["durablePatchCount"], 0);
            assert_eq!(
                report["operations"][0]["phases"],
                json!(["Prepared", "Attempting", "Uncertain"])
            );
        }
        assert_eq!(storage::check_repo(&f.state).unwrap(), unchanged_ref);
        drop(session);
        session = f.session();
    }
    assert_eq!(fs::read(&f.world).unwrap(), unchanged_world);
    f.mutate(|w| w.task.as_mut().unwrap().due = Some("2026-11-10T00:00:00Z".into()));
    let matching_world = fs::read(&f.world).unwrap();
    let report = session.recover(&id).unwrap();
    assert_eq!(report["phase"], "RecoveredSatisfied");
    assert_eq!(report["attribution"], "UNKNOWN");
    assert_eq!(report["fakeApplied"], false);
    assert_eq!(report["durablePatchCount"], 0);
    let terminal_ref = storage::check_repo(&f.state).unwrap();
    assert_ne!(terminal_ref, unchanged_ref);
    session.recover(&id).unwrap();
    assert_eq!(storage::check_repo(&f.state).unwrap(), terminal_ref);
    assert_eq!(fs::read(&f.world).unwrap(), matching_world);
}

#[test]
fn saturated_legacy_uncertain_history_can_still_reach_terminal_without_replay() {
    let f = Fixture::new();
    f.interrupt(Stop::AfterAttempting);
    let id = f.id();
    let (oid, mut state): (_, State) = storage::load(&f.state).unwrap();
    state.operations[0]
        .phases
        .extend(std::iter::repeat_n(Phase::Uncertain, 126));
    state.validate(&f.state).unwrap();
    let saturated_ref = storage::save(&f.state, Some(&oid), &state).unwrap();
    let mut session = f.session();
    assert_eq!(session.recover(&id).unwrap()["phase"], "Uncertain");
    assert_eq!(storage::check_repo(&f.state).unwrap(), saturated_ref);
    f.mutate(|w| w.task.as_mut().unwrap().due = Some("2026-11-10T00:00:00Z".into()));
    let report = session.recover(&id).unwrap();
    assert_eq!(report["phase"], "RecoveredSatisfied");
    assert_eq!(
        report["operations"][0]["phases"],
        json!(["Prepared", "Attempting", "Uncertain", "RecoveredSatisfied"])
    );
    assert_eq!(report["attribution"], "UNKNOWN");
    assert_eq!(report["fakeApplied"], false);
    assert_eq!(report["durablePatchCount"], 0);
}

#[test]
fn already_satisfied_is_recorded_without_dispatch_and_stale_overwrite_is_blocked() {
    let f = Fixture::new();
    f.mutate(|w| w.task.as_mut().unwrap().due = Some("2026-11-10T00:00:00Z".into()));
    let report = f
        .session()
        .execute_first_due(f.source(), &f.handoff)
        .unwrap();
    assert_eq!(report["phase"], "AlreadySatisfied");
    f.mutate(|w| w.task.as_mut().unwrap().due = None);
    f.session()
        .execute_first_due(f.source(), &f.handoff)
        .unwrap();
    assert_eq!(f.world().durable_patch_count, 0);
    assert_eq!(f.state().operations.len(), 1);
}

#[test]
fn unsupported_world_semantics_identity_lifecycle_and_handoff_never_patch() {
    for change in ["title", "status", "due", "identity", "unknown"] {
        let f = Fixture::new();
        f.mutate(|w| {
            let t = w.task.as_mut().unwrap();
            match change {
                "title" => t.title = "changed".into(),
                "status" => t.status = "completed".into(),
                "due" => t.due = Some("2026-11-11T00:00:00.000Z".into()),
                "identity" => t.id = "wrong".into(),
                _ => {
                    t.extra.insert("futureField".into(), json!(true));
                }
            }
        });
        assert!(
            FakeSession::open(&f.state, &f.world)
                .and_then(|mut s| s.execute_first_due(f.source(), &f.handoff))
                .is_err()
        );
        assert!(f.state().operations.is_empty());
        assert_eq!(f.world().durable_patch_count, 0);
    }
    let f = Fixture::new();
    let mut h = f.handoff.clone();
    h.source_repo_id = "wrong".into();
    assert!(f.session().execute_first_due(f.source(), &h).is_err());
    assert_eq!(f.world().durable_patch_count, 0);
}

#[test]
fn strict_state_unknown_fields_schema_transitions_and_world_binding() {
    for change in ["unknown", "schema", "transition", "world", "intent"] {
        let f = Fixture::new();
        f.interrupt(Stop::AfterAttempting);
        let (oid, state): (_, State) = storage::load(&f.state).unwrap();
        let mut value = serde_json::to_value(state).unwrap();
        match change {
            "unknown" => value["future"] = json!(true),
            "schema" => value["manifest"]["schemaVersion"] = json!("2"),
            "transition" => value["operations"][0]["phases"] = json!(["Prepared", "Applied"]),
            "world" => value["manifest"]["worldId"] = json!("other-world"),
            _ => value["operations"][0]["intent"]["patch"]["title"] = json!("forbidden"),
        }
        storage::save(&f.state, Some(&oid), &value).unwrap();
        assert!(FakeSession::open(&f.state, &f.world).is_err(), "{change}");
        assert_eq!(f.world().durable_patch_count, 0);
    }
}

#[test]
fn stale_cas_foreign_refs_and_init_overwrite_refused() {
    let f = Fixture::new();
    assert!(initialize(&f.state, &f.world, &f.fixture).is_err());
    let (old, state): (_, State) = storage::load(&f.state).unwrap();
    storage::save(&f.state, Some(&old), &state).unwrap();
    // Change payload to avoid identical commit objects within the same second.
    let mut changed = state;
    changed.manifest.world_id = "different".into();
    let current = storage::check_repo(&f.state).unwrap();
    storage::save(&f.state, Some(&current), &changed).unwrap();
    assert!(storage::save(&f.state, Some(&old), &changed).is_err());
    let oid = storage::check_repo(&f.state).unwrap();
    storage::git(&f.state, &["update-ref", "refs/heads/foreign", &oid], b"").unwrap();
    assert!(inspect(&f.state).is_err());
    let existing = f.state.with_file_name("existing");
    fs::create_dir(&existing).unwrap();
    assert!(initialize(&existing, &f.world.with_file_name("new-world"), &f.fixture).is_err());
}

#[test]
fn inspect_is_read_only_without_lock_files_and_json_is_bounded() {
    let f = Fixture::new();
    fs::remove_file(f.state.join("hive-fake-lock")).unwrap();
    flags(&inspect(&f.state).unwrap());
    assert!(!f.state.join("hive-fake-lock").exists());
    assert!(FakeSession::open(&f.state, &f.world).is_err());
    fs::write(&f.world, vec![b' '; storage::LIMIT + 1]).unwrap();
    assert!(storage::read::<World>(&f.world).is_err());
}

#[test]
fn checked_in_synthetic_fixture_and_invalid_init_have_no_implicit_adoption() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fixture = root.join("fixture.json");
    let state = root.join("state.git");
    let world = root.join("world.json");
    assert!(FakeSession::open(&state, &world).is_err());
    assert!(!state.exists() && !world.exists());
    let bytes = include_str!("../tests/fixtures/world.json");
    let mut invalid: Value = serde_json::from_str(bytes).unwrap();
    invalid["unknown"] = json!(true);
    fs::write(&fixture, serde_json::to_vec(&invalid).unwrap()).unwrap();
    assert!(initialize(&state, &world, &fixture).is_err());
    assert!(!state.exists() && !world.exists() && !world_lock(&world).exists());
    fs::write(&fixture, bytes).unwrap();
    flags(&initialize(&state, &world, &fixture).unwrap());
    assert_eq!(
        storage::read::<World>(&world).unwrap().durable_patch_count,
        0
    );
    let duplicate = root.join("other.git");
    assert!(initialize(&duplicate, &world, &fixture).is_err());
    assert!(!duplicate.exists());
}

#[test]
fn executor_rejects_invalid_source_date_completion_and_stable_identity() {
    for change in ["date", "done", "stable"] {
        let f = Fixture::new();
        // Keep the parser assertion and committed literal in agreement where relevant.
        let text_path = f.repo.join("tasks.md");
        let mut text = fs::read_to_string(&text_path).unwrap();
        let mut parser = fs::read_to_string(&f.parser).unwrap();
        match change {
            "date" => {
                text = text.replace("2026-11-10", "2026-02-29");
                parser = parser.replace("2026-11-10", "2026-02-29");
            }
            "done" => {
                text = text.replace("- [ ]", "- [x]");
                parser = parser.replace("\"done\":false", "\"done\":true");
            }
            _ => {
                text = text.replace(&f.handoff.capture_id, &"a".repeat(32));
                parser = parser.replace(&f.handoff.capture_id, &"a".repeat(32));
            }
        }
        let old = fs::read_to_string(&text_path).unwrap();
        parser = parser.replace(&hash(old.as_bytes()), &hash(text.as_bytes()));
        fs::write(&text_path, text).unwrap();
        fs::write(&f.parser, parser).unwrap();
        storage::git(&f.repo, &["add", "tasks.md"], b"").unwrap();
        storage::git(&f.repo, &["commit", "-m", "unsupported source"], b"").unwrap();
        let commit = storage::git(&f.repo, &["rev-parse", "HEAD"], b"").unwrap();
        let mut source = f.source();
        source.commit = &commit;
        assert!(
            f.session().execute_first_due(source, &f.handoff).is_err(),
            "{change}"
        );
        assert!(f.state().operations.is_empty());
        assert_eq!(f.world().durable_patch_count, 0);
    }
}
