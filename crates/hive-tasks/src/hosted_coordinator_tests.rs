use super::*;
use crate::hosted::tests::{ACCESS, Fixture, OAuth, wire};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    thread,
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(120)
}
fn request(stream: &mut TcpStream) -> Option<String> {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut b = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        match stream.read(&mut b) {
            Ok(1) => {}
            Ok(0) if bytes.is_empty() => return None,
            Err(e)
                if bytes.is_empty()
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
            {
                return None;
            }
            other => panic!("incomplete HTTP request: {other:?}"),
        }
        bytes.push(b[0]);
        assert!(bytes.len() < 8192);
    }
    let headers = String::from_utf8(bytes).unwrap();
    let size = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(|s| s.parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(size < 1024);
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    Some(format!("{headers}{}", String::from_utf8(body).unwrap()))
}
fn reply(stream: &mut TcpStream, status: u16, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}
// Servers are nonblocking and join on drop even when the coordinator stops early.
struct Server {
    base: String,
    stop: Arc<AtomicUsize>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(String, &mut TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicUsize::new(0));
        let flag = stop.clone();
        let handler = Arc::new(handler);
        let worker = thread::spawn(move || {
            let mut connections = Vec::new();
            while flag.load(SeqCst) == 0 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let handler = handler.clone();
                        connections.push(thread::spawn(move || {
                            if let Some(raw) = request(&mut stream) {
                                handler(raw, &mut stream);
                            }
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            for connection in connections {
                connection.join().unwrap();
            }
        });
        Self {
            base,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(1, SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Lab {
    f: Fixture,
    b: Binding,
    a: AdmittedPush,
    coordinator: Coordinator,
    google: Server,
    github: Server,
    patches: Arc<AtomicUsize>,
    current_main: Arc<Mutex<String>>,
    task: Arc<Mutex<Value>>,
    google_mode: Arc<AtomicUsize>,
}
impl Lab {
    fn new() -> Self {
        let f = Fixture::new();
        let b = f.binding();
        let a = b.admit(f.claims(), &f.commit).unwrap();
        let current_main = Arc::new(Mutex::new(f.commit.clone()));
        let main = current_main.clone();
        let github = Server::new(move |raw, stream| {
            let lower = raw.to_ascii_lowercase();
            assert!(lower.contains("authorization: bearer github-canary\r\n"));
            assert!(lower.contains("x-github-api-version: 2022-11-28\r\n"));
            assert!(lower.contains("user-agent: hive-tasks/1\r\n"));
            let value = if raw.starts_with("GET /repos/owner/repo HTTP/") {
                json!({"id":123,"full_name":"owner/repo","private":true})
            } else {
                assert!(raw.starts_with("GET /repos/owner/repo/git/ref/heads/main HTTP/"));
                json!({"ref":"refs/heads/main","object":{"type":"commit","sha":*main.lock().unwrap()}})
            };
            reply(stream, 200, &value.to_string());
        });
        let mut task = serde_json::to_value(&b.reviewed_task_handoff.baseline).unwrap();
        task["etag"] = json!("\"before\"");
        let task = Arc::new(Mutex::new(task));
        let resource = task.clone();
        let patches = Arc::new(AtomicUsize::new(0));
        let counter = patches.clone();
        let google_mode = Arc::new(AtomicUsize::new(0));
        let mode = google_mode.clone();
        let google = Server::new(move |raw, stream| {
            assert!(
                raw.to_ascii_lowercase()
                    .contains(&format!("authorization: bearer {ACCESS}\r\n"))
            );
            let mut task = resource.lock().unwrap();
            if raw.starts_with("PATCH ") {
                assert!(raw.starts_with("PATCH /lists/list%2Fone+/tasks/task%2Fone+ HTTP/1.1"));
                assert!(
                    raw.to_ascii_lowercase()
                        .contains("if-match: \"before\"\r\n")
                );
                assert_eq!(
                    raw.split_once("\r\n\r\n").unwrap().1,
                    "{\"due\":\"2026-11-10T00:00:00.000Z\"}"
                );
                counter.fetch_add(1, SeqCst);
                task["due"] = json!("2026-11-10T00:00:00.000Z");
                task["etag"] = json!("\"after\"");
                if mode.load(SeqCst) == 1 {
                    return;
                }
            } else {
                assert!(raw.starts_with("GET /lists/list%2Fone+/tasks/task%2Fone+ HTTP/1.1"));
                match mode.load(SeqCst) {
                    2 => {
                        reply(stream, 200, "malformed-provider-token-canary");
                        return;
                    }
                    3 => {
                        reply(stream, 302, "provider-token-canary");
                        return;
                    }
                    4 => {
                        reply(stream, 500, "provider-token-canary");
                        return;
                    }
                    _ => {}
                }
            }
            reply(stream, 200, &task.to_string());
        });
        let remote = f.root.join("remote.git");
        fs::create_dir(&remote).unwrap();
        storage::git(&remote, &["init", "--bare", "--template=", "."], b"").unwrap();
        let coordinator = Coordinator::initialize(
            &remote,
            &b,
            github::GitHub::loopback(&github.base).unwrap(),
            Http::loopback(&format!("{}lists/", google.base)),
        )
        .unwrap();
        Self {
            f,
            b,
            a,
            coordinator,
            google,
            github,
            patches,
            current_main,
            task,
            google_mode,
        }
    }
    fn store(&self) -> ImmutableTasksWriteStore {
        ImmutableTasksWriteStore::from_record_bytes(
            "work",
            &self.b.expected_account_namespace,
            wire().to_string().as_bytes(),
        )
        .unwrap()
    }
    fn prepare(&self, lost: bool) -> Result<()> {
        self.coordinator
            .prepare(&self.f.inputs(), &self.b, &self.a, lost)
    }
    fn attempt(&self, lost: bool) -> Result<(Permit, hive_google_auth::BoundAccessToken)> {
        self.coordinator.attempt(
            &self.f.inputs(),
            &self.b,
            &self.a,
            &self.store(),
            &OAuth::new(),
            "github-canary",
            lost,
            deadline(),
        )
    }
    fn dispatch(
        &self,
        permit: Permit,
        bound: hive_google_auth::BoundAccessToken,
        stop: Stop,
    ) -> Result<Phase> {
        self.coordinator.dispatch(
            permit,
            &self.f.inputs(),
            &self.b,
            &self.a,
            bound.token.expose(),
            "github-canary",
            deadline(),
            stop,
        )
    }
    fn recover(&self) -> Phase {
        self.coordinator
            .recover(&self.b, &self.store(), &OAuth::new(), deadline())
            .unwrap()
    }
    fn second(&self) -> Coordinator {
        Coordinator {
            remote: self.coordinator.remote.reopen(),
            github: github::GitHub::loopback(&self.github.base).unwrap(),
            http: Http::loopback(&format!("{}lists/", self.google.base)),
            attempt_barrier: None,
        }
    }
}

#[test]
fn integrated_real_receive_pack_and_fake_authenticated_http_applies_once() {
    let lab = Lab::new();
    lab.prepare(false).unwrap();
    let (permit, bound) = lab.attempt(false).unwrap();
    assert_eq!(
        lab.dispatch(permit, bound, Stop::None).unwrap(),
        Phase::Applied
    );
    assert_eq!(lab.patches.load(SeqCst), 1);
    assert!(lab.attempt(false).is_err());
    assert!(lab.prepare(false).is_err());
    assert_eq!(lab.recover(), Phase::Applied);
    let (view, _) = lab.coordinator.load(&lab.b).unwrap();
    assert_eq!(view.history.len(), 5);
}

#[test]
fn lost_prepared_and_attempting_ack_never_grant_dispatch_or_readback_authority() {
    for prepared in [true, false] {
        let lab = Lab::new();
        if prepared {
            assert!(lab.prepare(true).is_err());
        } else {
            lab.prepare(false).unwrap();
            assert!(lab.attempt(true).is_err());
        }
        assert_eq!(lab.patches.load(SeqCst), 0);
        let (_, state) = lab.coordinator.load(&lab.b).unwrap();
        assert_eq!(
            state.operation.unwrap().phase,
            if prepared {
                Phase::Prepared
            } else {
                Phase::Attempting
            }
        );
        if !prepared {
            for _ in 0..8 {
                assert_eq!(lab.recover(), Phase::Uncertain);
            }
            assert!(lab.attempt(false).is_err());
        }
    }
}

#[test]
fn readonly_recovery_does_not_take_over_or_revoke_paused_creator() {
    let lab = Lab::new();
    lab.prepare(false).unwrap();
    let (permit, bound) = lab.attempt(false).unwrap();
    let old = lab.coordinator.remote.observe().unwrap().oid;
    for _ in 0..12 {
        assert_eq!(lab.recover(), Phase::Uncertain);
    }
    assert_eq!(lab.coordinator.remote.observe().unwrap().oid, old);
    assert!(
        lab.second()
            .attempt(
                &lab.f.inputs(),
                &lab.b,
                &lab.a,
                &lab.store(),
                &OAuth::new(),
                "github-canary",
                false,
                deadline()
            )
            .is_err()
    );
    assert_eq!(
        lab.dispatch(permit, bound, Stop::None).unwrap(),
        Phase::Applied
    );
    assert_eq!(lab.patches.load(SeqCst), 1);
}

#[test]
fn crashes_after_patch_and_lost_evidence_or_outcome_ack_are_recovery_only() {
    for stop in [
        Stop::AfterPatch,
        Stop::LoseEvidenceAck,
        Stop::LoseOutcomeAck,
    ] {
        let lab = Lab::new();
        lab.prepare(false).unwrap();
        let (permit, bound) = lab.attempt(false).unwrap();
        assert!(lab.dispatch(permit, bound, stop).is_err());
        assert_eq!(lab.patches.load(SeqCst), 1);
        fs::remove_file(&lab.f.parser).unwrap();
        fs::remove_dir_all(&lab.f.repo).unwrap();
        let recovered = lab.recover();
        assert!(matches!(
            recovered,
            Phase::RecoveredSatisfied | Phase::Applied
        ));
        assert_eq!(lab.recover(), recovered);
        assert_eq!(lab.patches.load(SeqCst), 1);
    }
}

#[test]
fn main_source_or_state_advance_after_attempting_stops_patch_without_retry() {
    for case in 0..3 {
        let lab = Lab::new();
        lab.prepare(false).unwrap();
        let (permit, bound) = lab.attempt(false).unwrap();
        match case {
            0 => *lab.current_main.lock().unwrap() = "a".repeat(40),
            1 => {
                storage::git(
                    &lab.f.repo,
                    &["commit", "--allow-empty", "-m", "advance"],
                    b"",
                )
                .unwrap();
            }
            _ => {
                // A factual conflicting recovery advances state, not takeover.
                lab.task.lock().unwrap()["title"] = json!("changed");
                assert_eq!(lab.recover(), Phase::Conflict);
            }
        }
        assert!(lab.dispatch(permit, bound, Stop::None).is_err());
        assert_eq!(lab.patches.load(SeqCst), 0);
        assert!(lab.attempt(false).is_err());
    }
}

#[test]
fn immutable_rotation_before_attempting_cannot_acquire_authority() {
    let lab = Lab::new();
    lab.prepare(false).unwrap();
    let mut oauth = OAuth::new();
    oauth.rotated = Some("rotated-never-persisted");
    assert!(
        lab.coordinator
            .attempt(
                &lab.f.inputs(),
                &lab.b,
                &lab.a,
                &lab.store(),
                &oauth,
                "github-canary",
                false,
                deadline()
            )
            .is_err()
    );
    assert_eq!(
        lab.coordinator
            .load(&lab.b)
            .unwrap()
            .1
            .operation
            .unwrap()
            .phase,
        Phase::Prepared
    );
    assert_eq!(lab.patches.load(SeqCst), 0);
}

#[test]
fn independent_workers_race_same_old_only_one_attempt_creator() {
    let mut lab = Lab::new();
    lab.prepare(false).unwrap();
    let mut second = lab.second();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    lab.coordinator.attempt_barrier = Some(barrier.clone());
    second.attempt_barrier = Some(barrier);
    // Independent staging repositories and independent receive-pack processes.
    let results = thread::scope(|scope| {
        let mut workers = Vec::new();
        for coordinator in [&lab.coordinator, &second] {
            let lab = &lab;
            workers.push(scope.spawn(move || {
                coordinator.attempt(
                    &lab.f.inputs(),
                    &lab.b,
                    &lab.a,
                    &lab.store(),
                    &OAuth::new(),
                    "github-canary",
                    false,
                    deadline(),
                )
            }));
        }
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let (permit, bound) = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(
        lab.dispatch(permit, bound, Stop::None).unwrap(),
        Phase::Applied
    );
    assert_eq!(lab.patches.load(SeqCst), 1);
}

#[test]
fn verify_main_loopback_cannot_become_production_proof_and_admits_before_lookup() {
    let lab = Lab::new();
    let report = verify_main_with(
        lab.f.inputs(),
        lab.f.claims(),
        || Ok(Zeroizing::new("github-canary".into())),
        || github::GitHub::loopback(&lab.github.base),
    )
    .unwrap();
    assert_eq!(report["currentMainVerifiedDuringCollection"], false);
    assert_eq!(report["githubOrigin"], false);
    for key in [
        "publicationReady",
        "pushVerified",
        "githubPushAuthenticated",
        "hostedExecutionEnabled",
        "dispatchAllowed",
    ] {
        assert_eq!(report[key], false);
    }
    let text = report.to_string();
    for canary in [
        "github-canary",
        "owner/repo",
        "123",
        &lab.f.commit,
        lab.f.root.to_str().unwrap(),
    ] {
        assert!(!text.contains(canary));
    }
    let mut claims = lab.f.claims();
    claims.event_name = "pull_request".into();
    assert!(
        verify_main_with(
            lab.f.inputs(),
            claims,
            || panic!("early token"),
            || panic!("early client")
        )
        .is_err()
    );
}

#[test]
fn github_security_fields_duplicates_redirects_malformed_and_raw_errors_fail_closed() {
    let f = Fixture::new();
    let b = f.binding();
    for body in [
        "{\"id\":123,\"id\":123,\"full_name\":\"owner/repo\",\"private\":true}",
        "{\"id\":123,\"full_name\":\"owner/repo\",\"private\":false}",
        "{\"id\":124,\"full_name\":\"owner/repo\",\"private\":true}",
        "{\"id\":123,\"full_name\":\"new/repo\",\"private\":true}",
        "github-canary",
        "[]",
    ] {
        let server = Server::new(move |_, s| reply(s, 200, body));
        let http = github::GitHub::loopback(&server.base).unwrap();
        assert!(
            http.verify(&b, &f.commit, "github-canary", deadline())
                .is_err()
        );
    }
    for status in [301, 302, 403, 500] {
        let server = Server::new(move |_, s| reply(s, status, "github-canary"));
        let error = verify_main_with(
            f.inputs(),
            f.claims(),
            || Ok(Zeroizing::new("github-canary".into())),
            || github::GitHub::loopback(&server.base),
        )
        .unwrap_err();
        assert_eq!(format!("{error:#}"), "github_verification_failed");
    }
}

#[test]
fn github_main_requires_exact_ref_commit_type_full_oid_unique_and_bounded_json() {
    let f = Fixture::new();
    let b = f.binding();
    for bad in [
        json!({"ref":"refs/heads/other","object":{"type":"commit","sha":f.commit}}).to_string(),
        json!({"ref":"refs/heads/main","object":{"type":"tag","sha":f.commit}}).to_string(),
        json!({"ref":"refs/heads/main","object":{"type":"commit","sha":"a".repeat(40)}})
            .to_string(),
        json!({"ref":"refs/heads/main","object":{"type":"commit","sha":&f.commit[..7]}})
            .to_string(),
        format!(
            "{{\"ref\":\"refs/heads/main\",\"object\":{{\"type\":\"commit\",\"sha\":\"{}\",\"sha\":\"{}\"}}}}",
            f.commit, f.commit
        ),
        " ".repeat(64 * 1024 + 1),
    ] {
        let server = Server::new(move |raw, s| {
            if raw.starts_with("GET /repos/owner/repo HTTP/") {
                reply(
                    s,
                    200,
                    "{\"id\":123,\"full_name\":\"owner/repo\",\"private\":true}",
                );
            } else {
                reply(s, 200, &bad);
            }
        });
        assert!(
            github::GitHub::loopback(&server.base)
                .unwrap()
                .verify(&b, &f.commit, "github-canary", deadline())
                .is_err()
        );
    }
}

#[test]
fn source_is_rechecked_after_github_collection_and_invalid_source_precedes_token() {
    let f = Fixture::new();
    let parser = f.parser.clone();
    let commit = f.commit.clone();
    let server = Server::new(move |raw, s| {
        if raw.starts_with("GET /repos/owner/repo HTTP/") {
            reply(
                s,
                200,
                "{\"id\":123,\"full_name\":\"owner/repo\",\"private\":true}",
            );
        } else {
            fs::write(&parser, "#!/bin/sh\nexit 9\n").unwrap();
            reply(
                s,
                200,
                &json!({"ref":"refs/heads/main","object":{"type":"commit","sha":commit}})
                    .to_string(),
            );
        }
    });
    let error = verify_main_with(
        f.inputs(),
        f.claims(),
        || Ok(Zeroizing::new("github-canary".into())),
        || github::GitHub::loopback(&server.base),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "source_changed");
    assert!(
        verify_main_with(
            f.inputs(),
            f.claims(),
            || panic!("early lookup"),
            || panic!("early client")
        )
        .is_err()
    );
}

#[test]
fn mutated_server_lost_response_and_malformed_recovery_never_replay() {
    let lab = Lab::new();
    lab.prepare(false).unwrap();
    let (permit, bound) = lab.attempt(false).unwrap();
    lab.google_mode.store(1, SeqCst);
    assert_eq!(
        lab.dispatch(permit, bound, Stop::None).unwrap(),
        Phase::Uncertain
    );
    assert_eq!(lab.patches.load(SeqCst), 1);
    let frozen = lab.coordinator.remote.observe().unwrap().oid;
    for mode in [2, 3, 4, 2] {
        lab.google_mode.store(mode, SeqCst);
        assert_eq!(lab.recover(), Phase::Uncertain);
        assert_eq!(lab.coordinator.remote.observe().unwrap().oid, frozen);
    }
    lab.google_mode.store(0, SeqCst);
    assert_eq!(lab.recover(), Phase::RecoveredSatisfied);
    assert!(lab.attempt(false).is_err());
    assert_eq!(lab.patches.load(SeqCst), 1);
}

#[test]
fn invalid_transition_binding_and_insufficient_headroom_fail_before_push() {
    let lab = Lab::new();
    let genesis = lab.coordinator.remote.observe().unwrap();
    lab.prepare(false).unwrap();
    let (view, mut state) = lab.coordinator.load(&lab.b).unwrap();
    let tip = view.oid.clone();
    let op = state.operation.as_mut().unwrap();
    op.intent.source["literalText"] = json!("x".repeat(720 * 1024));
    op.id = digest(&op.intent).unwrap();
    assert!(
        lab.coordinator
            .persist(&lab.b, &genesis, &state, "synthetic", false)
            .is_err()
    );
    assert_eq!(lab.coordinator.remote.observe().unwrap().oid, tip);
    let (_, mut state) = lab.coordinator.load(&lab.b).unwrap();
    state.operation.as_mut().unwrap().phase = Phase::Applied;
    assert!(
        lab.coordinator
            .persist(&lab.b, &view, &state, "synthetic", false)
            .is_err()
    );
    let mut wrong = lab.f.binding();
    wrong.expected_repository_id = "124".into();
    assert!(lab.coordinator.load(&wrong).is_err());
    assert_eq!(lab.coordinator.remote.observe().unwrap().oid, tip);
}
