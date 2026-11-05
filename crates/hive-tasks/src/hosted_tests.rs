use super::*;
use hive_capture::{handoff::ReceiptEvidence, store::deterministic_capture_id};
use hive_google_auth::{
    AuthProfile, AuthorizationGrant, ClientConfig, CredentialStore, GoogleIdentity, SecretString,
    TokenSet,
};
use std::{
    fs,
    io::Write,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    thread,
};

const SUBJECT: &str = "hosted-raw-subject-canary";
const REFRESH: &str = "hosted-refresh-canary";
pub(super) const ACCESS: &str = "hosted-access-canary";
const CLIENT: &str = "hosted-client-canary";
fn identity(s: &str) -> GoogleIdentity {
    GoogleIdentity::new(s).unwrap()
}
fn secret(s: &str) -> SecretString {
    SecretString::new(s).unwrap()
}
pub(super) fn wire() -> Value {
    json!({"version":1,"connection":"work","identity":identity(SUBJECT),"generation":7,
        "grant":{"client":{"client_id":"synthetic.apps.googleusercontent.com","client_secret":CLIENT},
        "refresh_token":REFRESH,"scopes":AuthProfile::TasksWrite.scopes()}})
}
pub(super) struct Fixture {
    _temp: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) repo: PathBuf,
    pub(super) parser: PathBuf,
    binding: PathBuf,
    event: PathBuf,
    pub(super) commit: String,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("source");
        fs::create_dir(&repo).unwrap();
        storage::git(&repo, &["init", "--template=", "-b", "main", "."], b"").unwrap();
        let account = identity(SUBJECT).namespace().unwrap();
        let capture = deterministic_capture_id(&account, "list/one+", "task/one+").unwrap();
        let handoff = TaskHandoff {
            schema_version: "1".into(),
            kind: "google-task-handoff".into(),
            source_repo_id: "hosted-source".into(),
            capture_id: capture.clone(),
            account_key: account.clone(),
            list_id: "list/one+".into(),
            provider_id: "task/one+".into(),
            accepted_current_baseline: true,
            baseline: serde_json::from_value(
                json!({"id":"task/one+","title":"private-title-canary","status":"needsAction"}),
            )
            .unwrap(),
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
            "---\nid: synthetic-note\ndomains: [task]\ntask_active: true\ntask_scope: project\n---\n\n## Todo\n### Inbox\n- [ ] private-source-canary\n  - due:2026-11-10\n  <!-- mw-task:v1:{capture} -->\n"
        );
        fs::write(repo.join("tasks.md"), &text).unwrap();
        storage::git(&repo, &["add", "tasks.md"], b"").unwrap();
        storage::git(&repo, &["commit", "-m", "synthetic source"], b"").unwrap();
        let commit = head(&repo).unwrap();
        let item = json!({"id":format!("task:{capture}"),"stableId":capture,"identityKind":"stable",
            "sourceRevision":format!("sha256:{:x}",Sha256::digest(text.as_bytes())),"sourceId":"synthetic-note",
            "sourcePath":"tasks.md","line":10,"text":"private-source-canary","done":false,
            "metadata":{"due":"2026-11-10","raw":"due:2026-11-10","start":"","estimate":""}});
        let parser = root.join("mw-stub");
        fs::write(&parser, format!("#!/bin/sh\nset -eu\ntest \"${{HIVE_HOSTED_TASKS_WRITE_RECORD+x}}\" != x\ntest \"${{GOOGLE_ACCESS_TOKEN+x}}\" != x\ntest \"$*\" = 'todos snapshot --json'\nprintf '{{\"schemaVersion\":\"1\",\"source\":\"saved-markdown\",\"notesDir\":\"%s\",\"dashboardPath\":\"%s\",\"items\":[{item}]}}' \"$NOTES_DIR\" \"$DASHBOARD_PATH\"\n")).unwrap();
        fs::set_permissions(&parser, fs::Permissions::from_mode(0o700)).unwrap();
        let binding = root.join("binding.json");
        fs::write(&binding, serde_json::to_vec(&json!({"schemaVersion":"1","kind":"google-tasks-hosted-binding",
            "expectedRepositoryId":"123","expectedRepository":"owner/repo","sourceRepoId":"hosted-source",
            "connection":"work","expectedAccountNamespace":account,"trustedParserSha256":parser_hash(&parser).unwrap(),
            "reviewedTaskHandoff":handoff,"handoffSha256":format!("{:x}", Sha256::digest(serde_json::to_vec(&serde_json::to_value(&handoff).unwrap()).unwrap()))})).unwrap()).unwrap();
        let event = root.join("event.json");
        fs::write(
            &event,
            json!({"ref":"refs/heads/main","deleted":false,"after":commit,
            "repository":{"id":123,"full_name":"owner/repo","unrelated":"allowed"},"commits":[]})
            .to_string(),
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            repo,
            parser,
            binding,
            event,
            commit,
        }
    }
    pub(super) fn claims(&self) -> PushClaims {
        PushClaims {
            actions: "true".into(),
            event_name: "push".into(),
            git_ref: "refs/heads/main".into(),
            sha: self.commit.clone(),
            repository: "owner/repo".into(),
            repository_id: "123".into(),
            event_path: self.event.clone(),
        }
    }
    pub(super) fn inputs(&self) -> Inputs<'_> {
        Inputs {
            binding: &self.binding,
            repo: &self.repo,
            mw_bin: &self.parser,
            commit: &self.commit,
        }
    }
    pub(super) fn binding(&self) -> Binding {
        Binding::read(&self.binding).unwrap()
    }
    fn mutate_binding(&self, change: impl FnOnce(&mut Value)) {
        let mut v: Value = serde_json::from_slice(&fs::read(&self.binding).unwrap()).unwrap();
        change(&mut v);
        fs::write(&self.binding, v.to_string()).unwrap();
    }
    fn check(&self) -> Value {
        check(self.inputs(), self.claims(), || {
            Ok(Zeroizing::new(wire().to_string()))
        })
        .unwrap()
    }
}
fn no_secret() -> Result<Zeroizing<String>> {
    panic!("secret lookup before admission")
}
fn redacted(text: &str, f: &Fixture) {
    for s in [
        SUBJECT,
        REFRESH,
        ACCESS,
        CLIENT,
        "rotated-canary",
        "private-title-canary",
        "private-source-canary",
        "list/one+",
        "task/one+",
        "2026-11-10",
        f.root.to_str().unwrap(),
    ] {
        assert!(!text.contains(s), "non-summary data leaked");
    }
}
fn flags(v: &Value, remote: bool) {
    assert_eq!(v["redacted"], true);
    assert_eq!(v["pushClaimsAdmitted"], true);
    assert_eq!(v["remoteReadVerifiedDuringCollection"], remote);
    for flag in [
        "currentMainVerified",
        "pushVerified",
        "githubVerified",
        "portableEvidenceAuthenticated",
        "publicationReady",
        "conditionalWriteSafe",
        "hiveWriteProven",
        "hostedExecutionEnabled",
    ] {
        assert_eq!(v[flag], false, "{flag}");
    }
}

#[test]
fn check_is_readonly_detached_exact_head_and_dirty_worktree_ignored() {
    let f = Fixture::new();
    let before = storage::git(&f.repo, &["show-ref"], b"").unwrap();
    storage::git(&f.repo, &["checkout", "--detach", &f.commit], b"").unwrap();
    fs::write(f.repo.join("tasks.md"), "dirty ignored").unwrap();
    let v = f.check();
    flags(&v, false);
    assert_eq!(v["decision"], "set_first_due");
    redacted(&v.to_string(), &f);
    assert_eq!(before, storage::git(&f.repo, &["show-ref"], b"").unwrap());
    assert_eq!(
        fs::read_to_string(f.repo.join("tasks.md")).unwrap(),
        "dirty ignored"
    );
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 4);
}

#[test]
fn explicit_context_reader_only_requests_job_claims_and_missing_fails() {
    let f = Fixture::new();
    let values = [
        "true",
        "push",
        "refs/heads/main",
        &f.commit,
        "owner/repo",
        "123",
        f.event.to_str().unwrap(),
    ];
    let mut index = 0;
    let c = PushClaims::from_environment(|key| {
        assert_eq!(
            key,
            [
                "GITHUB_ACTIONS",
                "GITHUB_EVENT_NAME",
                "GITHUB_REF",
                "GITHUB_SHA",
                "GITHUB_REPOSITORY",
                "GITHUB_REPOSITORY_ID",
                "GITHUB_EVENT_PATH"
            ][index]
        );
        let v = values[index].to_string();
        index += 1;
        Some(v)
    })
    .unwrap();
    assert!(f.binding().admit(c, &f.commit).is_ok());
    assert!(PushClaims::from_environment(|_| None).is_err());
}

#[test]
fn invalid_claims_rejected_before_secret() {
    let f = Fixture::new();
    for case in 0..15 {
        let mut c = f.claims();
        match case {
            0 => c.actions = "false".into(),
            1 => c.event_name = "pull_request".into(),
            2 => c.event_name = "pull_request_target".into(),
            3 => c.git_ref = "refs/heads/other".into(),
            4 => c.git_ref = "refs/pull/1/merge".into(),
            5 => c.sha = "a".repeat(40),
            6 => c.sha = "0".repeat(40),
            7 => c.sha = f.commit[..8].into(),
            8 => c.sha = f.commit.to_uppercase(),
            9 => c.repository = "other/repo".into(),
            10 => c.repository = "Owner/repo".into(),
            11 => c.repository_id = "0123".into(),
            12 => c.repository_id = "0".into(),
            13 => c.repository_id = "124".into(),
            _ => c.event_path = "relative".into(),
        }
        let e = check(f.inputs(), c, no_secret).unwrap_err();
        assert_eq!(e.to_string(), "push_admission_failed");
        redacted(&format!("{e:#}"), &f);
    }
}

#[test]
fn event_bounded_regular_once_unique_and_consistent_before_secret() {
    let f = Fixture::new();
    let original = fs::read(&f.event).unwrap();
    for (pointer, value) in [
        ("/ref", json!("refs/heads/other")),
        ("/deleted", json!(true)),
        ("/deleted", Value::Null),
        ("/after", json!("a".repeat(40))),
        ("/repository/id", json!(124)),
        ("/repository/id", json!("123")),
        ("/repository/full_name", json!("other/repo")),
    ] {
        let mut v: Value = serde_json::from_slice(&original).unwrap();
        *v.pointer_mut(pointer).unwrap() = value;
        fs::write(&f.event, v.to_string()).unwrap();
        assert!(check(f.inputs(), f.claims(), no_secret).is_err());
    }
    for bytes in [b"{}".to_vec(),b"not json canary".to_vec(),vec![b' ';1024*1024+1],
        format!("{{\"ref\":\"refs/heads/main\",\"ref\":\"refs/heads/main\",\"deleted\":false,\"after\":\"{}\",\"repository\":{{\"id\":123,\"full_name\":\"owner/repo\"}}}}", f.commit).into_bytes(),
        format!("{{\"ref\":\"refs/heads/main\",\"deleted\":false,\"after\":\"{}\",\"repository\":{{\"id\":124,\"id\":123,\"full_name\":\"owner/repo\"}}}}", f.commit).into_bytes(),
        b"{\"unknown\":{\"nested\":1,\"nested\":2}}".to_vec()] {
        fs::write(&f.event, bytes).unwrap();
        assert!(check(f.inputs(), f.claims(), no_secret).is_err());
    }
    fs::remove_file(&f.event).unwrap();
    assert!(check(f.inputs(), f.claims(), no_secret).is_err());
    std::os::unix::fs::symlink(&f.binding, &f.event).unwrap();
    assert!(check(f.inputs(), f.claims(), no_secret).is_err());
    fs::remove_file(&f.event).unwrap();
    let path = std::ffi::CString::new(f.event.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(check(f.inputs(), f.claims(), no_secret).is_err());
}

#[test]
fn invalid_binding_never_looks_up_secret() {
    for case in 0..13 {
        let f = Fixture::new();
        f.mutate_binding(|v| match case {
            0 => v["unexpected"] = json!(true),
            1 => v["schemaVersion"] = json!("2"),
            2 => v["expectedRepositoryId"] = json!("0123"),
            3 => v["expectedRepository"] = json!("repo"),
            4 => v["expectedAccountNamespace"] = json!(SUBJECT),
            5 => v["connection"] = json!("../work"),
            6 => v["trustedParserSha256"] = json!("A".repeat(64)),
            7 => v["handoffSha256"] = json!("a".repeat(64)),
            8 => v["sourceRepoId"] = json!("different"),
            9 => v["expectedAccountNamespace"] = json!(format!("google-sub-v1:{}", "a".repeat(64))),
            10 => v["reviewedTaskHandoff"]["unexpected"] = json!(true),
            11 => v["reviewedTaskHandoff"]["receipt"]["sourcePath"] = json!("../bad"),
            _ => v["reviewedTaskHandoff"]["acceptedCurrentBaseline"] = json!(false),
        });
        let e = check(f.inputs(), f.claims(), no_secret).unwrap_err();
        assert_eq!(e.to_string(), "invalid_binding");
        redacted(&format!("{e:#}"), &f);
    }
    let f = Fixture::new();
    let original = fs::read_to_string(&f.binding).unwrap();
    fs::write(
        &f.binding,
        original.replacen("{", "{\"schemaVersion\":\"1\",", 1),
    )
    .unwrap();
    assert!(check(f.inputs(), f.claims(), no_secret).is_err());
    fs::write(&f.binding, vec![b' '; 128 * 1024 + 1]).unwrap();
    assert!(check(f.inputs(), f.claims(), no_secret).is_err());
    fs::remove_file(&f.binding).unwrap();
    std::os::unix::fs::symlink(&f.event, &f.binding).unwrap();
    assert!(check(f.inputs(), f.claims(), no_secret).is_err());
}

#[test]
fn source_head_parser_shallow_and_duplicate_invalid_policy_before_secret() {
    for case in 0..7 {
        let mut f = Fixture::new();
        match case {
            0 => {
                storage::git(&f.repo, &["commit", "--allow-empty", "-m", "advance"], b"").unwrap();
            }
            1 => fs::write(f.repo.join(".git/shallow"), format!("{}\n", f.commit)).unwrap(),
            2 => {
                let mut text = fs::read_to_string(&f.parser).unwrap();
                text.push_str("\n# changed\n");
                fs::write(&f.parser, text).unwrap();
            }
            _ => {
                let mut text = fs::read_to_string(f.repo.join("tasks.md")).unwrap();
                match case {
                    3 => {
                        let marker = text.lines().last().unwrap().to_owned();
                        text.push_str(&format!("{marker}\n"));
                    }
                    4 => text = text.replace("2026-11-10", "2026-02-30"),
                    5 => text = text.replace("- [ ]", "- [x]"),
                    _ => text = text.replace("mw-task:v1:", "mw-task:v2:"),
                }
                fs::write(f.repo.join("tasks.md"), text).unwrap();
                storage::git(&f.repo, &["add", "tasks.md"], b"").unwrap();
                storage::git(&f.repo, &["commit", "-m", "invalid source"], b"").unwrap();
                f.commit = head(&f.repo).unwrap();
                let mut e: Value = serde_json::from_slice(&fs::read(&f.event).unwrap()).unwrap();
                e["after"] = json!(f.commit);
                fs::write(&f.event, e.to_string()).unwrap();
            }
        }
        assert_eq!(
            check(f.inputs(), f.claims(), || panic!(
                "secret lookup in source case {case}"
            ))
            .unwrap_err()
            .to_string(),
            "source_preflight_failed",
            "case {case}"
        );
    }
}

#[test]
fn replaced_private_parser_pin_mismatch_never_executes_or_loads_secret() {
    let f = Fixture::new();
    let pin = parser_hash(&f.parser).unwrap();
    assert_eq!(pin, f.binding().trusted_parser_sha256);
    let marker = f.root.join("parser-b-executed");
    let replacement = f.root.join("replacement");
    fs::write(
        &replacement,
        format!("#!/bin/sh\n: > '{}'\nexit 9\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(&replacement, &f.parser).unwrap();
    let calls = AtomicUsize::new(0);
    let error = check(f.inputs(), f.claims(), || {
        calls.fetch_add(1, SeqCst);
        Ok(Zeroizing::new(wire().to_string()))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "source_preflight_failed");
    assert_eq!(calls.load(SeqCst), 0);
    assert!(!marker.exists());
    redacted(&format!("{error:#}"), &f);
}

#[test]
fn credential_structure_failures_are_sanitized_and_no_fallback() {
    let f = Fixture::new();
    for case in 0..6 {
        let mut w = wire();
        match case {
            0 => w["connection"] = json!("wrong"),
            1 => w["identity"] = json!(identity("other-subject")),
            2 => w["grant"] = Value::Null,
            3 => w["grant"]["scopes"] = json!(AuthProfile::TasksReadonly.scopes()),
            4 => w["grant"]["scopes"] = json!(["openid"]),
            _ => w["unknown"] = json!(REFRESH),
        }
        let e = check(f.inputs(), f.claims(), || Ok(Zeroizing::new(w.to_string()))).unwrap_err();
        assert_eq!(e.to_string(), "credential_structure_invalid");
        redacted(&format!("{e:#}"), &f);
    }
    assert_eq!(
        check(f.inputs(), f.claims(), || Err(anyhow!(REFRESH)))
            .unwrap_err()
            .to_string(),
        "credential_unavailable"
    );
    // Pretty JSON is valid credential bytes, not constrained to token string syntax.
    assert!(
        check(f.inputs(), f.claims(), || Ok(Zeroizing::new(
            serde_json::to_string_pretty(&wire()).unwrap()
        )))
        .is_ok()
    );
}

#[test]
fn admitted_files_are_not_reopened_and_source_advance_after_preflight_blocks_network() {
    let f = Fixture::new();
    let report = check(f.inputs(), f.claims(), || {
        fs::remove_file(&f.event).unwrap();
        fs::remove_file(&f.binding).unwrap();
        Ok(Zeroizing::new(wire().to_string()))
    })
    .unwrap();
    flags(&report, false);
    let f = Fixture::new();
    let error = plan(f.inputs(), f.claims(), || {
        storage::git(
            &f.repo,
            &["commit", "--allow-empty", "-m", "advanced after admission"],
            b"",
        )
        .unwrap();
        Ok(Zeroizing::new(wire().to_string()))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "source_changed");
    redacted(&format!("{error:#}"), &f);
}

pub(super) struct OAuth {
    pub(super) rotated: Option<&'static str>,
    subject: &'static str,
    refreshes: AtomicUsize,
    identities: AtomicUsize,
    on_refresh: Option<Box<dyn Fn() + Send + Sync>>,
    on_identity: Option<Box<dyn Fn() + Send + Sync>>,
}
impl OAuth {
    pub(super) fn new() -> Self {
        Self {
            rotated: None,
            subject: SUBJECT,
            refreshes: AtomicUsize::new(0),
            identities: AtomicUsize::new(0),
            on_refresh: None,
            on_identity: None,
        }
    }
}
impl OAuthTransport for OAuth {
    fn exchange(
        &self,
        _: &ClientConfig,
        _: &AuthorizationGrant,
    ) -> hive_google_auth::Result<TokenSet> {
        panic!("no login")
    }
    fn refresh(
        &self,
        client: &ClientConfig,
        refresh: &SecretString,
    ) -> hive_google_auth::Result<TokenSet> {
        assert_eq!(client.client_secret.as_ref().unwrap().expose(), CLIENT);
        assert_eq!(refresh.expose(), REFRESH);
        assert_eq!(self.refreshes.fetch_add(1, SeqCst), 0);
        if let Some(change) = &self.on_refresh {
            change();
        }
        Ok(TokenSet {
            access_token: secret(ACCESS),
            refresh_token: self.rotated.map(secret),
            expires_in: 3600,
            scopes: Some(
                AuthProfile::TasksWrite
                    .scopes()
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        })
    }
    fn identity(&self, token: &SecretString) -> hive_google_auth::Result<GoogleIdentity> {
        assert_eq!(token.expose(), ACCESS);
        assert_eq!(self.identities.fetch_add(1, SeqCst), 0);
        if let Some(change) = &self.on_identity {
            change();
        }
        Ok(identity(self.subject))
    }
}

#[test]
fn fresh_auth_same_bearer_exact_get_only_redacted_decisions_and_immutable_snapshot() {
    for (rotation, due, decision) in [
        (None, None, "set_first_due"),
        (
            Some(REFRESH),
            Some("2026-11-10T00:00:00Z"),
            "already_satisfied",
        ),
        (None, Some("2026-12-01T00:00:00Z"), "blocked"),
    ] {
        let f = Fixture::new();
        let b = f.binding();
        let a = b.admit(f.claims(), &f.commit).unwrap();
        let snapshot = source(&f.inputs(), &b, &a).unwrap();
        let store = ImmutableTasksWriteStore::from_record_bytes(
            "work",
            &b.expected_account_namespace,
            &serde_json::to_vec(&wire()).unwrap(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let gets = Arc::new(AtomicUsize::new(0));
        let counter = gets.clone();
        let mut raw = serde_json::to_value(&b.reviewed_task_handoff.baseline).unwrap();
        raw["etag"] = json!("\"fresh\"");
        if let Some(due) = due {
            raw["due"] = json!(due);
        }
        let body = raw.to_string();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                assert_eq!(stream.read(&mut byte).unwrap(), 1);
                bytes.push(byte[0]);
                assert!(bytes.len() < 8192);
            }
            let text = String::from_utf8(bytes).unwrap();
            assert!(
                text.starts_with("GET /lists/list%2Fone+/tasks/task%2Fone+ HTTP/1.1\r\n"),
                "{text}"
            );
            assert!(
                text.to_lowercase()
                    .contains(&format!("authorization: bearer {ACCESS}\r\n"))
            );
            assert!(!text.to_lowercase().contains("if-match"));
            counter.fetch_add(1, SeqCst);
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"fresh\"\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        });
        let http = ExactGet::loopback(&format!("http://{addr}/lists/"));
        let mut oauth = OAuth::new();
        oauth.rotated = rotation;
        let refs = storage::git(&f.repo, &["show-ref"], b"").unwrap();
        let report = collect(
            &f.inputs(),
            &b,
            &a,
            &snapshot,
            &store,
            &oauth,
            &http,
            Instant::now() + Duration::from_secs(120),
        )
        .unwrap();
        worker.join().unwrap();
        flags(&report, true);
        assert_eq!(report["decision"], decision);
        assert_eq!(report["googleOrigin"], false);
        redacted(&report.to_string(), &f);
        assert_eq!(gets.load(SeqCst), 1);
        assert_eq!(oauth.refreshes.load(SeqCst), 1);
        assert_eq!(oauth.identities.load(SeqCst), 1);
        assert_eq!(store.load("work").unwrap().unwrap().generation, 7);
        assert_eq!(storage::git(&f.repo, &["show-ref"], b"").unwrap(), refs);
        assert_eq!(fs::read_dir(&f.root).unwrap().count(), 4);
    }
}

#[test]
fn rotation_wrong_identity_and_expired_budget_make_zero_gets() {
    let f = Fixture::new();
    let b = f.binding();
    let a = b.admit(f.claims(), &f.commit).unwrap();
    let snapshot = source(&f.inputs(), &b, &a).unwrap();
    for case in 0..3 {
        let store = ImmutableTasksWriteStore::from_record_bytes(
            "work",
            &b.expected_account_namespace,
            &serde_json::to_vec(&wire()).unwrap(),
        )
        .unwrap();
        let mut oauth = OAuth::new();
        if case == 0 {
            oauth.rotated = Some("rotated-canary");
        } else if case == 1 {
            oauth.subject = "wrong-identity";
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let http = ExactGet::loopback(&format!("http://{}/lists/", listener.local_addr().unwrap()));
        let deadline = if case == 2 {
            Instant::now()
        } else {
            Instant::now() + Duration::from_secs(120)
        };
        let e = collect(
            &f.inputs(),
            &b,
            &a,
            &snapshot,
            &store,
            &oauth,
            &http,
            deadline,
        )
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            [
                "auth_action_required",
                "authentication_failed",
                "budget_expired"
            ][case]
        );
        assert_eq!(oauth.refreshes.load(SeqCst), usize::from(case != 2));
        assert_eq!(oauth.identities.load(SeqCst), usize::from(case == 1));
        assert!(listener.accept().is_err());
        redacted(&format!("{e:#}"), &f);
        assert_eq!(
            store
                .load("work")
                .unwrap()
                .unwrap()
                .grant
                .unwrap()
                .refresh_token
                .expose(),
            REFRESH
        );
    }
}

#[test]
fn head_or_parser_change_during_refresh_or_userinfo_blocks_following_get() {
    for during_identity in [false, true] {
        for change_parser in [false, true] {
            let f = Fixture::new();
            let b = f.binding();
            let a = b.admit(f.claims(), &f.commit).unwrap();
            let snapshot = source(&f.inputs(), &b, &a).unwrap();
            let store = ImmutableTasksWriteStore::from_record_bytes(
                "work",
                &b.expected_account_namespace,
                &serde_json::to_vec(&wire()).unwrap(),
            )
            .unwrap();
            let repo = f.repo.clone();
            let parser = f.parser.clone();
            let change = Box::new(move || {
                if change_parser {
                    fs::write(&parser, "#!/bin/sh\nexit 9\n").unwrap();
                } else {
                    storage::git(
                        &repo,
                        &["commit", "--allow-empty", "-m", "advanced during auth"],
                        b"",
                    )
                    .unwrap();
                }
            });
            let mut oauth = OAuth::new();
            if during_identity {
                oauth.on_identity = Some(change);
            } else {
                oauth.on_refresh = Some(change);
            }
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let http =
                ExactGet::loopback(&format!("http://{}/lists/", listener.local_addr().unwrap()));
            let error = collect(
                &f.inputs(),
                &b,
                &a,
                &snapshot,
                &store,
                &oauth,
                &http,
                Instant::now() + Duration::from_secs(120),
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "source_changed");
            assert_eq!(oauth.refreshes.load(SeqCst), 1);
            assert_eq!(oauth.identities.load(SeqCst), 1);
            assert!(listener.accept().is_err());
            redacted(&format!("{error:#}"), &f);
        }
    }
}
