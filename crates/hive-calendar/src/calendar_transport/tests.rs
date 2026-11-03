use super::*;
use crate::{ExpectedTask, LedgerEntry, Lifecycle, PendingOperation, SourceAck};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

pub(super) const LOGICAL: &str = "11111111-1111-4111-8111-111111111111";
pub(super) const OPERATION: &str = "22222222-2222-4222-8222-222222222222";

// Keep deadline fixtures independent while Git fixture tests remain parallel.
// Lock once per test, not per server: redirects use simultaneous servers.
static HTTP_TEST_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn http_test_guard() -> std::sync::MutexGuard<'static, ()> {
    let guard = HTTP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Initialize the shared blocking runtime before any listener lifetime starts.
    // Constructing a transport makes no request to this unused loopback origin.
    HttpTransport::loopback("http://127.0.0.1:1/", Duration::from_secs(10)).unwrap();
    guard
}

pub(super) struct Reply {
    status: u16,
    body: Vec<u8>,
    headers: String,
    delay: Duration,
    chunked: bool,
    truncate: bool,
    trickle: bool,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self::raw(200, serde_json::to_vec(&value).unwrap())
    }
    pub(super) fn raw(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            body,
            headers: String::new(),
            delay: Duration::ZERO,
            chunked: false,
            truncate: false,
            trickle: false,
        }
    }
}

// Every listener binds literal loopback, accepts nonblocking, and has a stop guard.
// Stream I/O and reply sleeps are bounded even when an assertion unwinds.
pub(super) struct Server {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Server {
    pub(super) fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!(
            "http://127.0.0.1:{}/",
            listener.local_addr().unwrap().port()
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = requests.clone();
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            let mut replies = replies.into_iter();
            let end = Instant::now() + Duration::from_secs(30);
            while !stopping.load(Ordering::Relaxed) && Instant::now() < end {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let mut complete = false;
                while !stopping.load(Ordering::Relaxed) && Instant::now() < end {
                    let n = match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(pos) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..pos]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (k, v) = line.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= pos + 4 + length {
                            complete = true;
                            break;
                        }
                    }
                    if bytes.len() > 2 * EVENT_LIMIT {
                        break;
                    }
                }
                if !complete {
                    continue;
                }
                seen.lock().unwrap().push(String::from_utf8(bytes).unwrap());
                let Some(reply) = replies.next() else {
                    continue;
                };
                thread::sleep(reply.delay.min(Duration::from_secs(1)));
                let framing = if reply.chunked {
                    "Transfer-Encoding: chunked\r\n".into()
                } else {
                    format!(
                        "Content-Length: {}\r\n",
                        reply.body.len() + usize::from(reply.truncate) * 20
                    )
                };
                let header = format!(
                    "HTTP/1.1 {} Fixture\r\nConnection: close\r\n{}{}\r\n",
                    reply.status, framing, reply.headers
                );
                if stream.write_all(header.as_bytes()).is_err() {
                    continue;
                }
                for chunk in reply.body.chunks(if reply.trickle { 1 } else { 4096 }) {
                    if reply.trickle {
                        thread::sleep(Duration::from_millis(20));
                    }
                    if stopping.load(Ordering::Relaxed) {
                        break;
                    }
                    if reply.chunked && write!(stream, "{:x}\r\n", chunk.len()).is_err() {
                        break;
                    }
                    if stream.write_all(chunk).is_err() {
                        break;
                    }
                    if reply.chunked && stream.write_all(b"\r\n").is_err() {
                        break;
                    }
                }
                if reply.chunked {
                    let _ = stream.write_all(b"0\r\n\r\n");
                }
            }
        });
        Self {
            base,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    pub(super) fn transport(&self) -> HttpTransport {
        HttpTransport::loopback(&self.base, Duration::from_secs(10)).unwrap()
    }
    pub(super) fn seen(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

pub(super) fn token() -> SecretToken {
    SecretToken::new("fixture-secret-token".into()).unwrap()
}
pub(super) fn principal() -> Reply {
    Reply::json(json!({"sub":"fixture-sub", "email":"not-authority@example.test"}))
}

pub(super) fn fixture(update: bool) -> Ledger {
    let binding = Binding {
        provider: "google_calendar".into(),
        account_id: hive_core::GoogleIdentity::new("fixture-sub")
            .unwrap()
            .namespace()
            .unwrap(),
        calendar_id: "cal/with?query#fragment@example.test".into(),
        source_repo_id: "fixture".into(),
    };
    let source = SourceAck {
        commit: "a".repeat(40),
        parser_sha256: "b".repeat(64),
        path: "fixture.md".into(),
        source_id: "fixture".into(),
        line: 1,
        mw_id: "fixture:1:0".into(),
        git_blob_id: "c".repeat(40),
        source_revision: format!("sha256:{}", "d".repeat(64)),
        stable_id: None,
        expected: ExpectedTask {
            text: "Private title".into(),
            literal_text: "- [ ] Private title due:2026-11-01".into(),
            done: false,
            due: "2026-11-01".into(),
            raw: String::new(),
            start: String::new(),
            estimate: String::new(),
        },
    };
    let mut pending = PendingOperation {
        operation_id: OPERATION.into(),
        kind: if update {
            OperationKind::Update
        } else {
            OperationKind::Create
        },
        phase: OperationPhase::Prepared,
        intended_source: source.clone(),
        target_event_id: operations::reserved_event_id(&binding, LOGICAL).unwrap(),
        base_etag: update.then(|| "\"old\"".into()),
        desired_payload_hash: Some(String::new()),
    };
    // frozen_event requires its hash; compute the same frozen projection independently.
    let event = ProspectiveEvent {
        summary: "Private title".into(),
        start: CalendarDate {
            date: "2026-11-01".into(),
        },
        end: CalendarDate {
            date: "2026-11-02".into(),
        },
        availability: "",
        transparency: "transparent",
    };
    pending.desired_payload_hash = Some(ledger::payload_hash(&event));
    let mut prior_event = event;
    prior_event.summary = "Prior title".into();
    Ledger {
        schema_version: "1".into(),
        binding,
        source_watermark: "a".repeat(40),
        entries: vec![LedgerEntry {
            logical_id: LOGICAL.into(),
            lifecycle: if update {
                Lifecycle::Active
            } else {
                Lifecycle::Enrolled
            },
            source_ack: source,
            provider_ack: update.then(|| ProviderAck {
                event_id: pending.target_event_id.clone(),
                etag: "\"old\"".into(),
                last_payload_hash: ledger::payload_hash(&prior_event),
            }),
            pending: Some(pending),
            tombstone: None,
        }],
    }
}
fn intent(update: bool) -> PreparedFrozenIntent {
    let ledger = fixture(update);
    PreparedFrozenIntent::prepare(&ledger, &ledger.binding, LOGICAL, OPERATION).unwrap()
}

#[test]
fn actual_loopback_success_is_not_durable_ack_authority_even_when_pinned() {
    let _guard = http_test_guard();
    for update in [false, true] {
        for phase in [OperationPhase::Prepared, OperationPhase::Uncertain] {
            let mut old = fixture(update);
            old.entries[0].pending.as_mut().unwrap().phase = phase;
            let pinned = ack::fixture_context(old.clone());
            let mut prepared =
                PreparedFrozenIntent::prepare(&old, &old.binding, LOGICAL, OPERATION).unwrap();
            pinned.attach(&mut prepared).unwrap();
            let server = Server::new(vec![principal(), Reply::json(event(&prepared, false))]);
            let success = server.transport().reconcile(&token(), &prepared).unwrap();
            assert_eq!(success.context_digest, prepared.context_digest);
            assert!(!success.production_origin_eligible());
            assert!(pinned.admit(success).is_err());
            assert_eq!(server.seen().len(), 2);
        }
    }
}
fn event(intent: &PreparedFrozenIntent, prior: bool) -> Value {
    let mut body = managed_body(intent, ownership(intent));
    body["id"] = json!(intent.event_id);
    body["status"] = json!("confirmed");
    body["etag"] = json!(if prior { "\"old\"" } else { "\"new\"" });
    if prior {
        body["summary"] = json!("Prior title");
    }
    body
}
fn body(request: &str) -> Value {
    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
}

#[test]
fn create_and_reconcile_bind_identity_paths_payload_and_origin() {
    let _guard = http_test_guard();
    let intent = intent(false);
    let server = Server::new(vec![
        principal(),
        Reply::json(event(&intent, false)),
        principal(),
        Reply::json(event(&intent, false)),
    ]);
    let transport = server.transport();
    let success = transport
        .create(
            &token(),
            &intent,
            DispatchPermit::test_permit(&intent, &transport),
        )
        .unwrap();
    assert!(!success.production_origin_eligible());
    assert!(success.cause == SuccessCause::MutationResponse);
    assert!(success.binding == intent.binding);
    assert_eq!(success.logical_id, LOGICAL);
    assert_eq!(success.operation_id, OPERATION);
    assert_eq!(success.intent_digest, intent.digest);
    assert_eq!(success.event_id, intent.event_id);
    assert_eq!(success.etag, "\"new\"");
    assert_eq!(success.payload_hash, intent.desired_hash);
    let observation = transport.reconcile(&token(), &intent).unwrap();
    assert!(observation.cause == SuccessCause::ReconciledObservation);
    assert!(!observation.production_origin_eligible());
    let requests = server.seen();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET /userinfo HTTP/1.1\r\n"));
    let path = "/calendar/v3/calendars/cal%2Fwith%3Fquery%23fragment@example.test/events";
    assert!(requests[1].starts_with(&format!("POST {path} HTTP/1.1\r\n")));
    assert!(requests[3].starts_with(&format!("GET {path}/{} HTTP/1.1\r\n", intent.event_id)));
    for request in &requests {
        assert!(request.contains("authorization: Bearer fixture-secret-token\r\n"));
    }
    assert!(requests[0].ends_with("\r\n\r\n"));
    let sent = body(&requests[1]);
    assert_eq!(sent.as_object().unwrap().len(), 6);
    assert_eq!(sent["id"], intent.event_id);
    assert_eq!(sent["start"], json!({"date":"2026-11-01"}));
    assert_eq!(sent["end"], json!({"date":"2026-11-02"}));
    assert_eq!(sent["summary"], "Private title");
    assert_eq!(sent["transparency"], "transparent");
    assert_eq!(
        sent["extendedProperties"]["private"],
        json!(ownership(&intent))
    );
}

#[test]
fn update_preflights_prior_hash_etag_and_preserves_private_only() {
    let _guard = http_test_guard();
    let intent = intent(true);
    let mut prior = event(&intent, true);
    prior["extendedProperties"]["private"]["unrelated"] = json!("keep");
    prior["extendedProperties"]["shared"] = json!({"shared":"untouched"});
    prior["description"] = json!("unmanaged");
    prior["start"]["timeZone"] = json!("America/New_York");
    let server = Server::new(vec![
        principal(),
        Reply::json(prior),
        Reply::json(event(&intent, false)),
    ]);
    let transport = server.transport();
    assert!(
        transport
            .update(
                &token(),
                &intent,
                DispatchPermit::test_permit(&intent, &transport)
            )
            .is_ok()
    );
    let requests = server.seen();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].starts_with("GET /calendar/"));
    assert!(requests[2].starts_with("PATCH /calendar/"));
    assert!(requests[2].contains("if-match: \"old\"\r\n"));
    let sent = body(&requests[2]);
    assert_eq!(sent.as_object().unwrap().len(), 5);
    assert!(sent.get("id").is_none());
    assert_eq!(sent["extendedProperties"]["private"]["unrelated"], "keep");
    assert_eq!(sent["extendedProperties"].as_object().unwrap().len(), 1);
    assert_eq!(sent["start"], json!({"date":"2026-11-01"}));
}

#[test]
fn principal_mismatch_and_invalid_userinfo_never_contact_calendar() {
    let _guard = http_test_guard();
    for reply in [
        Reply::json(json!({"sub":"wrong"})),
        Reply::json(json!({"email":"fixture-sub"})),
        Reply::json(json!({"sub":17})),
        Reply::json(json!({"sub":""})),
        Reply::raw(
            200,
            br#"{"sub":"fixture-sub","extra":{"a":1,"a":2}}"#.to_vec(),
        ),
        Reply::raw(200, br#"{"sub":"wrong","sub":"fixture-sub"}"#.to_vec()),
    ] {
        let server = Server::new(vec![reply]);
        let transport = server.transport();
        assert!(transport.reconcile(&token(), &intent(false)).is_err());
        assert_eq!(server.seen().len(), 1);
    }
    let server = Server::new(vec![Reply::json(json!({"sub":"wrong"}))]);
    assert_eq!(
        server.transport().reconcile(&token(), &intent(false)).err(),
        Some(TransportError::PrincipalMismatch)
    );
}

#[test]
fn invalid_permit_phase_and_origin_make_zero_requests() {
    let _guard = http_test_guard();
    let intent = intent(false);
    let server = Server::new(vec![]);
    let transport = server.transport();
    for field in 0..7 {
        let mut permit = DispatchPermit::test_permit(&intent, &transport);
        match field {
            0 => permit.binding.source_repo_id.push('x'),
            1 => permit.logical_id.push('x'),
            2 => permit.operation_id.push('x'),
            3 => permit.digest.push('x'),
            4 => permit.kind = OperationKind::Update,
            5 => permit.origin = ImmutableEndpointOrigin::Google,
            _ => permit.binding.calendar_id.push('x'),
        }
        assert_eq!(
            transport.create(&token(), &intent, permit).err(),
            Some(TransportError::Invalid)
        );
    }
    let mut intent = intent;
    intent.phase = OperationPhase::Uncertain;
    assert_eq!(
        transport
            .create(
                &token(),
                &intent,
                DispatchPermit::test_permit(&intent, &transport)
            )
            .err(),
        Some(TransportError::Invalid)
    );
    assert!(server.seen().is_empty());
}

#[test]
fn frozen_intent_rejects_malformed_ledger_delete_wrong_selection_and_old_etag() {
    let _guard = http_test_guard();
    for variant in 0..9 {
        let mut ledger = fixture(variant >= 4);
        let pending = ledger.entries[0].pending.as_mut().unwrap();
        match variant {
            0 => pending.target_event_id = "other0".into(),
            1 => pending.desired_payload_hash = Some(format!("sha256:{}", "f".repeat(64))),
            2 => ledger.binding.calendar_id = ".".into(),
            3 => ledger.binding.calendar_id = "..".into(),
            4 => {
                pending.kind = OperationKind::Delete;
                pending.desired_payload_hash = None;
            }
            5 => {
                pending.base_etag = Some("unquoted".into());
                ledger.entries[0].provider_ack.as_mut().unwrap().etag = "unquoted".into();
            }
            6 => ledger.entries[0].lifecycle = Lifecycle::Enrolled,
            7 => pending.intended_source.expected.done = true,
            _ => pending.operation_id = LOGICAL.into(),
        }
        assert!(
            PreparedFrozenIntent::prepare(&ledger, &ledger.binding, LOGICAL, OPERATION).is_err()
        );
    }
    let ledger = fixture(false);
    assert!(PreparedFrozenIntent::prepare(&ledger, &ledger.binding, OPERATION, OPERATION).is_err());
    let mut other = ledger.binding.clone();
    other.source_repo_id.push('x');
    assert!(PreparedFrozenIntent::prepare(&ledger, &other, LOGICAL, OPERATION).is_err());
}

fn bad_events(intent: &PreparedFrozenIntent, prior: bool) -> Vec<Value> {
    let base = event(intent, prior);
    let mut values = Vec::new();
    for (pointer, bad) in [
        ("/id", json!("other0")),
        ("/status", json!("cancelled")),
        ("/status", json!(null)),
        ("/etag", json!("W/\"weak\"")),
        ("/etag", json!("unquoted")),
        ("/start/date", json!("2026-02-30")),
        ("/start/date", json!("0000-11-01")),
        ("/end/date", json!("0000-11-02")),
        ("/end/date", json!("2026-11-01")),
        ("/summary", json!("changed")),
        ("/transparency", json!("opaque")),
        ("/extendedProperties/private/hiveOwner", json!("other")),
        ("/extendedProperties/private/hiveSchema", json!(2)),
        (
            "/extendedProperties/private/hiveLogicalId",
            json!(OPERATION),
        ),
    ] {
        let mut value = base.clone();
        *value.pointer_mut(pointer).unwrap() = bad;
        values.push(value);
    }
    for (key, bad) in [
        ("recurrence", json!([])),
        ("recurringEventId", json!(null)),
        ("originalStartTime", json!({})),
        ("eventType", json!("outOfOffice")),
        ("eventType", json!(null)),
    ] {
        let mut value = base.clone();
        value[key] = bad;
        values.push(value);
    }
    for key in [
        "id",
        "status",
        "etag",
        "summary",
        "start",
        "end",
        "transparency",
        "extendedProperties",
    ] {
        let mut value = base.clone();
        value.as_object_mut().unwrap().remove(key);
        values.push(value);
    }
    for key in ["start", "end"] {
        let mut value = base.clone();
        value[key]["dateTime"] = json!(null);
        values.push(value);
        let mut value = base.clone();
        value[key] = json!({"dateTime":"2026-11-01T00:00:00Z"});
        values.push(value);
    }
    values
}

#[test]
fn invalid_remote_classes_never_patch_and_malformed_mutation_is_uncertain() {
    let _guard = http_test_guard();
    let intent = intent(true);
    let mut invalid = bad_events(&intent, true);
    let mut changed_etag = event(&intent, true);
    changed_etag["etag"] = json!("\"changed\"");
    invalid.push(changed_etag);
    // Desired payload cannot substitute for the acknowledged prior payload.
    invalid.push(event(&intent, false));
    for value in invalid {
        let server = Server::new(vec![principal(), Reply::json(value)]);
        let transport = server.transport();
        assert!(
            transport
                .update(
                    &token(),
                    &intent,
                    DispatchPermit::test_permit(&intent, &transport)
                )
                .is_err()
        );
        assert_eq!(server.seen().len(), 2);
    }
    let create_intent = self::intent(false);
    for value in bad_events(&create_intent, false) {
        let server = Server::new(vec![principal(), Reply::json(value)]);
        let transport = server.transport();
        assert_eq!(
            transport
                .create(
                    &token(),
                    &create_intent,
                    DispatchPermit::test_permit(&create_intent, &transport)
                )
                .err(),
            Some(TransportError::Uncertain)
        );
        assert_eq!(server.seen().len(), 2);
    }
}

#[test]
fn duplicate_keys_at_all_depths_are_rejected_over_http() {
    let _guard = http_test_guard();
    let intent = intent(false);
    let original = serde_json::to_string(&event(&intent, false)).unwrap();
    for key in ["id", "date", "hiveOwner"] {
        let needle = format!("\"{key}\":");
        let duplicate = original.replacen(&needle, &format!("\"{key}\":null,{needle}"), 1);
        let server = Server::new(vec![principal(), Reply::raw(200, duplicate.into_bytes())]);
        assert_eq!(
            server.transport().reconcile(&token(), &intent).err(),
            Some(TransportError::Invalid)
        );
    }
    let mut extra = event(&intent, false);
    extra["unknown"] = json!({"compatible":true});
    extra["eventType"] = json!("default");
    extra["status"] = json!("tentative");
    let server = Server::new(vec![principal(), Reply::json(extra)]);
    assert!(server.transport().reconcile(&token(), &intent).is_ok());
}

#[test]
fn statuses_are_static_and_mutations_are_never_retried() {
    let _guard = http_test_guard();
    let intent = intent(false);
    for (status, expected) in [
        (401, TransportError::Authorization),
        (403, TransportError::Authorization),
        (404, TransportError::Missing),
        (410, TransportError::Missing),
        (409, TransportError::Collision),
        (412, TransportError::Conflict),
        (429, TransportError::Unavailable),
        (500, TransportError::Unavailable),
        (503, TransportError::Unavailable),
    ] {
        let server = Server::new(vec![
            principal(),
            Reply::raw(status, b"private error body".to_vec()),
        ]);
        let transport = server.transport();
        assert_eq!(
            transport
                .create(
                    &token(),
                    &intent,
                    DispatchPermit::test_permit(&intent, &transport)
                )
                .err(),
            Some(if status >= 500 {
                TransportError::Uncertain
            } else {
                expected
            })
        );
        assert_eq!(server.seen().len(), 2);
        let server = Server::new(vec![principal(), Reply::raw(status, Vec::new())]);
        assert_eq!(
            server.transport().reconcile(&token(), &intent).err(),
            Some(expected)
        );
        assert_eq!(server.seen().len(), 2);
    }
    for status in [404, 410, 412] {
        let intent = self::intent(true);
        let server = Server::new(vec![principal(), Reply::raw(status, Vec::new())]);
        let transport = server.transport();
        assert!(
            transport
                .update(
                    &token(),
                    &intent,
                    DispatchPermit::test_permit(&intent, &transport)
                )
                .is_err()
        );
        assert_eq!(server.seen().len(), 2);
    }
}

#[test]
fn server_failures_are_uncertain_only_for_attempted_mutations() {
    let _guard = http_test_guard();
    for status in [500, 502, 503, 504] {
        for update in [false, true] {
            let intent = intent(update);
            let mut replies = vec![principal()];
            if update {
                replies.push(Reply::json(event(&intent, true)));
            }
            replies.push(Reply::raw(status, b"private server failure".to_vec()));
            let server = Server::new(replies);
            let transport = server.transport();
            let permit = DispatchPermit::test_permit(&intent, &transport);
            let result = if update {
                transport.update(&token(), &intent, permit)
            } else {
                transport.create(&token(), &intent, permit)
            };
            assert_eq!(result.err(), Some(TransportError::Uncertain));
            let requests = server.seen();
            assert_eq!(requests.len(), if update { 3 } else { 2 });
            let method = if update { "PATCH " } else { "POST " };
            assert_eq!(requests.iter().filter(|r| r.starts_with(method)).count(), 1);
            assert!(requests.last().unwrap().starts_with(method));

            // Prepared metadata cannot turn a failed principal read into a write.
            let server = Server::new(vec![Reply::raw(status, Vec::new())]);
            let transport = server.transport();
            let permit = DispatchPermit::test_permit(&intent, &transport);
            let result = if update {
                transport.update(&token(), &intent, permit)
            } else {
                transport.create(&token(), &intent, permit)
            };
            assert_eq!(result.err(), Some(TransportError::Unavailable));
            let requests = server.seen();
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with("GET /userinfo "));
        }

        let intent = intent(true);
        let server = Server::new(vec![principal(), Reply::raw(status, Vec::new())]);
        let transport = server.transport();
        assert_eq!(
            transport
                .update(
                    &token(),
                    &intent,
                    DispatchPermit::test_permit(&intent, &transport)
                )
                .err(),
            Some(TransportError::Unavailable)
        );
        let requests = server.seen();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /userinfo "));
        assert!(requests[1].starts_with("GET /calendar/"));

        let server = Server::new(vec![principal(), Reply::raw(status, Vec::new())]);
        assert_eq!(
            server.transport().reconcile(&token(), &intent).err(),
            Some(TransportError::Unavailable)
        );
        let requests = server.seen();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| r.starts_with("GET ")));
    }
}

#[test]
fn year_zero_is_invalid_even_when_prior_ack_hash_matches() {
    let _guard = http_test_guard();
    let mut ledger = fixture(true);
    let payload = ProspectiveEvent {
        summary: "Prior title".into(),
        start: CalendarDate {
            date: "0000-11-01".into(),
        },
        end: CalendarDate {
            date: "0000-11-02".into(),
        },
        availability: "",
        transparency: "transparent",
    };
    ledger.entries[0]
        .provider_ack
        .as_mut()
        .unwrap()
        .last_payload_hash = ledger::payload_hash(&payload);
    let intent =
        PreparedFrozenIntent::prepare(&ledger, &ledger.binding, LOGICAL, OPERATION).unwrap();
    let mut prior = event(&intent, true);
    prior["start"]["date"] = json!(payload.start.date);
    prior["end"]["date"] = json!(payload.end.date);
    let server = Server::new(vec![principal(), Reply::json(prior)]);
    let transport = server.transport();
    assert_eq!(
        transport
            .update(
                &token(),
                &intent,
                DispatchPermit::test_permit(&intent, &transport)
            )
            .err(),
        Some(TransportError::Invalid)
    );
    let requests = server.seen();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.starts_with("GET ")));
}

#[test]
fn patch_failure_is_uncertain_only_after_dispatch_and_never_retried() {
    let _guard = http_test_guard();
    let intent = intent(true);
    for status in [200, 412, 503] {
        let reply = if status == 200 {
            Reply::raw(200, b"{malformed private payload".to_vec())
        } else {
            Reply::raw(status, Vec::new())
        };
        let server = Server::new(vec![principal(), Reply::json(event(&intent, true)), reply]);
        let transport = server.transport();
        let expected = match status {
            412 => TransportError::Conflict,
            _ => TransportError::Uncertain,
        };
        assert_eq!(
            transport
                .update(
                    &token(),
                    &intent,
                    DispatchPermit::test_permit(&intent, &transport)
                )
                .err(),
            Some(expected)
        );
        assert_eq!(server.seen().len(), 3);
    }
    // Closing a mutation connection without a reply cannot prove non-application.
    let server = Server::new(vec![principal(), Reply::json(event(&intent, true))]);
    let transport = server.transport();
    assert_eq!(
        transport
            .update(
                &token(),
                &intent,
                DispatchPermit::test_permit(&intent, &transport)
            )
            .err(),
        Some(TransportError::Uncertain)
    );
    assert_eq!(server.seen().len(), 3);
    let mut truncated = Reply::json(event(&intent, true));
    truncated.truncate = true;
    let server = Server::new(vec![principal(), truncated]);
    let transport = server.transport();
    assert_eq!(
        transport
            .update(
                &token(),
                &intent,
                DispatchPermit::test_permit(&intent, &transport)
            )
            .err(),
        Some(TransportError::Unavailable)
    );
    assert_eq!(server.seen().len(), 2);
    for error in [
        TransportError::Authorization,
        TransportError::PrincipalMismatch,
        TransportError::Missing,
        TransportError::Collision,
        TransportError::Conflict,
        TransportError::Unavailable,
        TransportError::Invalid,
        TransportError::Uncertain,
    ] {
        let debug = format!("{error:?}");
        assert!(!debug.contains("private"));
        assert!(!debug.contains("http"));
        assert!(!debug.contains("fixture"));
    }
}

#[test]
fn redirects_never_contact_secondary_server() {
    let _guard = http_test_guard();
    let secondary = Server::new(vec![]);
    for userinfo in [true, false] {
        let mut redirect = Reply::raw(302, Vec::new());
        redirect.headers = format!("Location: {}steal\r\n", secondary.base);
        let replies = if userinfo {
            vec![redirect]
        } else {
            vec![principal(), redirect]
        };
        let server = Server::new(replies);
        assert_eq!(
            server.transport().reconcile(&token(), &intent(false)).err(),
            Some(TransportError::Invalid)
        );
        assert_eq!(server.seen().len(), if userinfo { 1 } else { 2 });
    }
    assert!(secondary.seen().is_empty());
}

#[test]
fn bounded_chunked_oversized_truncated_and_trickled_responses() {
    let _guard = http_test_guard();
    let intent = intent(false);
    let mut chunked = Reply::json(event(&intent, false));
    chunked.chunked = true;
    let server = Server::new(vec![principal(), chunked]);
    assert!(server.transport().reconcile(&token(), &intent).is_ok());
    for mutation in [false, true] {
        for kind in 0..4 {
            let mut reply = Reply::json(event(&intent, false));
            match kind {
                0 => reply.truncate = true,
                1 => reply.body = vec![b' '; EVENT_LIMIT + 1],
                2 => {
                    reply.body = vec![b' '; EVENT_LIMIT + 1];
                    reply.chunked = true;
                }
                _ => reply.trickle = true,
            }
            let server = Server::new(vec![principal(), reply]);
            let transport = HttpTransport::loopback(&server.base, Duration::from_secs(1)).unwrap();
            let started = Instant::now();
            let result = if mutation {
                transport.create(
                    &token(),
                    &intent,
                    DispatchPermit::test_permit(&intent, &transport),
                )
            } else {
                transport.reconcile(&token(), &intent)
            };
            assert!(result.is_err());
            if mutation {
                assert_eq!(result.err(), Some(TransportError::Uncertain));
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            assert_eq!(server.seen().len(), 2);
        }
    }
    for chunked in [false, true] {
        let mut reply = Reply::raw(200, vec![b' '; USERINFO_LIMIT + 1]);
        reply.chunked = chunked;
        let server = Server::new(vec![reply]);
        assert!(server.transport().reconcile(&token(), &intent).is_err());
        assert_eq!(server.seen().len(), 1);
    }
}

#[test]
fn total_deadline_spans_principal_preflight_and_mutation() {
    let _guard = http_test_guard();
    let intent = intent(true);
    let mut info = principal();
    info.delay = Duration::from_millis(450);
    let mut preflight = Reply::json(event(&intent, true));
    preflight.delay = Duration::from_millis(450);
    let mut mutation = Reply::json(event(&intent, false));
    // This reply fits a reset 1500ms request budget, but not the shared
    // remainder after the two preceding 450ms reads.
    mutation.delay = Duration::from_millis(900);
    let server = Server::new(vec![info, preflight, mutation]);
    let transport = HttpTransport::loopback(&server.base, Duration::from_millis(1500)).unwrap();
    let start = Instant::now();
    let result = transport.update(
        &token(),
        &intent,
        DispatchPermit::test_permit(&intent, &transport),
    );
    assert!(start.elapsed() < Duration::from_secs(4));
    let requests = server.seen();
    assert!(requests.len() <= 3);
    if requests.len() == 3 {
        assert!(requests[2].starts_with("PATCH "));
        assert_eq!(
            requests.iter().filter(|r| r.starts_with("PATCH ")).count(),
            1
        );
        assert_eq!(result.err(), Some(TransportError::Uncertain));
    } else {
        // Under load, the shared deadline can expire before dispatch. That is
        // unavailable, not evidence of an uncertain mutation that never started.
        assert!(requests.iter().all(|r| r.starts_with("GET ")));
        assert_eq!(result.err(), Some(TransportError::Unavailable));
    }
    let server = Server::new(vec![]);
    let base = server.base.clone();
    drop(server);
    let transport = HttpTransport::loopback(&base, Duration::from_millis(100)).unwrap();
    assert_eq!(
        transport.reconcile(&token(), &intent).err(),
        Some(TransportError::Unavailable)
    );
}

#[test]
fn secret_etag_endpoints_and_builder_policy() {
    let _guard = http_test_guard();
    assert_eq!(format!("{:?}", token()), "SecretToken([REDACTED])");
    for bad in ["", " ", "a\r\nb", "a b", "a\tb", "\u{e9}", "=", "a=b"] {
        assert_eq!(
            SecretToken::new(bad.into()).err(),
            Some(TransportError::Invalid)
        );
    }
    assert!(SecretToken::new("x".repeat(8193)).is_err());
    assert!(SecretToken::new("a.b_c-0+/==".into()).is_ok());
    for bad in [
        "",
        "\"\"",
        "plain",
        "W/\"weak\"",
        "\"a\"b\"",
        "\"a\rb\"",
        "\"a b\"",
        "\"\u{e9}\"",
    ] {
        assert!(strong_etag(bad).is_err());
    }
    assert!(strong_etag(&format!("\"{}\"", "a".repeat(1022))).is_ok());
    assert!(strong_etag(&format!("\"{}\"", "a".repeat(1023))).is_err());
    for bad in [
        "http://localhost:1234/",
        "https://127.0.0.1:1234/",
        "http://127.0.0.1/",
        "http://127.0.0.1:1234/path",
        "http://user@127.0.0.1:1234/",
        "http://127.0.0.1:1234/?x",
        "http://127.0.0.1:1234/#x",
        "http://2130706433:1234/",
        "http://127.1:1234/",
        "http://127.0.0.1:0/",
    ] {
        assert!(HttpTransport::loopback(bad, Duration::from_secs(1)).is_err());
    }
    // No process-wide environment mutation: verify the actual builder source.
    let source = include_str!("../calendar_transport.rs");
    let builder = source
        .split("fn client(https_only: bool)")
        .nth(1)
        .unwrap()
        .split("#[cfg(test)]")
        .next()
        .unwrap();
    assert!(builder.contains(".no_proxy()"));
    assert!(builder.contains(".https_only(https_only)"));
    assert!(builder.contains("Policy::none()"));
    assert!(builder.contains("reqwest::retry::never()"));
    let production = source
        .split("fn production()")
        .nth(1)
        .unwrap()
        .split("fn client(")
        .next()
        .unwrap();
    assert!(production.contains("Self::client(true)"));
    let loopback = source
        .split("#[cfg(test)]\n    fn loopback(")
        .nth(1)
        .unwrap()
        .split("fn event_url(")
        .next()
        .unwrap();
    assert!(loopback.contains("Self::client(false)"));
    let server = Server::new(vec![]);
    assert!(
        HttpTransport::client(true)
            .unwrap()
            .get(&server.base)
            .send()
            .is_err()
    );
    assert!(server.seen().is_empty());
    assert_eq!(USERINFO, "https://openidconnect.googleapis.com/v1/userinfo");
    assert_eq!(CALENDAR, "https://www.googleapis.com/calendar/v3/");
}
