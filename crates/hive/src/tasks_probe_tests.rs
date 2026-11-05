use super::*;
use hive_google_auth::{AuthError, CredentialGuard, GrantRecord, StoredConnection};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    thread,
    time::{Duration, Instant},
};

const D0: &str = "2026-11-10";
const D1: &str = "2026-11-11";
const E0: &str = "\"e0\"";
const E1: &str = "\"e1\"";
const TOKEN: &str = "probe-bearer-canary";

fn resource(etag: &str, due: &str) -> Value {
    json!({"id":"task/one", "title":"Disposable fixture", "status":"needsAction", "kind":"tasks#task",
        "etag":etag,"due":format!("{due}T00:00:00.000Z"),"updated":"2026-11-04T00:00:00Z","position":"001","links":[]})
}
fn selector(path: PathBuf) -> Selector {
    Selector {
        connection: "fixture".into(),
        expected_account_key: identity().namespace().unwrap(),
        list_id: "list/one".into(),
        task_id: "task/one".into(),
        journal: path,
    }
}
fn run(path: PathBuf) -> ProbeCommand {
    ProbeCommand::Run(Run {
        selector: selector(path),
        expected_etag: E0.into(),
        expected_original_due: D0.into(),
        chosen_due: D1.into(),
        expected_baseline_sha256: digest(&resource(E0, D0)).unwrap(),
        acknowledge_disposable: true,
    })
}
fn identity() -> GoogleIdentity {
    GoogleIdentity::new("subject-canary-never-log").unwrap()
}
fn temp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}
fn path(t: &tempfile::TempDir) -> PathBuf {
    t.path().canonicalize().unwrap().join("probe.jsonl")
}

type AppendHook = Box<dyn Fn(&Value) + Send>;

#[derive(Default)]
struct Memory {
    records: Vec<Value>,
    calls: usize,
    fail_at: usize,
    on_append: Option<AppendHook>,
}
struct MemorySink(Arc<Mutex<Memory>>);
impl journal::Sink for MemorySink {
    fn append(&mut self, v: &Value) -> Result<()> {
        let mut m = self.0.lock().unwrap();
        m.calls += 1;
        if m.calls == m.fail_at {
            anyhow::bail!("injected journal failure");
        }
        m.records.push(v.clone());
        if let Some(hook) = &m.on_append {
            hook(v);
        }
        Ok(())
    }
}
fn memory(fail_at: usize) -> (Journal, Arc<Mutex<Memory>>) {
    let m = Arc::new(Mutex::new(Memory {
        fail_at,
        ..Memory::default()
    }));
    (Journal::new(Box::new(MemorySink(m.clone()))), m)
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    headers: String,
    delay: Duration,
    before_response: Option<Box<dyn FnOnce() + Send>>,
}
fn reply(status: u16, body: Value) -> Reply {
    Reply {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: String::new(),
        delay: Duration::ZERO,
        before_response: None,
    }
}
fn success_replies() -> Vec<Reply> {
    vec![
        reply(200, resource(E0, D0)),
        reply(200, resource(E1, D1)),
        reply(200, resource(E1, D1)),
        reply(
            412,
            json!({"error":{"code":412,"message":"Precondition Failed","errors":[{"reason":"conditionNotMet"}]}}),
        ),
        reply(200, resource(E1, D1)),
    ]
}

fn read_request(stream: &mut TcpStream) -> String {
    // Darwin can inherit the listener's nonblocking flag on accepted sockets.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let n = stream.read(&mut buffer).unwrap();
        assert_ne!(n, 0, "incomplete request");
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..index]).to_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap_or("0")
                .parse()
                .unwrap();
            if bytes.len() >= index + 4 + length {
                break;
            }
        }
        assert!(bytes.len() < RECORD_LIMIT);
    }
    String::from_utf8(bytes).unwrap()
}

// The only alternate-endpoint constructor is private and test-only. The production
// adapter's actual builder, URL encoding, headers, and body reader run here.
fn server(replies: Vec<Reply>, timeout: Duration) -> (Http, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}/tasks/v1/lists/", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in replies {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing expected request");
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => panic!("listener: {e}"),
                }
            };
            requests.push(read_request(&mut stream));
            thread::sleep(response.delay);
            if let Some(hook) = response.before_response {
                hook();
            }
            let head = format!(
                "HTTP/1.1 {} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                response.status,
                response.body.len(),
                response.headers
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&response.body);
        }
        // Detect prohibited follow-up requests rather than simply dropping the listener.
        let deadline = Instant::now() + Duration::from_millis(50);
        while Instant::now() < deadline {
            assert!(
                listener.accept().is_err(),
                "unexpected retry, rollback, or follow-up request"
            );
            thread::sleep(Duration::from_millis(1));
        }
        requests
    });
    (Http::loopback(&base, timeout), handle)
}

fn exercise(replies: Vec<Reply>) -> (Result<Value>, Vec<String>, Arc<Mutex<Memory>>) {
    let t = temp();
    let command = run(path(&t));
    let (journal, records) = memory(0);
    freeze(&command, &journal).unwrap();
    let (http, server) = server(replies, Duration::from_secs(1));
    let result = finish(execute(&command, &journal, &http, TOKEN), &journal);
    (result, server.join().unwrap(), records)
}

#[test]
fn matching_accepted_stale_rejected_real_loopback_wire_contract() {
    let (result, requests, records) = exercise(success_replies());
    let report = result.unwrap();
    assert_eq!(report["result"], "matchingAcceptedAndStaleRejected");
    assert_eq!(report["googleConditionalBehaviorObserved"], false);
    assert_eq!(report["origin"], "loopback test only; no Google validation");
    assert_eq!(report["productionReady"], false);
    assert_eq!(requests.len(), 5);
    for (i, request) in requests.iter().enumerate() {
        let method = if i == 1 || i == 3 { "PATCH" } else { "GET" };
        assert!(
            request.starts_with(&format!(
                "{method} /tasks/v1/lists/list%2Fone/tasks/task%2Fone HTTP/1.1\r\n"
            )),
            "{request}"
        );
        assert!(
            request
                .to_lowercase()
                .contains(&format!("authorization: bearer {TOKEN}\r\n"))
        );
        if method == "PATCH" {
            assert!(request.contains(&format!("if-match: {E0}\r\n")));
            let body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(
                body,
                json!({"due":format!("{}T00:00:00.000Z",if i==1 {D1} else {D0})})
            );
        } else {
            assert!(!request.contains("if-match:"));
        }
    }
    let encoded = serde_json::to_string(&records.lock().unwrap().records).unwrap();
    assert!(!encoded.contains(TOKEN));
    assert!(!encoded.contains("subject-canary"));
}

#[test]
fn stale_2xx_is_failure_even_when_unchanged_or_restored_or_malformed() {
    for body in [resource("\"e2\"", D0), resource(E1, D1), Value::Null] {
        for status in [200, 204, 299] {
            let mut responses = success_replies();
            responses.truncate(4);
            responses[3] = reply(status, body.clone());
            let (r, requests, _) = exercise(responses);
            assert!(
                r.unwrap_err()
                    .to_string()
                    .starts_with("failed: stale PATCH accepted")
            );
            assert_eq!(requests.len(), 4);
        }
    }
}

#[test]
fn adversarial_server_ignores_stale_precondition_actually_writes_d0_and_probe_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let http = Http::loopback(
        &format!("http://{}/tasks/v1/lists/", listener.local_addr().unwrap()),
        Duration::from_secs(2),
    );
    let server = thread::spawn(move || {
        let mut due = D0.to_owned();
        let mut writes = 0;
        for expected_method in ["GET", "PATCH", "GET", "PATCH"] {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                if let Ok((stream, _)) = listener.accept() {
                    break stream;
                }
                assert!(Instant::now() < deadline, "missing expected request");
                thread::sleep(Duration::from_millis(1));
            };
            let request = read_request(&mut stream);
            assert!(request.starts_with(expected_method));
            if expected_method == "PATCH" {
                assert!(request.contains(&format!("if-match: {E0}\r\n")));
                let patch: Value =
                    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                // Deliberately ignore If-Match, just like a broken provider.
                due = patch["due"].as_str().unwrap()[..10].to_owned();
                writes += 1;
                assert_eq!(due, if writes == 1 { D1 } else { D0 });
            }
            let body = serde_json::to_vec(&resource(&format!("\"e{writes}\""), &due)).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
        let deadline = Instant::now() + Duration::from_millis(50);
        while Instant::now() < deadline {
            assert!(
                listener.accept().is_err(),
                "no third PATCH, rollback, or follow-up GET"
            );
            thread::sleep(Duration::from_millis(1));
        }
        (due, writes)
    });
    let t = temp();
    let (journal, _) = memory(0);
    let error = execute(&run(path(&t)), &journal, &http, TOKEN)
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("failed: stale PATCH accepted"));
    assert_eq!(server.join().unwrap(), (D0.to_owned(), 2));
}

#[test]
fn stale_2xx_oversize_still_explicit_failure_not_inconclusive() {
    let mut responses = success_replies();
    responses.truncate(4);
    responses[3].status = 200;
    responses[3].body = vec![b'x'; RECORD_LIMIT + 1];
    assert!(
        exercise(responses)
            .0
            .unwrap_err()
            .to_string()
            .starts_with("failed: stale PATCH accepted")
    );
}

#[test]
fn other_stale_statuses_inconclusive_no_final_get() {
    for status in [301, 401, 403, 409, 429, 500] {
        let mut responses = success_replies();
        responses.truncate(4);
        responses[3].status = status;
        assert!(
            exercise(responses)
                .0
                .unwrap_err()
                .to_string()
                .starts_with("inconclusive: stale PATCH")
        );
    }
}

#[test]
fn baseline_drift_missing_weak_etags_and_task_shapes_stop_before_patch() {
    let base = resource(E0, D0);
    let cases = [
        ("etag", Value::Null),
        ("etag", json!("W/\"e0\"")),
        ("etag", json!("\"different\"")),
        ("id", json!("other")),
        ("title", Value::Null),
        ("status", Value::Null),
        ("status", json!("completed")),
        ("deleted", json!(true)),
        ("hidden", json!(true)),
        ("parent", json!("parent")),
        ("notes", json!("notes")),
        ("assignmentInfo", json!({"x":true})),
        ("unknown", json!("field")),
        ("due", Value::Null),
        ("due", json!("2026-02-30T00:00:00.000Z")),
        ("due", json!("2026-11-10T01:00:00.000+01:00")),
        ("due", json!("2026-11-12T00:00:00Z")),
        ("title", json!("changed title")),
        ("updated", json!("invalid")),
        ("position", json!(123)),
    ];
    for (key, value) in cases {
        let mut body = base.clone();
        body[key] = value;
        let (r, requests, _) = exercise(vec![reply(200, body)]);
        assert!(r.is_err(), "accepted {key}");
        assert_eq!(requests.len(), 1);
    }
    for key in ["id", "title", "status", "etag"] {
        let mut body = base.clone();
        body.as_object_mut().unwrap().remove(key);
        assert!(
            exercise(vec![reply(200, body)]).0.is_err(),
            "accepted missing {key}"
        );
    }
}

#[test]
fn matching_response_invalid_stops_without_get_or_stale_patch() {
    for (key, value) in [
        ("id", json!("other")),
        ("due", json!(format!("{D0}T00:00:00.000Z"))),
        ("title", json!("unexpected")),
        ("etag", json!("W/\"e1\"")),
        ("updated", json!("bad")),
    ] {
        let mut changed = resource(E1, D1);
        changed[key] = value;
        let (r, requests, _) = exercise(vec![reply(200, resource(E0, D0)), reply(200, changed)]);
        assert!(r.is_err());
        assert_eq!(requests.len(), 2);
    }
    for status in [201, 204, 302, 401, 412, 500] {
        assert!(
            exercise(vec![
                reply(200, resource(E0, D0)),
                reply(status, resource(E1, D1))
            ])
            .0
            .is_err()
        );
    }
}

#[test]
fn unchanged_or_drifting_changed_etag_stops_before_stale_patch() {
    for changed in [resource(E0, D1), resource("\"e2\"", D1), resource(E1, D0)] {
        let (r, requests, _) = exercise(vec![
            reply(200, resource(E0, D0)),
            reply(200, resource(E1, D1)),
            reply(200, changed),
        ]);
        assert!(r.unwrap_err().to_string().starts_with("inconclusive:"));
        assert_eq!(requests.len(), 3);
    }
}

#[test]
fn final_external_edits_are_inconclusive() {
    for (key, value) in [
        ("etag", json!("\"external\"")),
        ("due", json!(format!("{D0}T00:00:00.000Z"))),
        ("title", json!("external")),
    ] {
        let mut responses = success_replies();
        let mut body = resource(E1, D1);
        body[key] = value;
        responses[4] = reply(200, body);
        assert!(
            exercise(responses)
                .0
                .unwrap_err()
                .to_string()
                .starts_with("inconclusive:")
        );
    }
}

#[test]
fn validated_updated_metadata_can_change_but_other_metadata_cannot() {
    let mut responses = success_replies();
    for i in [1, 2, 4] {
        let mut body = resource(E1, D1);
        body["updated"] = json!("2026-11-05T12:00:00.000Z");
        responses[i] = reply(200, body);
    }
    assert!(exercise(responses).0.is_ok());
    let mut body = resource(E1, D1);
    body["position"] = json!("002");
    assert!(
        exercise(vec![reply(200, resource(E0, D0)), reply(200, body)])
            .0
            .is_err()
    );
}

#[test]
fn http_resource_etags_must_agree() {
    for header in [
        "etag: \"other\"\r\n",
        "etag: W/\"e0\"\r\n",
        "etag: \"e0\"\r\netag: \"e0\"\r\n",
    ] {
        let mut r = reply(200, resource(E0, D0));
        r.headers = header.into();
        assert!(exercise(vec![r]).0.is_err());
    }
    let mut responses = success_replies();
    responses[0].headers = format!("etag: {E0}\r\n");
    assert!(exercise(responses).0.is_ok());
}

#[test]
fn malformed_oversize_bodies_and_timeouts_are_static_stop_errors() {
    for body in [
        b"provider-body-canary".to_vec(),
        vec![b'x'; RECORD_LIMIT + 1],
    ] {
        let mut r = reply(200, Value::Null);
        r.body = body;
        let error = exercise(vec![r]).0.unwrap_err().to_string();
        assert!(!error.contains("provider-body-canary"));
        assert!(!error.contains("http://"));
    }
    let t = temp();
    let command = run(path(&t));
    let (journal, _) = memory(0);
    let mut r = reply(200, resource(E0, D0));
    r.delay = Duration::from_millis(100);
    let (http, handle) = server(vec![r], Duration::from_millis(20));
    let error = execute(&command, &journal, &http, TOKEN)
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("inconclusive: Tasks transport"));
    assert!(!error.contains(TOKEN));
    assert_eq!(handle.join().unwrap().len(), 1);
}

#[test]
fn redirect_is_blocked_and_secondary_endpoint_never_receives_bearer() {
    let secondary = TcpListener::bind("127.0.0.1:0").unwrap();
    secondary.set_nonblocking(true).unwrap();
    let mut r = reply(302, json!({}));
    r.headers = format!(
        "Location: http://{}/stolen\r\n",
        secondary.local_addr().unwrap()
    );
    assert!(exercise(vec![r]).0.is_err());
    assert!(secondary.accept().is_err());
}

#[test]
fn preflight_invalid_inputs_never_create_journal_or_reach_native_auth() {
    let t = temp();
    for field in [
        "ack",
        "etag",
        "original",
        "chosen",
        "same",
        "hash",
        "account",
        "connection",
        "list",
        "task",
        "default",
        "empty",
        "long",
        "space",
        "relative",
    ] {
        let mut c = run(path(&t));
        let ProbeCommand::Run(r) = &mut c else {
            unreachable!()
        };
        match field {
            "ack" => r.acknowledge_disposable = false,
            "etag" => r.expected_etag = "*".into(),
            "original" => r.expected_original_due = "2026-02-30".into(),
            "chosen" => r.chosen_due = "2026-11-11Z".into(),
            "same" => r.chosen_due = D0.into(),
            "hash" => r.expected_baseline_sha256 = "abc".into(),
            "account" => r.selector.expected_account_key = "legacy:subject".into(),
            "connection" => r.selector.connection = "../bad".into(),
            "list" => r.selector.list_id = ".".into(),
            "task" => r.selector.task_id = "bad\n".into(),
            "default" => r.selector.list_id = "@default".into(),
            "empty" => r.selector.task_id.clear(),
            "long" => r.selector.list_id = "x".repeat(257),
            "space" => r.selector.list_id = " list ".into(),
            "relative" => r.selector.journal = "relative".into(),
            _ => unreachable!(),
        }
        assert!(command(c).is_err(), "{field}");
        assert!(!path(&t).exists());
    }
}

#[test]
fn strong_etags_are_exact_not_requoted_and_dates_are_calendar_dates() {
    for bad in [
        "",
        "\"\"",
        "*",
        "W/\"x\"",
        "\"x\",\"y\"",
        "\"x,y\"",
        "\"x\n\"",
        " x ",
        "\"a\"b\"",
        "\"\u{80}\"",
    ] {
        assert!(!strong_etag(bad), "{bad:?}");
    }
    assert!(!strong_etag(&format!("\"{}\"", "x".repeat(1024))));
    for good in [E0, "\"a-b:c\"", "\"a\\b\""] {
        assert!(strong_etag(good));
    }
    for bad in [
        "2026-2-01",
        "0000-01-01",
        "2026-02-29",
        "2026-11-11T00:00:00Z",
    ] {
        assert!(!date(bad));
    }
    assert!(date("2028-02-29"));
}

#[test]
fn exclusive_private_append_journal_refuses_existing_files_symlinks_and_noncanonical_parents() {
    let t = temp();
    let p = path(&t);
    let journal = Journal::create(&p).unwrap();
    journal.record(json!({"event":"one"})).unwrap();
    journal.record(json!({"event":"two"})).unwrap();
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(Journal::create(&p).is_err());
    drop(journal);
    assert!(Journal::create(&p).is_err());
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(text.lines().count(), 2);
    let link = p.with_file_name("symlink");
    std::os::unix::fs::symlink(&p, &link).unwrap();
    assert!(Journal::create(&link).is_err());
    let parent = p.with_file_name("parentlink");
    std::os::unix::fs::symlink(p.parent().unwrap(), &parent).unwrap();
    assert!(Journal::create(&parent.join("new")).is_err());
    assert!(Journal::create(&p.parent().unwrap().join("../probe-new")).is_err());
}

#[test]
fn journal_record_and_total_caps_stop_and_poison_sink() {
    let t = temp();
    let journal = Journal::create(&path(&t)).unwrap();
    assert!(
        journal
            .record(json!({"x":"a".repeat(RECORD_LIMIT)}))
            .is_err()
    );
    assert!(journal.record(json!({"event":"later"})).is_err());
    let journal = Journal::create(&path(&t).with_file_name("total")).unwrap();
    for _ in 0..16 {
        journal.record(json!({"x":"a".repeat(63*1024)})).unwrap();
    }
    assert!(journal.record(json!({"x":"a".repeat(63*1024)})).is_err());
}

#[test]
fn journal_failure_before_or_after_each_tasks_request_stops_all_followups() {
    // Each request has attempted, HTTP status, then response-outcome records.
    for fail_at in 1..=15 {
        let t = temp();
        let c = run(path(&t));
        let (journal, records) = memory(fail_at);
        let count = if fail_at % 3 == 1 {
            (fail_at - 1) / 3
        } else {
            fail_at.div_ceil(3)
        };
        let mut responses = success_replies();
        responses.truncate(count);
        let (http, handle) = server(responses, Duration::from_secs(1));
        let error = execute(&c, &journal, &http, TOKEN).unwrap_err().to_string();
        assert!(error.contains("journal"), "failure at {fail_at}: {error}");
        assert!(journal.record(json!({"event":"retry"})).is_err());
        assert_eq!(handle.join().unwrap().len(), count);
        assert_eq!(records.lock().unwrap().calls, fail_at);
    }
}

struct Guard;
impl CredentialGuard for Guard {}
struct Store {
    denied: bool,
}
impl CredentialStore for Store {
    fn profile(&self) -> AuthProfile {
        AuthProfile::TasksWrite
    }
    fn lock(&self, _: &str) -> hive_google_auth::Result<Box<dyn CredentialGuard + '_>> {
        if self.denied {
            Err(AuthError::StorageDenied)
        } else {
            Ok(Box::new(Guard))
        }
    }
    fn load(&self, c: &str) -> hive_google_auth::Result<Option<StoredConnection>> {
        Ok(Some(StoredConnection {
            version: 1,
            connection: c.into(),
            identity: identity(),
            generation: 1,
            grant: Some(GrantRecord {
                client: ClientConfig {
                    client_id: "fixture.apps.googleusercontent.com".into(),
                    client_secret: None,
                },
                refresh_token: SecretString::new("refresh-canary").unwrap(),
                scopes: AuthProfile::TasksWrite
                    .scopes()
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            }),
        }))
    }
    fn save(&self, _: &str, _: &StoredConnection) -> hive_google_auth::Result<()> {
        panic!("no rotation in fixture")
    }
}
struct OAuth {
    calls: AtomicUsize,
    mismatch: bool,
    denied: bool,
}
impl OAuthTransport for &OAuth {
    fn exchange(
        &self,
        _: &ClientConfig,
        _: &AuthorizationGrant,
    ) -> hive_google_auth::Result<TokenSet> {
        panic!("never login")
    }
    fn refresh(&self, _: &ClientConfig, _: &SecretString) -> hive_google_auth::Result<TokenSet> {
        self.calls.fetch_add(1, SeqCst);
        if self.denied {
            return Err(AuthError::Denied);
        }
        Ok(TokenSet {
            access_token: SecretString::new(TOKEN).unwrap(),
            refresh_token: None,
            expires_in: 300,
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
        self.calls.fetch_add(1, SeqCst);
        assert_eq!(token.expose(), TOKEN);
        Ok(if self.mismatch {
            GoogleIdentity::new("other-canary").unwrap()
        } else {
            identity()
        })
    }
}
fn oauth() -> OAuth {
    OAuth {
        calls: AtomicUsize::new(0),
        mismatch: false,
        denied: false,
    }
}

#[test]
fn fresh_auth_and_inspection_use_same_bearer_without_write_authority() {
    let t = temp();
    let c = ProbeCommand::Inspect(selector(path(&t)));
    let (journal, records) = memory(0);
    freeze(&c, &journal).unwrap();
    let oauth = oauth();
    let (http, handle) = server(vec![reply(200, resource(E0, D0))], Duration::from_secs(1));
    let report = finish(
        collect(
            &c,
            &journal,
            &Store { denied: false },
            &|_| Ok(&oauth),
            &http,
        ),
        &journal,
    )
    .unwrap();
    assert_eq!(oauth.calls.load(SeqCst), 2);
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains(TOKEN));
    assert_eq!(report["baselineSha256"], digest(&resource(E0, D0)).unwrap());
    assert_eq!(report["originalDue"], D0);
    assert_eq!(report["originalEtag"], E0);
    assert_eq!(report["executionCapability"], false);
    let records = &records.lock().unwrap().records;
    assert_eq!(records[0]["event"], "frozenBinding");
    assert_eq!(records[1]["stage"], "authRefresh");
    assert_eq!(records[3]["stage"], "authUserInfo");
    let text = serde_json::to_string(records).unwrap();
    for secret in [TOKEN, "refresh-canary", "subject-canary"] {
        assert!(!text.contains(secret));
    }
}

#[test]
fn denied_auth_and_both_account_mismatches_make_zero_tasks_requests() {
    for case in ["storage", "refresh", "userinfo", "expected"] {
        let t = temp();
        let mut c = run(path(&t));
        let (journal, _) = memory(0);
        let mut oauth = oauth();
        if case == "refresh" {
            oauth.denied = true;
        }
        if case == "userinfo" {
            oauth.mismatch = true;
        }
        if case == "expected" {
            let ProbeCommand::Run(r) = &mut c else {
                unreachable!()
            };
            r.selector.expected_account_key = format!("google-sub-v1:{}", "a".repeat(64));
        }
        let (http, handle) = server(vec![], Duration::from_secs(1));
        assert!(
            collect(
                &c,
                &journal,
                &Store {
                    denied: case == "storage"
                },
                &|_| Ok(&oauth),
                &http
            )
            .is_err()
        );
        assert!(handle.join().unwrap().is_empty());
        assert_eq!(
            oauth.calls.load(SeqCst),
            match case {
                "storage" => 0,
                "refresh" => 1,
                _ => 2,
            }
        );
    }
}

#[test]
fn journal_failures_around_oauth_stop_before_any_later_request() {
    // Frozen binding, refresh attempt/outcome, identity attempt/outcome.
    for fail_at in 1..=5 {
        let t = temp();
        let c = run(path(&t));
        let (journal, _) = memory(fail_at);
        let oauth = oauth();
        let (http, handle) = server(vec![], Duration::from_secs(1));
        let result = freeze(&c, &journal).and_then(|_| {
            collect(
                &c,
                &journal,
                &Store { denied: false },
                &|_| Ok(&oauth),
                &http,
            )
        });
        assert!(result.is_err());
        assert!(handle.join().unwrap().is_empty());
        assert_eq!(
            oauth.calls.load(SeqCst),
            match fail_at {
                1 | 2 => 0,
                3 | 4 => 1,
                _ => 2,
            }
        );
    }
}

#[test]
fn report_cap_prevents_output() {
    let (journal, _) = memory(0);
    assert!(finish(Ok(json!({"x":"x".repeat(RECORD_LIMIT)})), &journal).is_err());
}

#[tokio::test]
async fn probe_constructs_and_drops_blocking_http_only_inside_tasks_dispatch() {
    super::super::dispatch(|| {
        assert!(exercise(success_replies()).0.is_ok());
        Ok(())
    })
    .await
    .unwrap();
}

#[test]
fn terminal_journal_failure_does_not_publish_success_or_make_more_requests() {
    let t = temp();
    let c = run(path(&t));
    let (journal, records) = memory(16);
    let (http, handle) = server(success_replies(), Duration::from_secs(1));
    let result = finish(execute(&c, &journal, &http, TOKEN), &journal);
    assert!(result.unwrap_err().to_string().contains("journal"));
    assert_eq!(handle.join().unwrap().len(), 5);
    assert!(
        !records
            .lock()
            .unwrap()
            .records
            .iter()
            .any(|v| v["event"] == "resultCandidate")
    );
}

#[test]
fn matching_patch_malformed_oversize_or_timeout_never_reaches_stale_patch() {
    for oversized in [false, true] {
        let mut matching = reply(200, Value::Null);
        matching.body = if oversized {
            vec![b'x'; RECORD_LIMIT + 1]
        } else {
            b"body-canary-invalid-json".to_vec()
        };
        let (result, requests, _) = exercise(vec![reply(200, resource(E0, D0)), matching]);
        assert!(result.is_err());
        assert_eq!(requests.len(), 2);
        assert!(!result.unwrap_err().to_string().contains("body-canary"));
    }
    let t = temp();
    let command = run(path(&t));
    let (journal, _) = memory(0);
    let mut matching = reply(200, resource(E1, D1));
    matching.delay = Duration::from_millis(150);
    let (http, handle) = server(
        vec![reply(200, resource(E0, D0)), matching],
        Duration::from_millis(50),
    );
    let error = execute(&command, &journal, &http, TOKEN)
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("inconclusive: Tasks transport"));
    assert_eq!(handle.join().unwrap().len(), 2);
}

#[test]
fn baseline_digest_is_canonical_full_resource_not_just_semantic_fields() {
    let first: Value =
        serde_json::from_str(r#"{"id":"one", "title":"title", "etag":"quoted"}"#).unwrap();
    let reordered: Value =
        serde_json::from_str(r#"{ "etag":"quoted", "title":"title", "id":"one" }"#).unwrap();
    assert_eq!(digest(&first).unwrap(), digest(&reordered).unwrap());
    for (key, value) in [
        ("etag", json!("other")),
        ("updated", json!("2026-11-04T00:00:00Z")),
        ("notes", Value::Null),
    ] {
        let mut changed = first.clone();
        changed[key] = value;
        assert_ne!(digest(&first).unwrap(), digest(&changed).unwrap());
    }
}

#[test]
fn stopped_journal_reports_distinguish_failure_unsupported_and_inconclusive() {
    for (reply, expected) in [
        (reply(200, resource("\"drift\"", D0)), "failed"),
        (reply(200, Value::Null), "unsupported"),
        (
            reply(401, json!({"secret":"provider-body-canary"})),
            "inconclusive",
        ),
    ] {
        let (result, _, records) = exercise(vec![reply]);
        assert!(result.is_err());
        let records = records.lock().unwrap();
        let stopped = records.records.last().unwrap();
        assert_eq!(stopped["event"], "stopped");
        assert_eq!(stopped["result"], expected);
        assert!(!stopped.to_string().contains("provider-body-canary"));
    }
}

#[test]
fn malformed_or_wrong_412_envelopes_stop_after_four_tasks_requests() {
    for body in [
        b"<html>provider-body-canary</html>".to_vec(),
        b"{".to_vec(),
        Vec::new(),
        serde_json::to_vec(&json!({})).unwrap(),
        serde_json::to_vec(&json!({"error":{"code":409,"message":"provider-body-canary"}}))
            .unwrap(),
        serde_json::to_vec(&json!({"error":{"code":"412"}})).unwrap(),
    ] {
        let mut replies = success_replies();
        replies.truncate(4);
        replies[3].body = body;
        let (result, requests, records) = exercise(replies);
        assert!(result.is_err());
        assert_eq!(requests.len(), 4);
        let encoded = serde_json::to_string(&records.lock().unwrap().records).unwrap();
        assert!(!encoded.contains("provider-body-canary"));
        assert!(!encoded.contains("matchingAcceptedAndStaleRejected"));
    }
}

fn controlled_clock(journal: &mut Journal) -> (Instant, Arc<Mutex<Instant>>) {
    let base = Instant::now();
    let clock = Arc::new(Mutex::new(base));
    let read = clock.clone();
    journal.set_clock(move || *read.lock().unwrap());
    (base, clock)
}

#[test]
fn attempted_sync_crossing_deadline_never_calls_operation() {
    let (mut journal, records) = memory(0);
    let (base, clock) = controlled_clock(&mut journal);
    records.lock().unwrap().on_append = Some(Box::new(move |record| {
        if record["event"] == "attempted" {
            *clock.lock().unwrap() = base + Duration::from_secs(121);
        }
    }));
    let result = journal.request("matchingPATCH", |_| -> Result<()> {
        panic!("expired operation must never start")
    });
    assert!(result.unwrap_err().to_string().starts_with("inconclusive:"));
    let records = records.lock().unwrap();
    assert_eq!(records.records.len(), 2);
    assert_eq!(records.records[1]["event"], "outcome");
    assert_eq!(records.records[1]["budgetExpired"], true);
}

#[test]
fn near_deadline_tasks_timeout_is_clamped_on_actual_loopback_request() {
    let t = temp();
    let (mut journal, records) = memory(0);
    let (base, clock) = controlled_clock(&mut journal);
    *clock.lock().unwrap() = base + Duration::from_millis(119_700);
    let mut response = reply(200, resource(E0, D0));
    response.delay = Duration::from_millis(700);
    let (http, server) = server(vec![response], Duration::from_secs(2));
    let result = execute(&run(path(&t)), &journal, &http, TOKEN);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .starts_with("inconclusive: Tasks transport")
    );
    assert_eq!(server.join().unwrap().len(), 1);
    assert_eq!(
        records.lock().unwrap().records.last().unwrap()["event"],
        "outcome"
    );
}

#[test]
fn late_final_response_retains_outcome_but_never_reports_success() {
    let t = temp();
    let (mut journal, records) = memory(0);
    let (base, clock) = controlled_clock(&mut journal);
    let mut replies = success_replies();
    replies[4].before_response = Some(Box::new(move || {
        *clock.lock().unwrap() = base + Duration::from_secs(121);
    }));
    let (http, server) = server(replies, Duration::from_secs(1));
    let result = finish(execute(&run(path(&t)), &journal, &http, TOKEN), &journal);
    assert!(result.unwrap_err().to_string().starts_with("inconclusive:"));
    assert_eq!(server.join().unwrap().len(), 5);
    let records = &records.lock().unwrap().records;
    let outcome = records
        .iter()
        .find(|r| r["event"] == "outcome" && r["stage"] == "finalGET")
        .unwrap();
    assert_eq!(outcome["result"], "responseReceived");
    assert_eq!(outcome["budgetExpired"], true);
    assert_eq!(records.last().unwrap()["result"], "inconclusive");
    assert!(!records.iter().any(|r| r["event"] == "resultCandidate"));
}

#[test]
fn late_outcome_sync_stops_before_any_followup_request() {
    let t = temp();
    let (mut journal, records) = memory(0);
    let (base, clock) = controlled_clock(&mut journal);
    records.lock().unwrap().on_append = Some(Box::new(move |record| {
        if record["event"] == "outcome" && record["stage"] == "matchingPATCH" {
            *clock.lock().unwrap() = base + Duration::from_secs(121);
        }
    }));
    let mut replies = success_replies();
    replies.truncate(2);
    let (http, server) = server(replies, Duration::from_secs(1));
    let result = finish(execute(&run(path(&t)), &journal, &http, TOKEN), &journal);
    assert!(result.unwrap_err().to_string().starts_with("inconclusive:"));
    assert_eq!(server.join().unwrap().len(), 2);
    let records = &records.lock().unwrap().records;
    assert!(records.iter().any(|r| r["event"] == "outcome"
        && r["stage"] == "matchingPATCH"
        && r["result"] == "responseReceived"));
    assert_eq!(records.last().unwrap()["result"], "inconclusive");
}

#[test]
fn final_result_sync_crossing_deadline_returns_inconclusive_not_success() {
    let t = temp();
    let (mut journal, records) = memory(0);
    let (base, clock) = controlled_clock(&mut journal);
    records.lock().unwrap().on_append = Some(Box::new(move |record| {
        if record["event"] == "resultCandidate" {
            *clock.lock().unwrap() = base + Duration::from_secs(121);
        }
    }));
    let (http, server) = server(success_replies(), Duration::from_secs(1));
    let result = finish(execute(&run(path(&t)), &journal, &http, TOKEN), &journal);
    assert!(result.unwrap_err().to_string().starts_with("inconclusive:"));
    assert_eq!(server.join().unwrap().len(), 5);
    let records = &records.lock().unwrap().records;
    assert_eq!(records[15]["event"], "resultCandidate");
    assert_eq!(records[15]["notPublication"], true);
    assert_eq!(records.last().unwrap()["result"], "inconclusive");
}

#[test]
fn oauth_wrapper_clamps_each_request_to_remaining_budget_and_30_second_cap() {
    for near_expiry in [false, true] {
        let (mut journal, records) = memory(0);
        let (base, clock) = controlled_clock(&mut journal);
        let remaining = if near_expiry {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(30)
        };
        if near_expiry {
            *clock.lock().unwrap() = base + Duration::from_secs(118);
        }
        let oauth = oauth();
        let budgets = Mutex::new(Vec::new());
        let factory = |timeout| {
            budgets.lock().unwrap().push(timeout);
            Ok(&oauth)
        };
        let logged = LoggedOAuth {
            factory: &factory,
            journal: &journal,
        };
        logged
            .refresh(
                &ClientConfig {
                    client_id: "fixture.apps.googleusercontent.com".into(),
                    client_secret: None,
                },
                &SecretString::new("refresh-canary").unwrap(),
            )
            .unwrap();
        if near_expiry {
            *clock.lock().unwrap() = base + Duration::from_secs(119);
        }
        logged.identity(&SecretString::new(TOKEN).unwrap()).unwrap();
        let budgets = budgets.lock().unwrap();
        assert!(budgets[0] <= remaining && budgets[0] > remaining - Duration::from_millis(50));
        assert!(
            budgets[1]
                <= if near_expiry {
                    Duration::from_secs(1)
                } else {
                    remaining
                }
        );
        assert_eq!(oauth.calls.load(SeqCst), 2);
        assert_eq!(records.lock().unwrap().records.len(), 4);
    }
}

#[test]
fn oauth_attempt_or_factory_crossing_deadline_never_calls_transport() {
    for expire_in_factory in [false, true] {
        let (mut journal, records) = memory(0);
        let (base, clock) = controlled_clock(&mut journal);
        let expire = clock.clone();
        records.lock().unwrap().on_append = Some(Box::new(move |record| {
            if !expire_in_factory && record["event"] == "attempted" {
                *expire.lock().unwrap() = base + Duration::from_secs(121);
            }
        }));
        let oauth = oauth();
        let factory = |_| {
            assert!(
                expire_in_factory,
                "must not construct transport after late attempt sync"
            );
            *clock.lock().unwrap() = base + Duration::from_secs(121);
            Ok(&oauth)
        };
        let logged = LoggedOAuth {
            factory: &factory,
            journal: &journal,
        };
        assert!(logged.identity(&SecretString::new(TOKEN).unwrap()).is_err());
        assert_eq!(oauth.calls.load(SeqCst), 0);
        assert_eq!(
            records.lock().unwrap().records.last().unwrap()["event"],
            "outcome"
        );
    }
}
