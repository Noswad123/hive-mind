use super::*;
use hive_capture::{GoogleTask, handoff::ReceiptEvidence, store::deterministic_capture_id};
use hive_google_auth::{
    AuthorizationGrant, ClientConfig, CredentialGuard, GoogleIdentity, GrantRecord, SecretString,
    StoredConnection, TokenSet,
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    thread,
};

const SUBJECT: &str = "publisher-synthetic-subject";
const TOKEN: &str = "publisher-fake-bearer";
fn secret(s: &str) -> SecretString {
    SecretString::new(s).unwrap()
}
fn identity(s: &str) -> GoogleIdentity {
    GoogleIdentity::new(s).unwrap()
}
struct Guard;
impl CredentialGuard for Guard {}
struct Store {
    record: Mutex<Option<StoredConnection>>,
    profile: AuthProfile,
    locks: AtomicUsize,
}
impl Store {
    fn new() -> Self {
        Self {
            record: Mutex::new(Some(StoredConnection {
                version: 1,
                connection: "work".into(),
                identity: identity(SUBJECT),
                generation: 1,
                grant: Some(GrantRecord {
                    client: ClientConfig {
                        client_id: "fixture.apps.googleusercontent.com".into(),
                        client_secret: None,
                    },
                    refresh_token: secret("fake-refresh"),
                    scopes: AuthProfile::TasksWrite
                        .scopes()
                        .iter()
                        .map(|s| (*s).into())
                        .collect(),
                }),
            })),
            profile: AuthProfile::TasksWrite,
            locks: AtomicUsize::new(0),
        }
    }
}
impl CredentialStore for Store {
    fn profile(&self) -> AuthProfile {
        self.profile
    }
    fn lock(&self, _: &str) -> hive_google_auth::Result<Box<dyn CredentialGuard + '_>> {
        self.locks.fetch_add(1, SeqCst);
        Ok(Box::new(Guard))
    }
    fn load(&self, _: &str) -> hive_google_auth::Result<Option<StoredConnection>> {
        Ok(self.record.lock().unwrap().clone())
    }
    fn save(&self, _: &str, record: &StoredConnection) -> hive_google_auth::Result<()> {
        *self.record.lock().unwrap() = Some(record.clone());
        Ok(())
    }
}
#[derive(Clone)]
struct OAuth {
    subject: &'static str,
    calls: Arc<AtomicUsize>,
}
impl OAuth {
    fn new() -> Self {
        Self {
            subject: SUBJECT,
            calls: Arc::new(AtomicUsize::new(0)),
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
    fn refresh(&self, _: &ClientConfig, _: &SecretString) -> hive_google_auth::Result<TokenSet> {
        self.calls.fetch_add(1, SeqCst);
        Ok(TokenSet {
            access_token: secret(TOKEN),
            refresh_token: None,
            expires_in: 3600,
            scopes: Some(
                AuthProfile::TasksWrite
                    .scopes()
                    .iter()
                    .map(|s| (*s).into())
                    .collect(),
            ),
        })
    }
    fn identity(&self, token: &SecretString) -> hive_google_auth::Result<GoogleIdentity> {
        assert_eq!(token.expose(), TOKEN);
        self.calls.fetch_add(1, SeqCst);
        Ok(identity(self.subject))
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: PathBuf,
    repo: PathBuf,
    parser: PathBuf,
    commit: String,
}
impl Fixture {
    fn new() -> Self {
        Self::with_source_padding(0)
    }
    fn with_source_padding(padding: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("source");
        fs::create_dir(&repo).unwrap();
        storage::git(&repo, &["init", "--template=", "-b", "main", "."], b"").unwrap();
        let account = identity(SUBJECT).namespace().unwrap();
        let capture = deterministic_capture_id(&account, "list/one+", "task/one+").unwrap();
        let baseline: GoogleTask = serde_json::from_value(
            json!({"id":"task/one+", "title":"Synthetic task", "status":"needsAction", "links":[]}),
        )
        .unwrap();
        let handoff = TaskHandoff {
            schema_version: "1".into(),
            kind: "google-task-handoff".into(),
            source_repo_id: "publisher-source".into(),
            capture_id: capture.clone(),
            account_key: account,
            list_id: "list/one+".into(),
            provider_id: "task/one+".into(),
            accepted_current_baseline: true,
            baseline,
            receipt: ReceiptEvidence {
                method: "todos-add".into(),
                sha256: "a".repeat(64),
                stable_id: capture.clone(),
                source_id: "synthetic-note".into(),
                source_path: "tasks.md".into(),
                source_revision: format!("sha256:{}", "b".repeat(64)),
            },
        };
        let title = format!("Renamed source{}", "x".repeat(padding));
        let text = format!(
            "---\nid: synthetic-note\ndomains: [task]\ntask_active: true\ntask_scope: project\n---\n\n## Todo\n### Inbox\n- [ ] {title}\n  - due:2026-11-10\n  <!-- mw-task:v1:{capture} -->\n"
        );
        fs::write(repo.join("tasks.md"), &text).unwrap();
        storage::git(&repo, &["add", "tasks.md"], b"").unwrap();
        storage::git(&repo, &["commit", "-m", "synthetic source"], b"").unwrap();
        let commit = main_tip(&repo).unwrap();
        let item = json!({"id":format!("task:{capture}"), "stableId":capture, "identityKind":"stable", "sourceRevision":format!("sha256:{}", hash(text.as_bytes())), "sourceId":"synthetic-note", "sourcePath":"tasks.md", "line":10, "text":title, "done":false, "metadata":{"due":"2026-11-10","raw":"due:2026-11-10","start":"","estimate":""}});
        let parser = root.join("mw-stub");
        fs::write(&parser, format!("#!/bin/sh\nset -eu\ntest \"$*\" = 'todos snapshot --json'\nprintf '{{\"schemaVersion\":\"1\",\"source\":\"saved-markdown\",\"notesDir\":\"%s\",\"dashboardPath\":\"%s\",\"items\":[{item}]}}' \"$NOTES_DIR\" \"$DASHBOARD_PATH\"\n")).unwrap();
        fs::set_permissions(&parser, fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.join("publisher.git");
        initialize_origin(&state, &repo, &parser, "work", &handoff, "test-loopback").unwrap();
        Self {
            _temp: temp,
            state,
            repo,
            parser,
            commit,
        }
    }
    fn session(&self) -> Publisher {
        Publisher::open_origin(&self.state, "test-loopback").unwrap()
    }
    fn prepare(&self, server: &Server) -> Value {
        let oauth = OAuth::new();
        self.session()
            .prepare_with(
                &self.commit,
                &Store::new(),
                &|_| Ok(oauth.clone()),
                &server.http(),
                &Budget::new(),
                Stop::None,
            )
            .unwrap()
    }
    fn apply(&self, server: &Server, report: &Value, stop: Stop) -> Result<Value> {
        let oauth = OAuth::new();
        self.session().apply_with(
            &approval(report),
            &Store::new(),
            &|_| Ok(oauth.clone()),
            &server.http(),
            &Budget::new(),
            stop,
        )
    }
    fn recover(&self, server: &Server, report: &Value) -> Value {
        let oauth = OAuth::new();
        self.session()
            .recover_with(
                report["operationId"].as_str().unwrap(),
                &Store::new(),
                &|_| Ok(oauth.clone()),
                &server.http(),
                &Budget::new(),
            )
            .unwrap()
    }
}
fn approval(report: &Value) -> Approval<'_> {
    Approval {
        operation_id: report["operationId"].as_str().unwrap(),
        expected_state_oid: report["stateOid"].as_str().unwrap(),
        expected_intent_sha256: report["intentSha256"].as_str().unwrap(),
        acknowledge_live_write: true,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Conflict,
    Lost,
    Timeout,
    Malformed,
    Oversize,
    Redirect,
    WrongId,
    Weak,
    UnchangedTag,
    Drift,
    ConfirmDrift,
    HeaderMismatch,
    HeaderMismatchGet,
    MissingStrings,
    InvalidUpdated,
    NullUpdated,
    GetStatus(u16),
    MalformedGet,
}
struct Server {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    raw: Arc<Mutex<Value>>,
    mode: Arc<Mutex<Mode>>,
    done: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/tasks/v1/lists/", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let raw = Arc::new(Mutex::new(
            json!({"id":"task/one+","title":"Synthetic task","status":"needsAction","etag":"\"v1\"","links":[],"kind":"tasks#task","position":"00001","updated":"2026-11-01T00:00:00Z"}),
        ));
        let mode = Arc::new(Mutex::new(Mode::Normal));
        let done = Arc::new(AtomicBool::new(false));
        let (rs, task, behavior, end) = (requests.clone(), raw.clone(), mode.clone(), done.clone());
        let worker = thread::spawn(move || {
            while !end.load(SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let patch = request.starts_with("PATCH ");
                rs.lock().unwrap().push(request);
                let mode = *behavior.lock().unwrap();
                let mut status = "200 OK";
                let mut resource = task.lock().unwrap();
                if patch && mode == Mode::Conflict {
                    status = "412 Precondition Failed";
                } else if patch {
                    resource["due"] = json!("2026-11-10T00:00:00.000Z");
                    resource["etag"] = json!(if mode == Mode::UnchangedTag {
                        "\"v1\""
                    } else {
                        "\"v2\""
                    });
                    resource["updated"] = json!("2026-11-04T00:00:00Z");
                }
                if patch && mode == Mode::Lost {
                    continue;
                }
                if patch && mode == Mode::Timeout {
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
                let mut body = resource.clone();
                if patch {
                    match mode {
                        Mode::WrongId => body["id"] = json!("wrong"),
                        Mode::Weak => body["etag"] = json!("W/\"weak\""),
                        Mode::Drift => body["position"] = json!("drift"),
                        Mode::MissingStrings => {
                            body.as_object_mut().unwrap().remove("title");
                        }
                        Mode::InvalidUpdated => body["updated"] = json!("not-a-timestamp"),
                        Mode::NullUpdated => body["updated"] = Value::Null,
                        _ => (),
                    }
                } else if mode == Mode::ConfirmDrift && resource.get("due").is_some() {
                    body["position"] = json!("drift");
                }
                let tag =
                    if mode == Mode::HeaderMismatchGet || (patch && mode == Mode::HeaderMismatch) {
                        "\"other-header\""
                    } else {
                        body["etag"].as_str().unwrap_or("\"header\"")
                    }
                    .to_owned();
                let mut bytes = serde_json::to_vec(&body).unwrap();
                if patch {
                    match mode {
                        Mode::Conflict => bytes = br#"{"error":{"code":412}}"#.to_vec(),
                        Mode::Malformed => bytes = b"not json".to_vec(),
                        Mode::Oversize => bytes = vec![b' '; 65537],
                        Mode::Redirect => status = "302 Found",
                        _ => (),
                    }
                } else {
                    match mode {
                        Mode::GetStatus(code) => {
                            status = match code {
                                302 => "302 Found",
                                404 => "404 Not Found",
                                429 => "429 Too Many Requests",
                                500 => "500 Internal Server Error",
                                503 => "503 Service Unavailable",
                                _ => panic!("unsupported fixture status"),
                            }
                        }
                        Mode::MalformedGet => bytes = b"not JSON".to_vec(),
                        _ => (),
                    }
                }
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nETag: {tag}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&bytes);
            }
        });
        Self {
            base,
            requests,
            raw,
            mode,
            done,
            worker: Some(worker),
        }
    }
    fn http(&self) -> Http {
        Http::loopback(&self.base)
    }
    fn patches(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("PATCH "))
            .count()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.done.store(true, SeqCst);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}
fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut bytes = Vec::new();
    let mut buf = [0; 1024];
    loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&buf[..n]);
        assert!(bytes.len() < 16384);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
            let len = header
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .map(|s| s.parse::<usize>().unwrap())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + len {
                return Some(String::from_utf8(bytes).unwrap());
            }
        }
    }
}

#[test]
fn actual_http_full_chain_exact_request_durable_ack_and_no_replay() {
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    assert_eq!(prepared["phase"], "Prepared");
    assert!(
        !serde_json::to_string(&prepared["operations"])
            .unwrap()
            .contains(f.repo.to_str().unwrap())
    );
    let result = f.apply(&server, &prepared, Stop::None).unwrap();
    assert_eq!(result["phase"], "Applied");
    assert_eq!(
        result["operations"][0]["attemptEvidence"]["response"]["status"],
        200
    );
    for key in [
        "hostedReady",
        "publicationReady",
        "pushVerified",
        "remoteVerified",
        "hiveWriteProven",
    ] {
        assert_eq!(result[key], false);
    }
    assert!(Publisher::open(&f.state).is_err());
    assert!(f.apply(&server, &prepared, Stop::None).is_err());
    assert_eq!(f.recover(&server, &prepared)["phase"], "Applied");
    assert_eq!(server.patches(), 1);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for request in requests.iter() {
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer publisher-fake-bearer\r\n")
        );
    }
    let patch = requests.iter().find(|s| s.starts_with("PATCH ")).unwrap();
    assert!(
        patch.starts_with("PATCH /tasks/v1/lists/list%2Fone+/tasks/task%2Fone+ HTTP/1.1\r\n"),
        "{patch}"
    );
    assert!(
        patch
            .to_lowercase()
            .contains("authorization: bearer publisher-fake-bearer\r\n")
    );
    assert!(patch.to_lowercase().contains("if-match: \"v1\"\r\n"));
    assert_eq!(
        serde_json::from_str::<Value>(patch.split("\r\n\r\n").nth(1).unwrap()).unwrap(),
        json!({"due":"2026-11-10T00:00:00.000Z"})
    );
}

#[test]
fn actual_http_412_is_terminal_no_retry() {
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    *server.mode.lock().unwrap() = Mode::Conflict;
    let result = f.apply(&server, &prepared, Stop::None).unwrap();
    assert_eq!(result["phase"], "RejectedConflict");
    assert_eq!(
        result["operations"][0]["attemptEvidence"]["response"]["status"],
        412
    );
    assert_eq!(f.recover(&server, &prepared)["phase"], "RejectedConflict");
    assert_eq!(server.patches(), 1);
}

#[test]
fn lost_response_restart_recovery_without_source_never_repatches() {
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    *server.mode.lock().unwrap() = Mode::Lost;
    assert_eq!(
        f.apply(&server, &prepared, Stop::None).unwrap()["phase"],
        "Uncertain"
    );
    fs::remove_dir_all(&f.repo).unwrap();
    fs::remove_file(&f.parser).unwrap();
    let recovered = f.recover(&server, &prepared);
    assert_eq!(recovered["phase"], "RecoveredSatisfied");
    assert_eq!(recovered["attribution"], "UNKNOWN");
    assert_eq!(f.recover(&server, &prepared)["phase"], "RecoveredSatisfied");
    assert_eq!(server.patches(), 1);
}

#[test]
fn actual_timeout_after_remote_mutation_recovers_unknown_without_second_patch() {
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    *server.mode.lock().unwrap() = Mode::Timeout;
    let oauth = OAuth::new();
    let result = f
        .session()
        .apply_with(
            &approval(&prepared),
            &Store::new(),
            &|_| Ok(oauth.clone()),
            &Http::loopback_timeout(&server.base, Duration::from_millis(200)),
            &Budget::new(),
            Stop::None,
        )
        .unwrap();
    assert_eq!(result["phase"], "Uncertain");
    fs::remove_dir_all(&f.repo).unwrap();
    fs::remove_file(&f.parser).unwrap();
    let recovered = f.recover(&server, &prepared);
    assert_eq!(recovered["phase"], "RecoveredSatisfied");
    assert_eq!(recovered["attribution"], "UNKNOWN");
    assert_eq!(server.patches(), 1);
}

#[test]
fn committed_source_completion_due_removal_and_duplicate_block_before_auth() {
    for case in ["completed", "no-due", "duplicate"] {
        let f = Fixture::new();
        let server = Server::new();
        let prepared = f.prepare(&server);
        let note = f.repo.join("tasks.md");
        let text = fs::read_to_string(&note).unwrap();
        let changed = match case {
            "completed" => text.replace("- [ ]", "- [x]"),
            "no-due" => text.replace("  - due:2026-11-10\n", ""),
            _ => format!("{text}\n{text}"),
        };
        fs::write(&note, changed).unwrap();
        storage::git(&f.repo, &["add", "tasks.md"], b"").unwrap();
        storage::git(&f.repo, &["commit", "-m", "changed source"], b"").unwrap();
        assert!(f.session().preflight_apply(&approval(&prepared)).is_err());
        // Even a fresh local commit cannot make the original parser evidence authorize it.
        let tip = main_tip(&f.repo).unwrap();
        let session = f.session();
        let (_, state) = session.load().unwrap();
        assert!(
            session
                .source(
                    &tip,
                    &state.manifest.handoff,
                    &state.operations[0].intent.baseline
                )
                .is_err()
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn frozen_resource_change_blocks_apply_and_recovery_semantic_conflict_blocks_target() {
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    server.raw.lock().unwrap()["position"] = json!("changed before attempt");
    assert!(f.apply(&server, &prepared, Stop::None).is_err());
    assert_eq!(server.patches(), 0);
    server.raw.lock().unwrap()["position"] = json!("00001");
    assert!(f.apply(&server, &prepared, Stop::AfterAttempting).is_err());
    server.raw.lock().unwrap()["due"] = json!("2026-12-01T00:00:00Z");
    assert_eq!(
        f.recover(&server, &prepared)["phase"],
        "ConflictNeedsReview"
    );
    assert!(f.session().preflight_prepare(&f.commit).is_err());
    assert_eq!(server.patches(), 0);
}

#[test]
fn strict_state_digests_phase_and_binding_copy_rejection() {
    for case in [
        "unknown",
        "schema",
        "intent",
        "account",
        "phase",
        "origin",
        "binding-path",
    ] {
        let f = Fixture::new();
        let server = Server::new();
        f.prepare(&server);
        if case == "binding-path" {
            let path = f.state.join(BINDING);
            let mut binding: Binding = storage::read(&path).unwrap();
            binding.state_repo = f.repo.clone();
            storage::atomic(&path, &binding).unwrap();
        } else {
            let (oid, mut state): (_, Value) = storage::load_ref(&f.state, STATE_REF).unwrap();
            match case {
                "unknown" => state["unknown"] = json!(true),
                "schema" => state["manifest"]["schema"] = json!(2),
                "intent" => {
                    state["operations"][0]["intent"]["patch"]["due"] = json!("2026-12-01T00:00:00Z")
                }
                "account" => {
                    state["manifest"]["handoff"]["accountKey"] =
                        json!(identity("other").namespace().unwrap())
                }
                "phase" => state["operations"][0]["phases"] = json!(["Prepared", "Applied"]),
                _ => state["manifest"]["origin"] = json!("Google"),
            }
            storage::save_ref(&f.state, STATE_REF, Some(&oid), &state).unwrap();
        }
        assert!(
            Publisher::open_origin(&f.state, "test-loopback").is_err(),
            "{case}"
        );
        assert_eq!(server.patches(), 0);
    }
}

#[test]
fn crash_boundaries_recover_without_replay() {
    for (stop, phase, patches, evidence) in [
        (Stop::AttemptingCas, "NotDispatched", 0, false),
        (Stop::AfterAttempting, "Uncertain", 0, false),
        (Stop::BeforePatch, "Uncertain", 0, false),
        (Stop::ExpiredAfterAttempting, "Uncertain", 0, false),
        (Stop::AfterRemoteMutation, "RecoveredSatisfied", 1, false),
        (Stop::AfterPatchEvidence, "RecoveredSatisfied", 1, true),
        (Stop::BeforeConfirmation, "RecoveredSatisfied", 1, true),
        (Stop::OutcomeCas, "RecoveredSatisfied", 1, true),
    ] {
        let f = Fixture::new();
        let server = Server::new();
        let prepared = f.prepare(&server);
        assert!(f.apply(&server, &prepared, stop).is_err());
        fs::remove_dir_all(&f.repo).unwrap();
        fs::remove_file(&f.parser).unwrap();
        let result = f.recover(&server, &prepared);
        assert_eq!(result["phase"], phase);
        assert_eq!(result["durableValidatedPatchEvidence"], evidence);
        assert_eq!(server.patches(), patches);
        let oid = result["stateOid"].clone();
        assert_eq!(f.recover(&server, &prepared)["stateOid"], oid);
    }
}

#[test]
fn malformed_or_drifting_mutation_never_applied() {
    for mode in [
        Mode::Malformed,
        Mode::Oversize,
        Mode::Redirect,
        Mode::WrongId,
        Mode::Weak,
        Mode::UnchangedTag,
        Mode::Drift,
        Mode::ConfirmDrift,
        Mode::HeaderMismatch,
        Mode::MissingStrings,
        Mode::InvalidUpdated,
        Mode::NullUpdated,
    ] {
        let f = Fixture::new();
        let server = Server::new();
        let prepared = f.prepare(&server);
        *server.mode.lock().unwrap() = mode;
        assert_eq!(
            f.apply(&server, &prepared, Stop::None).unwrap()["phase"],
            "Uncertain"
        );
        assert!(f.apply(&server, &prepared, Stop::None).is_err());
        assert_eq!(server.patches(), 1);
    }
}

#[test]
fn local_preflight_stale_review_parser_and_main_block_before_credentials() {
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    let session = f.session();
    let mut a = approval(&prepared);
    a.acknowledge_live_write = false;
    assert!(session.preflight_apply(&a).is_err());
    let bad_digest = "0".repeat(64);
    let bad_oid = "0".repeat(40);
    a = approval(&prepared);
    a.expected_intent_sha256 = &bad_digest;
    assert!(session.preflight_apply(&a).is_err());
    a = approval(&prepared);
    a.expected_state_oid = &bad_oid;
    assert!(session.preflight_apply(&a).is_err());
    fs::write(&f.parser, "#!/bin/sh\nexit 1\n").unwrap();
    assert!(session.preflight_apply(&approval(&prepared)).is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    drop(session);
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    fs::write(f.repo.join("advance"), "advance").unwrap();
    storage::git(&f.repo, &["add", "advance"], b"").unwrap();
    storage::git(&f.repo, &["commit", "-m", "advance"], b"").unwrap();
    assert!(f.session().preflight_apply(&approval(&prepared)).is_err());
    assert_eq!(server.patches(), 0);
}

#[test]
fn auth_profile_missing_and_account_mismatch_never_reach_tasks() {
    for case in ["profile", "missing", "saved-subject", "handoff-subject"] {
        let f = Fixture::new();
        let server = Server::new();
        let mut store = Store::new();
        let mut oauth = OAuth::new();
        match case {
            "profile" => store.profile = AuthProfile::TasksReadonly,
            "missing" => *store.record.lock().unwrap() = None,
            "saved-subject" => oauth.subject = "different-subject",
            _ => {
                store.record.lock().unwrap().as_mut().unwrap().identity =
                    identity("different-subject");
                oauth.subject = "different-subject";
            }
        }
        assert!(
            f.session()
                .prepare_with(
                    &f.commit,
                    &store,
                    &|_| Ok(oauth.clone()),
                    &server.http(),
                    &Budget::new(),
                    Stop::None
                )
                .is_err()
        );
        assert!(server.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn local_lock_binding_aliases_foreign_refs_and_fake_cross_open_rejected() {
    let f = Fixture::new();
    let session = f.session();
    assert!(Publisher::open_origin(&f.state, "test-loopback").is_err());
    drop(session);
    let alias = f.state.parent().unwrap().join("alias.git");
    std::os::unix::fs::symlink(&f.state, &alias).unwrap();
    assert!(Publisher::open_origin(&alias, "test-loopback").is_err());
    assert!(crate::inspect(&f.state).is_err());
    let oid = f.session().inspect().unwrap()["stateOid"]
        .as_str()
        .unwrap()
        .to_owned();
    storage::git(&f.state, &["update-ref", "refs/heads/foreign", &oid], b"").unwrap();
    assert!(Publisher::open_origin(&f.state, "test-loopback").is_err());
}

#[test]
fn prepared_recovery_no_auth_and_prepare_crash_boundaries() {
    for stop in [Stop::BeforePrepared, Stop::AfterPrepared] {
        let f = Fixture::new();
        let server = Server::new();
        let oauth = OAuth::new();
        assert!(
            f.session()
                .prepare_with(
                    &f.commit,
                    &Store::new(),
                    &|_| Ok(oauth.clone()),
                    &server.http(),
                    &Budget::new(),
                    stop
                )
                .is_err()
        );
        let report = f.session().inspect().unwrap();
        if stop == Stop::BeforePrepared {
            assert_eq!(report["operations"], json!([]));
        } else {
            fs::remove_dir_all(&f.repo).unwrap();
            fs::remove_file(&f.parser).unwrap();
            assert_eq!(
                f.session()
                    .recover(report["operationId"].as_str().unwrap())
                    .unwrap()["phase"],
                "NotDispatched"
            );
        }
        assert_eq!(server.patches(), 0);
    }
}

#[test]
fn same_due_closes_target_and_different_due_conflicts() {
    for due in ["2026-11-10T00:00:00.000Z", "2026-12-01T00:00:00.000Z"] {
        let f = Fixture::new();
        let server = Server::new();
        server.raw.lock().unwrap()["due"] = json!(due);
        let oauth = OAuth::new();
        let result = f.session().prepare_with(
            &f.commit,
            &Store::new(),
            &|_| Ok(oauth.clone()),
            &server.http(),
            &Budget::new(),
            Stop::None,
        );
        if due.starts_with("2026-11") {
            assert_eq!(result.unwrap()["phase"], "AlreadySatisfied");
            assert!(f.session().preflight_prepare(&f.commit).is_err());
        } else {
            assert!(result.is_err());
        }
        assert_eq!(server.patches(), 0);
    }
}

#[test]
fn malformed_fresh_get_resources_never_freeze_intent() {
    for case in [
        "id",
        "title",
        "status",
        "weak",
        "missing-etag",
        "links",
        "oversize",
        "header",
    ] {
        let f = Fixture::new();
        let server = Server::new();
        {
            let mut raw = server.raw.lock().unwrap();
            match case {
                "id" => raw["id"] = json!("wrong"),
                "title" => {
                    raw.as_object_mut().unwrap().remove("title");
                }
                "status" => raw["status"] = json!("completed"),
                "weak" => raw["etag"] = json!("W/\"v1\""),
                "missing-etag" => {
                    raw.as_object_mut().unwrap().remove("etag");
                }
                "links" => raw["links"] = json!([{"link":"not supported"}]),
                "oversize" => raw["position"] = json!("x".repeat(65537)),
                _ => *server.mode.lock().unwrap() = Mode::HeaderMismatchGet,
            }
        }
        let oauth = OAuth::new();
        assert!(
            f.session()
                .prepare_with(
                    &f.commit,
                    &Store::new(),
                    &|_| Ok(oauth.clone()),
                    &server.http(),
                    &Budget::new(),
                    Stop::None
                )
                .is_err(),
            "{case}"
        );
        assert_eq!(f.session().inspect().unwrap()["operations"], json!([]));
        assert_eq!(server.patches(), 0);
    }
}

#[test]
fn test_origin_inception_cannot_be_promoted_by_editing_tip_and_local_binding() {
    let f = Fixture::new();
    let server = Server::new();
    f.prepare(&server);
    let binding_path = f.state.join(BINDING);
    let mut binding: Binding = storage::read(&binding_path).unwrap();
    binding.origin = "Google".into();
    storage::atomic(&binding_path, &binding).unwrap();
    let (oid, mut state): (_, State) = storage::load_ref(&f.state, STATE_REF).unwrap();
    state.manifest.origin = "Google".into();
    storage::save_ref(&f.state, STATE_REF, Some(&oid), &state).unwrap();
    assert!(Publisher::open(&f.state).is_err());
    // The inception commit remains a second durable origin floor.
    fs::remove_file(f.state.join(TEST_FLOOR)).unwrap();
    assert!(Publisher::open(&f.state).is_err());
}

#[test]
fn normal_google_installation_local_init_and_inspect_require_no_credentials() {
    let f = Fixture::new();
    let google_state = f.state.parent().unwrap().join("google-local-only.git");
    let (_, state) = f.session().load().unwrap();
    let result = initialize(
        &google_state,
        &f.repo,
        &f.parser,
        "work",
        &state.manifest.handoff,
    )
    .unwrap();
    assert_eq!(result["origin"], "Google");
    assert_eq!(result["remoteVerified"], false);
    let session = Publisher::open(&google_state).unwrap();
    assert_eq!(session.inspect().unwrap()["operations"], json!([]));
    assert!(Publisher::open_origin(&google_state, "test-loopback").is_err());
    assert!(crate::inspect(&google_state).is_err());
}

#[test]
fn prepared_recovery_allows_fresh_intent_but_never_replays_old_approval() {
    let f = Fixture::new();
    let server = Server::new();
    let old = f.prepare(&server);
    assert_eq!(f.recover(&server, &old)["phase"], "NotDispatched");
    let fresh = f.prepare(&server);
    assert_ne!(fresh["operationId"], old["operationId"]);
    assert!(f.apply(&server, &old, Stop::None).is_err());
    assert_eq!(
        f.apply(&server, &fresh, Stop::None).unwrap()["phase"],
        "Applied"
    );
    assert_eq!(server.patches(), 1);
}

#[test]
fn repeated_unchanged_recovery_has_no_history_growth_and_no_source_dependency() {
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    assert!(f.apply(&server, &prepared, Stop::AfterAttempting).is_err());
    fs::remove_dir_all(&f.repo).unwrap();
    fs::remove_file(&f.parser).unwrap();
    let first = f.recover(&server, &prepared);
    assert_eq!(first["phase"], "Uncertain");
    for _ in 0..12 {
        assert_eq!(f.recover(&server, &prepared)["stateOid"], first["stateOid"]);
    }
    server.raw.lock().unwrap()["due"] = json!("2026-11-10T00:00:00Z");
    let final_report = f.recover(&server, &prepared);
    assert_eq!(final_report["phase"], "RecoveredSatisfied");
    assert_eq!(final_report["attribution"], "UNKNOWN");
    assert_eq!(
        final_report["operations"][0]["phases"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(server.patches(), 0);
}

#[test]
fn apply_and_recovery_reauthenticate_the_exact_frozen_account_before_tasks() {
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    let store = Store::new();
    store.record.lock().unwrap().as_mut().unwrap().identity = identity("wrong-account");
    let mut oauth = OAuth::new();
    oauth.subject = "wrong-account";
    assert!(
        f.session()
            .apply_with(
                &approval(&prepared),
                &store,
                &|_| Ok(oauth.clone()),
                &server.http(),
                &Budget::new(),
                Stop::None
            )
            .is_err()
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert_eq!(oauth.calls.load(SeqCst), 2);
    assert!(f.apply(&server, &prepared, Stop::AfterAttempting).is_err());
    assert!(
        f.session()
            .recover_with(
                prepared["operationId"].as_str().unwrap(),
                &store,
                &|_| Ok(oauth.clone()),
                &server.http(),
                &Budget::new()
            )
            .is_err()
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert_eq!(oauth.calls.load(SeqCst), 4);
    assert_eq!(server.patches(), 0);
}

#[test]
fn recovery_unavailable_or_untrusted_gets_remain_retryable_without_history_growth() {
    let f = Fixture::new();
    let server = Server::new();
    let prepared = f.prepare(&server);
    *server.mode.lock().unwrap() = Mode::Lost;
    let uncertain = f.apply(&server, &prepared, Stop::None).unwrap();
    assert_eq!(uncertain["phase"], "Uncertain");
    fs::remove_dir_all(&f.repo).unwrap();
    fs::remove_file(&f.parser).unwrap();

    // Even error responses carrying an otherwise healthy desired resource must
    // not establish a fact, close recovery, or append redundant state history.
    for mode in [
        Mode::GetStatus(503),
        Mode::GetStatus(429),
        Mode::GetStatus(500),
        Mode::GetStatus(404),
        Mode::GetStatus(302),
        Mode::MalformedGet,
        Mode::HeaderMismatchGet,
    ] {
        *server.mode.lock().unwrap() = mode;
        for _ in 0..2 {
            let before = server.requests.lock().unwrap().len();
            let recovered = f.recover(&server, &prepared);
            assert_eq!(recovered["phase"], "Uncertain");
            assert_eq!(recovered["stateOid"], uncertain["stateOid"]);
            assert_eq!(server.requests.lock().unwrap().len(), before + 1);
        }
    }
    *server.mode.lock().unwrap() = Mode::Normal;
    let healthy = server.raw.lock().unwrap().clone();
    for case in [
        "wrong-id",
        "weak-etag",
        "updated",
        "missing-title",
        "unsupported",
    ] {
        {
            let mut raw = server.raw.lock().unwrap();
            *raw = healthy.clone();
            match case {
                "wrong-id" => raw["id"] = json!("not-the-selected-task"),
                "weak-etag" => raw["etag"] = json!("W/\"weak\""),
                "updated" => raw["updated"] = json!("invalid timestamp"),
                "missing-title" => {
                    raw.as_object_mut().unwrap().remove("title");
                }
                _ => raw["links"] = json!([{"unsupported":"link"}]),
            }
        }
        let recovered = f.recover(&server, &prepared);
        assert_eq!(recovered["phase"], "Uncertain", "{case}");
        assert_eq!(recovered["stateOid"], uncertain["stateOid"], "{case}");
    }
    *server.raw.lock().unwrap() = healthy;
    let recovered = f.recover(&server, &prepared);
    assert_eq!(recovered["phase"], "RecoveredSatisfied");
    assert_eq!(recovered["attribution"], "UNKNOWN");
    assert_eq!(server.patches(), 1);
}

// Retained, locally cancelled preparations can legitimately repeat the exact
// same committed source and unchanged GET basis, with fresh operation nonces.
fn fill_cancelled_history(state: &mut State, target_bytes: usize) {
    let template = serde_json::to_vec(state.operations.last().unwrap()).unwrap();
    let mut bytes = serde_json::to_vec(state).unwrap().len();
    loop {
        let mut old: Operation = serde_json::from_slice(&template).unwrap();
        old.intent.nonce = hash(format!("cancelled-history-{}", state.operations.len()).as_bytes());
        old.intent_sha256 = digest(&old.intent).unwrap();
        old.operation_id = old.intent_sha256.clone();
        old.phases = vec![Phase::Prepared, Phase::NotDispatched];
        old.patch_evidence = None;
        old.confirmation = None;
        old.recovery_evidence = None;
        old.attempt_evidence = None;
        // An inserted operation contributes its compact JSON plus one array comma.
        // Avoid serializing the entire growing history on every iteration.
        let added_bytes = serde_json::to_vec(&old).unwrap().len() + 1;
        if bytes + added_bytes > target_bytes {
            break;
        }
        let index = state.operations.len() - 1;
        state.operations.insert(index, old);
        bytes += added_bytes;
        assert!(state.operations.len() < 128);
    }
    assert_eq!(serde_json::to_vec(state).unwrap().len(), bytes);
}

#[test]
fn evidence_headroom_rejects_prepare_and_apply_but_does_not_lock_out_attempted_recovery() {
    let f = Fixture::with_source_padding(32 * 1024);
    let server = Server::new();
    f.prepare(&server);
    let session = f.session();
    let (mut oid, mut state) = session.load().unwrap();
    fill_cancelled_history(&mut state, storage::LIMIT - EVIDENCE_RESERVE + 64 * 1024);
    state
        .operations
        .last_mut()
        .unwrap()
        .phases
        .push(Phase::NotDispatched);
    state.validate(&session.binding).unwrap();
    session.persist(&mut oid, &state).unwrap();
    let oauth = OAuth::new();
    let error = session
        .prepare_with(
            &f.commit,
            &Store::new(),
            &|_| Ok(oauth.clone()),
            &server.http(),
            &Budget::new(),
            Stop::None,
        )
        .unwrap_err();
    assert!(error.to_string().contains("headroom"), "{error}");
    assert_eq!(session.load().unwrap().0, oid);

    // Simulate a pre-fix, structurally valid Prepared state below the storage cap.
    // A real committed source supplies its large literal evidence, not JSON padding.
    state.operations.last_mut().unwrap().phases = vec![Phase::Prepared];
    fill_cancelled_history(&mut state, storage::LIMIT - 4096);
    let bytes = serde_json::to_vec(&state).unwrap().len();
    assert!(bytes > storage::LIMIT - 64 * 1024 && bytes < storage::LIMIT);
    assert!(
        state
            .validate(&session.binding)
            .unwrap_err()
            .to_string()
            .contains("headroom")
    );
    oid = storage::save_ref(&f.state, STATE_REF, Some(&oid), &state).unwrap();
    let approved = report(&state, &oid, false);
    let store = Store::new();
    let oauth = OAuth::new();
    let reads = server.requests.lock().unwrap().len();
    let error = session
        .apply_with(
            &approval(&approved),
            &store,
            &|_| Ok(oauth.clone()),
            &server.http(),
            &Budget::new(),
            Stop::None,
        )
        .unwrap_err();
    assert!(error.to_string().contains("headroom"), "{error}");
    assert_eq!(store.locks.load(SeqCst), 0);
    assert_eq!(oauth.calls.load(SeqCst), 0);
    assert_eq!(server.requests.lock().unwrap().len(), reads);
    assert_eq!(storage::check_repo_ref(&f.state, STATE_REF).unwrap(), oid);
    assert_eq!(server.patches(), 0);

    // Already-attempted state is not retroactively rejected by the new admission
    // invariant; GET-only recovery can still persist whatever evidence fits.
    state
        .operations
        .last_mut()
        .unwrap()
        .phases
        .push(Phase::Attempting);
    state.validate(&session.binding).unwrap();
    session.persist(&mut oid, &state).unwrap();
    drop(session);
    fs::remove_dir_all(&f.repo).unwrap();
    fs::remove_file(&f.parser).unwrap();
    assert_eq!(f.recover(&server, &approved)["phase"], "Uncertain");
    server.raw.lock().unwrap()["due"] = json!("2026-11-10T00:00:00Z");
    assert_eq!(f.recover(&server, &approved)["phase"], "RecoveredSatisfied");
    assert_eq!(server.patches(), 0);
}

#[test]
fn admitted_near_boundary_state_persists_patch_confirmation_and_recovery_together() {
    let f = Fixture::with_source_padding(32 * 1024);
    let server = Server::new();
    {
        let mut raw = server.raw.lock().unwrap();
        raw["position"] = json!("\\".repeat(4096));
        raw["etag"] = json!(format!("\"{}\"", "\\".repeat(1022)));
        raw["selfLink"] = json!(format!(
            "https://tasks.googleapis.com/tasks/v1/lists/list/tasks/{}",
            "a".repeat(4000)
        ));
        raw["webViewLink"] = json!(format!(
            "https://tasks.google.com/task/{}",
            "a".repeat(4000)
        ));
    }
    f.prepare(&server);
    let session = f.session();
    let (mut oid, mut state) = session.load().unwrap();
    fill_cancelled_history(&mut state, storage::LIMIT - EVIDENCE_RESERVE);
    let admitted_bytes = serde_json::to_vec(&state).unwrap().len();
    assert!(admitted_bytes > storage::LIMIT - EVIDENCE_RESERVE - 64 * 1024);
    state.validate(&session.binding).unwrap();
    session.persist(&mut oid, &state).unwrap();
    let approved = session.inspect().unwrap();
    drop(session);

    // This crash point retains both validated PATCH and confirmation evidence
    // before the Applied CAS; recovery must have room for a third full resource.
    assert!(f.apply(&server, &approved, Stop::OutcomeCas).is_err());
    fs::remove_dir_all(&f.repo).unwrap();
    fs::remove_file(&f.parser).unwrap();
    assert_eq!(f.recover(&server, &approved)["phase"], "RecoveredSatisfied");
    let session = f.session();
    let (_, state) = session.load().unwrap();
    let op = state.operations.last().unwrap();
    for resource in [&op.patch_evidence, &op.confirmation, &op.recovery_evidence] {
        let resource = resource.as_ref().unwrap();
        assert!(serde_json::to_vec(&resource.raw).unwrap().len() > 16 * 1024);
        assert!(
            serde_json::to_vec(resource).unwrap().len()
                <= crate::publisher_http::RESOURCE_LIMIT + 2 * 1024 + 32
        );
    }
    let final_bytes = serde_json::to_vec(&state).unwrap().len();
    assert!(final_bytes > admitted_bytes && final_bytes <= storage::LIMIT);
    assert_eq!(server.patches(), 1);
}

#[test]
fn near_limit_applied_state_remains_inspectable_and_terminal_without_source_or_get() {
    let f = Fixture::with_source_padding(32 * 1024);
    let server = Server::new();
    let prepared = f.prepare(&server);
    let applied = f
        .apply(&server, &prepared, Stop::None)
        .context("near-limit fixture initial due-only apply")
        .unwrap();
    assert_eq!(
        applied["phase"], "Applied",
        "fixture did not reach Applied before history construction; attemptEvidence={}",
        applied["operations"][0]["attemptEvidence"]
    );
    let session = f.session();
    let (mut oid, mut state) = session.load().unwrap();
    fill_cancelled_history(&mut state, storage::LIMIT - 4096);
    assert!(serde_json::to_vec(&state).unwrap().len() > storage::LIMIT - 64 * 1024);
    state
        .validate(&session.binding)
        .context("near-limit Applied fixture validation")
        .unwrap();
    session
        .persist(&mut oid, &state)
        .context("near-limit Applied fixture persistence")
        .unwrap();
    drop(session);
    fs::remove_dir_all(&f.repo).unwrap();
    fs::remove_file(&f.parser).unwrap();
    let reads = server.requests.lock().unwrap().len();
    let mut session = f.session();
    assert_eq!(
        session
            .inspect()
            .context("near-limit Applied inspection without source")
            .unwrap()["phase"],
        "Applied"
    );
    let recovered = session
        .recover(prepared["operationId"].as_str().unwrap())
        .context("near-limit Applied terminal recovery without source")
        .unwrap();
    assert_eq!(recovered["phase"], "Applied");
    assert_eq!(recovered["stateOid"], oid);
    assert_eq!(server.requests.lock().unwrap().len(), reads);
    assert_eq!(server.patches(), 1);
}
