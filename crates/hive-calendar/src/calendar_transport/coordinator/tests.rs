use super::*;
use crate::{
    calendar_transport::{
        HttpTransport,
        tests::{LOGICAL, OPERATION, Reply, Server, fixture, http_test_guard, principal, token},
    },
    operations,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn operation() -> source::SourceSession {
    source::SourceSession::operation(Instant::now() + Duration::from_secs(120)).unwrap()
}

fn snapshot(ledger: Ledger, head: char) -> AuthoritativeSnapshot {
    AuthoritativeSnapshot {
        authority_id: "owned-loopback-fixture".into(),
        state_ref: STATE_REF.into(),
        head: head.to_string().repeat(40),
        path: "ledger.json".into(),
        blob: if head == 'a' { "b" } else { "d" }.repeat(40),
        ledger,
        origin: ImmutableEndpointOrigin::Loopback(12345),
    }
}

fn copy(s: &AuthoritativeSnapshot) -> AuthoritativeSnapshot {
    AuthoritativeSnapshot {
        authority_id: s.authority_id.clone(),
        state_ref: s.state_ref.clone(),
        head: s.head.clone(),
        path: s.path.clone(),
        blob: s.blob.clone(),
        ledger: s.ledger.clone(),
        origin: s.origin,
    }
}

fn clean(update: bool) -> Ledger {
    let mut old = fixture(update);
    old.entries[0].pending = None;
    old
}

fn publication(invocation: &Invocation, old: &AuthoritativeSnapshot) -> PreparedPublication {
    let mut next = old.ledger.clone();
    next.entries[0].pending = fixture(next.entries[0].provider_ack.is_some()).entries[0]
        .pending
        .clone();
    let p = next.entries[0].pending.as_ref().unwrap();
    PreparedPublication {
        old: copy(old),
        intent_digest: operations::intent_digest(&next.binding, LOGICAL, p).unwrap(),
        new: snapshot(next, 'c'),
        logical_id: LOGICAL.into(),
        operation_id: OPERATION.into(),
        invocation_nonce: invocation.nonce.clone(),
    }
}

#[derive(Default)]
enum Fault {
    #[default]
    None,
    Rejected,
    Lost,
    CancelAfter,
    LatestChanged,
    ObserveAfter,
    StageFailed,
    AlreadyApplied,
}

struct Fake {
    current: AuthoritativeSnapshot,
    fault: Fault,
    stages: usize,
    publications: usize,
    observations: usize,
    ack: AckPublicationOutcome,
}

impl Fake {
    fn new(update: bool) -> Self {
        Self {
            current: snapshot(clean(update), 'a'),
            fault: Fault::None,
            stages: 0,
            publications: 0,
            observations: 0,
            ack: AckPublicationOutcome::Unknown,
        }
    }
}
impl sealed::Sealed for Fake {}
impl AuthoritativeState for Fake {
    fn observe(&mut self, _: &source::SourceSession) -> Result<AuthoritativeSnapshot> {
        self.observations += 1;
        if self.observations > 1 {
            if matches!(self.fault, Fault::ObserveAfter) {
                return Err(anyhow!("lost observation"));
            }
            if matches!(self.fault, Fault::LatestChanged) {
                self.current.head = "e".repeat(40);
            }
        }
        Ok(copy(&self.current))
    }
    fn stage(
        &mut self,
        invocation: &Invocation,
        old: &AuthoritativeSnapshot,
        _: &source::SourceSession,
    ) -> Result<Option<PreparedPublication>> {
        self.stages += 1;
        if matches!(self.fault, Fault::StageFailed) {
            return Err(anyhow!("stage failed"));
        }
        if matches!(self.fault, Fault::AlreadyApplied) {
            return Ok(None);
        }
        Ok(Some(publication(invocation, old)))
    }
    fn publish(
        &mut self,
        p: PreparedPublication,
        root: &source::SourceSession,
    ) -> PublicationOutcome {
        self.publications += 1;
        if !p.old.same_basis(&self.current) || matches!(self.fault, Fault::Rejected) {
            return PublicationOutcome::Rejected;
        }
        self.current = copy(&p.new);
        if matches!(self.fault, Fault::Lost) {
            return PublicationOutcome::Unknown;
        }
        if matches!(self.fault, Fault::CancelAfter) {
            root.cancel();
        }
        PublicationOutcome::FreshWon(FreshCasReceipt {
            publication: Box::new(p),
        })
    }
    fn publish_ack(
        &mut self,
        _: &AckPublication,
        root: &source::SourceSession,
    ) -> AckPublicationOutcome {
        if matches!(self.fault, Fault::CancelAfter) {
            root.cancel();
        }
        match self.ack {
            AckPublicationOutcome::Confirmed => AckPublicationOutcome::Confirmed,
            AckPublicationOutcome::Rejected => AckPublicationOutcome::Rejected,
            AckPublicationOutcome::Unknown => AckPublicationOutcome::Unknown,
        }
    }
}

#[test]
fn fresh_creator_gets_one_loopback_permit_pinned_to_h1() {
    for update in [false, true] {
        let mut backend = Fake::new(update);
        let binding = backend.current.ledger.binding.clone();
        let InvocationOutcome::Ready {
            permit,
            intent,
            context,
        } = invoke(&mut backend, &binding, &operation()).unwrap()
        else {
            panic!("no permit")
        };
        assert_eq!(permit.ledger_head.as_ref(), Some(&backend.current.head));
        assert_eq!(permit.owner_nonce.as_ref().unwrap().len(), 32);
        assert!(
            permit
                .owner_nonce
                .as_ref()
                .unwrap()
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert!(permit.origin == ImmutableEndpointOrigin::Loopback(12345));
        let mut attached =
            PreparedFrozenIntent::prepare(&backend.current.ledger, &binding, LOGICAL, OPERATION)
                .unwrap();
        context.attach(&mut attached).unwrap();
        assert_eq!(attached.context_digest, intent.context_digest);
        let transport =
            HttpTransport::loopback("http://127.0.0.1:12345/", Duration::from_secs(1)).unwrap();
        (*permit)
            .validate(&intent, &transport, intent.kind)
            .unwrap();
        assert_eq!(backend.publications, 1);
        assert!(matches!(
            invoke(&mut backend, &binding, &operation()).unwrap(),
            InvocationOutcome::RecoverOnly(_)
        ));
        assert_eq!(backend.stages, 1);
    }
}

#[test]
fn acquisition_handoff_discards_ready_after_root_cancellation_or_expiry() {
    for expired in [false, true] {
        let mut backend = Fake::new(false);
        let binding = backend.current.ledger.binding.clone();
        let ready = invoke(&mut backend, &binding, &operation()).unwrap();
        assert!(matches!(&ready, InvocationOutcome::Ready { .. }));
        let root = if expired {
            source::SourceSession::operation(Instant::now() - Duration::from_secs(1)).unwrap()
        } else {
            let root = operation();
            root.cancel();
            root
        };
        assert!(root.check().is_err());
        assert!(matches!(
            finish_acquisition(Ok(ready), &root),
            InvocationOutcome::PublicationUnknown
        ));
        assert!(matches!(
            invoke(&mut backend, &binding, &operation()).unwrap(),
            InvocationOutcome::RecoverOnly(_)
        ));
        assert_eq!(backend.publications, 1);
    }
}

#[test]
fn any_pending_including_other_entry_is_recovery_only() {
    for phase in [OperationPhase::Prepared, OperationPhase::Uncertain] {
        for other in [false, true] {
            let mut backend = Fake::new(false);
            let mut entry = fixture(false).entries.remove(0);
            entry.pending.as_mut().unwrap().phase = phase;
            if other {
                entry.logical_id = "33333333-3333-4333-8333-333333333333".into();
                entry.source_ack.path = "other.md".into();
                entry.pending.as_mut().unwrap().intended_source = entry.source_ack.clone();
                entry.pending.as_mut().unwrap().target_event_id = operations::reserved_event_id(
                    &backend.current.ledger.binding,
                    &entry.logical_id,
                )
                .unwrap();
                backend.current.ledger.entries.push(entry);
            } else {
                backend.current.ledger.entries[0] = entry;
            }
            let binding = backend.current.ledger.binding.clone();
            let InvocationOutcome::RecoverOnly(metadata) =
                invoke(&mut backend, &binding, &operation()).unwrap()
            else {
                panic!("not recovery")
            };
            assert_eq!(metadata.len(), 1);
            assert_eq!(metadata[0].head, backend.current.head);
            assert_eq!(metadata[0].phase, phase);
            assert_eq!(metadata[0].operation_id, OPERATION);
            assert_eq!(
                metadata[0].logical_id,
                backend.current.ledger.entries[usize::from(other)].logical_id
            );
            assert_eq!((backend.stages, backend.publications), (0, 0));
        }
    }
}

#[test]
fn lost_cas_reply_readback_does_not_reacquire() {
    let mut backend = Fake::new(false);
    backend.fault = Fault::Lost;
    let binding = backend.current.ledger.binding.clone();
    assert!(matches!(
        invoke(&mut backend, &binding, &operation()).unwrap(),
        InvocationOutcome::PublicationUnknown
    ));
    backend.fault = Fault::None;
    assert!(matches!(
        invoke(&mut backend, &binding, &operation()).unwrap(),
        InvocationOutcome::RecoverOnly(_)
    ));
    assert_eq!(backend.publications, 1);
}

#[test]
fn cancellation_and_observation_failure_after_cas_never_mint() {
    for fault in [
        Fault::CancelAfter,
        Fault::LatestChanged,
        Fault::ObserveAfter,
    ] {
        let mut backend = Fake::new(false);
        backend.fault = fault;
        let binding = backend.current.ledger.binding.clone();
        assert!(matches!(
            invoke(&mut backend, &binding, &operation()).unwrap(),
            InvocationOutcome::PublicationUnknown
        ));
        assert!(backend.current.ledger.entries[0].pending.is_some());
        backend.fault = Fault::None;
        assert!(matches!(
            invoke(&mut backend, &binding, &operation()).unwrap(),
            InvocationOutcome::RecoverOnly(_)
        ));
    }
}

#[test]
fn staging_failure_already_applied_and_precancel_do_not_publish() {
    for fault in [Fault::StageFailed, Fault::AlreadyApplied] {
        let mut backend = Fake::new(false);
        backend.fault = fault;
        let binding = backend.current.ledger.binding.clone();
        let result = invoke(&mut backend, &binding, &operation());
        assert!(result.is_err() || matches!(result, Ok(InvocationOutcome::NotAcquired)));
        assert_eq!(backend.publications, 0);
    }
    let root = operation();
    root.cancel();
    let mut backend = Fake::new(false);
    let binding = backend.current.ledger.binding.clone();
    assert!(invoke(&mut backend, &binding, &root).is_err());
    assert_eq!(backend.observations, 0);
}

#[test]
fn receipt_rejects_different_invocation_and_all_changed_bases() {
    for mutation in 0..16 {
        let invocation = Invocation::fresh().unwrap();
        let old = snapshot(clean(false), 'a');
        let mut p = publication(&invocation, &old);
        let mut latest = copy(&p.new);
        match mutation {
            0 => p.invocation_nonce.push('!'),
            1 => p.intent_digest.push('!'),
            2 => p.operation_id.push('!'),
            3 => p.logical_id.push('!'),
            4 => latest.authority_id.push('!'),
            5 => latest.head = "e".repeat(40),
            6 => latest.path = "other.json".into(),
            7 => latest.blob = "e".repeat(40),
            8 => latest.ledger.binding.calendar_id.push('!'),
            9 => latest.origin = ImmutableEndpointOrigin::Loopback(12346),
            10 => latest.state_ref.push('!'),
            11 => p.new.authority_id.push('!'),
            12 => p.new.origin = ImmutableEndpointOrigin::Google,
            13 => p.old.head = "f".repeat(40),
            14 => p.new.ledger.entries[0].source_ack.expected.text.push('!'),
            _ => p.new.ledger.source_watermark = "e".repeat(40),
        }
        assert!(
            RemoteDispatchAuthority::from_fresh(
                FreshCasReceipt {
                    publication: Box::new(p)
                },
                invocation,
                &old,
                latest
            )
            .is_err(),
            "mutation {mutation}"
        );
    }
    let old = snapshot(clean(false), 'a');
    let p = publication(&Invocation::fresh().unwrap(), &old);
    let latest = copy(&p.new);
    assert!(
        RemoteDispatchAuthority::from_fresh(
            FreshCasReceipt {
                publication: Box::new(p)
            },
            Invocation::fresh().unwrap(),
            &old,
            latest
        )
        .is_err()
    );
}

#[test]
fn paused_creator_is_not_revoked_but_second_invocation_never_writes() {
    let mut backend = Fake::new(false);
    let binding = backend.current.ledger.binding.clone();
    let InvocationOutcome::Ready { permit, intent, .. } =
        invoke(&mut backend, &binding, &operation()).unwrap()
    else {
        panic!("no permit")
    };
    for _ in 0..3 {
        assert!(matches!(
            invoke(&mut backend, &binding, &operation()).unwrap(),
            InvocationOutcome::RecoverOnly(_)
        ));
    }
    assert_eq!(backend.publications, 1);
    (*permit)
        .validate(
            &intent,
            &HttpTransport::loopback("http://127.0.0.1:12345/", Duration::from_secs(1)).unwrap(),
            OperationKind::Create,
        )
        .unwrap();
}

// This adapter exists only in tests and owns both repos for their entire lifetime.
// Object transfer is a bounded local pack, not fetch/push, and never changes refs,
// FETCH_HEAD, tags, hooks, config or credentials. Normal I/O uses GitSession guards.
struct Bare {
    _root: tempfile::TempDir,
    worker: PathBuf,
    authority: PathBuf,
    binding: Binding,
    candidate: String,
    lost: bool,
    stages: usize,
}

fn git(repo: &Path, root: &source::SourceSession) -> source::GitSession {
    source::GitSession::with_operation(repo, Instant::now() + Duration::from_secs(120), root)
        .unwrap()
}
fn text(git: &source::GitSession, args: &[&str], bytes: &[u8]) -> String {
    String::from_utf8(git.run(args, bytes, 1024 * 1024).unwrap())
        .unwrap()
        .trim()
        .into()
}
fn commit(
    git: &source::GitSession,
    ledger: &Ledger,
    parent: Option<&str>,
    message: &[u8],
) -> String {
    let blob = text(
        git,
        &["hash-object", "-w", "--stdin"],
        &serde_json::to_vec(ledger).unwrap(),
    );
    let tree = text(
        git,
        &["mktree"],
        format!("100644 blob {blob}\tledger.json\n").as_bytes(),
    );
    let mut args = vec![
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@localhost",
        "commit-tree",
        &tree,
    ];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    text(git, &args, message)
}

impl Bare {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let worker = root.path().join("worker.git");
        let authority = root.path().join("authority.git");
        for repo in [&worker, &authority] {
            let output = std::process::Command::new("/usr/bin/git")
                .env_clear()
                .env("HOME", root.path())
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "init",
                    "--bare",
                    "--template=",
                    "--object-format=sha1",
                ])
                .arg(repo)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        let session = operation();
        let g = git(&worker, &session);
        let old = commit(&g, &clean(false), None, b"ordinary fixture H0\n");
        let candidate = commit(&g, &fixture(false), Some(&old), b"ordinary fixture H1\n");
        let mut backend = Self {
            _root: root,
            worker,
            authority,
            binding: fixture(false).binding,
            candidate,
            lost: false,
            stages: 0,
        };
        backend.transfer(&old, &session);
        text(
            &git(&backend.authority, &session),
            &["update-ref", STATE_REF, &old, &"0".repeat(40)],
            b"",
        );
        backend
    }
    fn transfer(&mut self, head: &str, root: &source::SourceSession) {
        assert!(
            self.worker
                .canonicalize()
                .unwrap()
                .starts_with(self._root.path().canonicalize().unwrap())
        );
        assert!(
            self.authority
                .canonicalize()
                .unwrap()
                .starts_with(self._root.path().canonicalize().unwrap())
        );
        let pack = git(&self.worker, root)
            .run(
                &["pack-objects", "--stdout", "--revs"],
                format!("{head}\n").as_bytes(),
                8 * 1024 * 1024,
            )
            .unwrap();
        git(&self.authority, root)
            .run(&["unpack-objects"], &pack, 1024 * 1024)
            .unwrap();
    }
    fn at(
        &self,
        repo: &Path,
        head: &str,
        root: &source::SourceSession,
    ) -> Result<AuthoritativeSnapshot> {
        let g = source::GitSession::with_operation(
            repo,
            Instant::now() + Duration::from_secs(120),
            root,
        )?;
        ensure!(
            text(&g, &["cat-file", "-t", head], b"") == "commit",
            "not commit"
        );
        let blob = text(&g, &["rev-parse", &format!("{head}:ledger.json")], b"");
        let bytes = g.run(&["cat-file", "blob", &blob], b"", 1024 * 1024)?;
        let mut s = snapshot(crate::ledger::decode(&bytes, &self.binding)?, 'a');
        s.head = head.into();
        s.blob = blob;
        s.validate(&self.binding)?;
        Ok(s)
    }
}
impl sealed::Sealed for Bare {}
impl AuthoritativeState for Bare {
    fn observe(&mut self, root: &source::SourceSession) -> Result<AuthoritativeSnapshot> {
        let g = source::GitSession::with_operation(
            &self.authority,
            Instant::now() + Duration::from_secs(120),
            root,
        )?;
        let head = String::from_utf8(g.run(&["rev-parse", "--verify", STATE_REF], b"", 1024)?)?;
        self.at(&self.authority, head.trim(), root)
    }
    fn stage(
        &mut self,
        invocation: &Invocation,
        old: &AuthoritativeSnapshot,
        root: &source::SourceSession,
    ) -> Result<Option<PreparedPublication>> {
        self.stages += 1;
        let g = source::GitSession::with_operation(
            &self.worker,
            Instant::now() + Duration::from_secs(120),
            root,
        )?;
        let raw = g.run(&["cat-file", "commit", &self.candidate], b"", 1024 * 1024)?;
        let raw = std::str::from_utf8(&raw)?;
        let parents: Vec<_> = raw
            .split("\n\n")
            .next()
            .unwrap()
            .lines()
            .filter_map(|s| s.strip_prefix("parent "))
            .collect();
        ensure!(
            parents == [old.head.as_str()],
            "staged commit must have sole parent H0"
        );
        let proposed = self.at(&self.worker, &self.candidate, root)?;
        // The fixture candidate is input, not a local apply receipt. Actually
        // stage a new commit in THIS invocation, with an invocation-bound message.
        let staged = commit(
            &g,
            &proposed.ledger,
            Some(&old.head),
            format!("fresh fixture prepare {}\n", invocation.nonce).as_bytes(),
        );
        let new = self.at(&self.worker, &staged, root)?;
        let pending = new.ledger.entries[0]
            .pending
            .as_ref()
            .ok_or_else(|| anyhow!("not a prepare"))?;
        let p = PreparedPublication {
            old: copy(old),
            intent_digest: operations::intent_digest(&self.binding, LOGICAL, pending)?,
            new,
            logical_id: LOGICAL.into(),
            operation_id: OPERATION.into(),
            invocation_nonce: invocation.nonce.clone(),
        };
        p.validate(invocation, old)?;
        Ok(Some(p))
    }
    fn publish(
        &mut self,
        p: PreparedPublication,
        root: &source::SourceSession,
    ) -> PublicationOutcome {
        if root.check().is_err() {
            return PublicationOutcome::Rejected;
        }
        self.transfer(&p.new.head, root);
        let g = git(&self.authority, root);
        // Only this server-side exact OLD compare creates the receipt. No readback
        // after failure can recover it, even if the ref now happens to equal H1.
        match g.run(
            &["update-ref", STATE_REF, &p.new.head, &p.old.head],
            b"",
            1024,
        ) {
            Ok(_) if !self.lost => PublicationOutcome::FreshWon(FreshCasReceipt {
                publication: Box::new(p),
            }),
            Ok(_) => PublicationOutcome::Unknown,
            Err(_) => PublicationOutcome::Unknown,
        }
    }
    fn publish_ack(
        &mut self,
        p: &AckPublication,
        root: &source::SourceSession,
    ) -> AckPublicationOutcome {
        self.transfer(&p.new.head, root);
        match git(&self.authority, root).run(
            &["update-ref", STATE_REF, &p.new.head, &p.old.head],
            b"",
            1024,
        ) {
            Ok(_) if !self.lost => AckPublicationOutcome::Confirmed,
            _ => AckPublicationOutcome::Unknown,
        }
    }
}

#[test]
fn bare_fresh_cas_and_lost_reply_recovery() {
    for lost in [false, true] {
        let mut backend = Bare::new();
        backend.lost = lost;
        let binding = backend.binding.clone();
        let result = invoke(&mut backend, &binding, &operation()).unwrap();
        assert!(matches!(
            (lost, result),
            (false, InvocationOutcome::Ready { .. })
                | (true, InvocationOutcome::PublicationUnknown)
        ));
        assert!(matches!(
            invoke(&mut backend, &binding, &operation()).unwrap(),
            InvocationOutcome::RecoverOnly(_)
        ));
        assert_eq!(backend.stages, 1);
    }
}

#[test]
fn bare_two_workers_same_h0_at_most_one_wins() {
    let mut backend = Bare::new();
    let root = operation();
    let old = backend.observe(&root).unwrap();
    let first = Invocation::fresh().unwrap();
    let second = Invocation::fresh().unwrap();
    let p1 = backend.stage(&first, &old, &root).unwrap().unwrap();
    let p2 = backend.stage(&second, &old, &root).unwrap().unwrap();
    let PublicationOutcome::FreshWon(receipt) = backend.publish(p1, &root) else {
        panic!("no winner")
    };
    assert!(!matches!(
        backend.publish(p2, &root),
        PublicationOutcome::FreshWon(_)
    ));
    let latest = backend.observe(&root).unwrap();
    assert!(matches!(
        RemoteDispatchAuthority::from_fresh(receipt, first, &old, latest).unwrap(),
        InvocationOutcome::Ready { .. }
    ));
}

#[test]
fn bare_whole_head_cas_rejects_missing_advanced_and_rollback_even_identical_ledger() {
    for change in 0..3 {
        let mut backend = Bare::new();
        let root = operation();
        let old = backend.observe(&root).unwrap();
        let invocation = Invocation::fresh().unwrap();
        let p = backend.stage(&invocation, &old, &root).unwrap().unwrap();
        let g = git(&backend.authority, &root);
        let changed = match change {
            0 => {
                text(&g, &["update-ref", "-d", STATE_REF, &old.head], b"");
                None
            }
            1 => Some(commit(
                &g,
                &old.ledger,
                Some(&old.head),
                b"advanced identical ledger\n",
            )),
            _ => Some(commit(
                &g,
                &old.ledger,
                None,
                b"rollback unrelated root same ledger\n",
            )),
        };
        if let Some(head) = &changed {
            text(&g, &["update-ref", STATE_REF, head, &old.head], b"");
        }
        assert!(!matches!(
            backend.publish(p, &root),
            PublicationOutcome::FreshWon(_)
        ));
        let actual = g.run(&["rev-parse", "--verify", STATE_REF], b"", 1024);
        if let Some(head) = changed {
            assert_eq!(String::from_utf8(actual.unwrap()).unwrap().trim(), head);
        } else {
            assert!(actual.is_err());
        }
    }
}

#[test]
fn bare_malformed_candidate_or_parent_and_unchanged_pending_cannot_acquire() {
    for mutation in 0..4 {
        let mut backend = Bare::new();
        let root = operation();
        let old = backend.observe(&root).unwrap();
        let g = git(&backend.worker, &root);
        backend.candidate = match mutation {
            0 => commit(&g, &fixture(false), None, b"wrong parent\n"),
            1 => commit(&g, &clean(false), Some(&old.head), b"no new pending\n"),
            2 => text(&g, &["hash-object", "-w", "--stdin"], b"not a commit"),
            _ => {
                let mut bad = fixture(false);
                bad.entries[0].source_ack.expected.text.push('!');
                commit(&g, &bad, Some(&old.head), b"bad diff\n")
            }
        };
        // Check raw object type without the assert-based fixture reader.
        if mutation == 2 {
            assert!(
                backend
                    .stage(&Invocation::fresh().unwrap(), &old, &root)
                    .is_err()
            );
        } else {
            assert!(invoke(&mut backend, &old.ledger.binding, &root).is_err());
        }
        assert_eq!(backend.observe(&root).unwrap().head, old.head);
    }
}

#[test]
fn ack_local_success_is_not_completion_and_only_remote_confirmed_completes() {
    let mut backend = Fake::new(false);
    let p = ack_fixture(&snapshot(fixture(false), 'c'));
    assert_eq!(
        publish_ack(&mut backend, &p, &operation()),
        Completion::AckPublicationUnknown
    );
    backend.ack = AckPublicationOutcome::Rejected;
    assert_eq!(
        publish_ack(&mut backend, &p, &operation()),
        Completion::AckPublicationPending
    );
    backend.ack = AckPublicationOutcome::Confirmed;
    assert_eq!(
        publish_ack(&mut backend, &p, &operation()),
        Completion::Completed
    );
    let root = operation();
    root.cancel();
    assert_eq!(
        publish_ack(&mut backend, &p, &root),
        Completion::AckPublicationPending
    );
}

fn ack_fixture(old: &AuthoritativeSnapshot) -> AckPublication {
    let pending = old.ledger.entries[0].pending.clone().unwrap();
    let proof = ack::fixture_proof(ack::AckAudit {
        schema_version: "1".into(),
        binding: old.ledger.binding.clone(),
        base_commit: old.head.clone(),
        path: old.path.clone(),
        old_blob_id: old.blob.clone(),
        logical_id: LOGICAL.into(),
        operation_id: OPERATION.into(),
        intent_digest: operations::intent_digest(&old.ledger.binding, LOGICAL, &pending).unwrap(),
        provider_ack: crate::ProviderAck {
            event_id: pending.target_event_id.clone(),
            etag: "\"fixture-result\"".into(),
            last_payload_hash: pending.desired_payload_hash.clone().unwrap(),
        },
        old_pending: pending,
        cause: ack::AckCause::MutationResponse,
        origin: ack::AckOrigin::SyntheticFixture,
        evidence_digest: String::new(),
    })
    .unwrap();
    let audit = proof.audit();
    let mut new = old.ledger.clone();
    new.entries[0].pending = None;
    new.entries[0].lifecycle = Lifecycle::Active;
    new.entries[0].provider_ack = Some(audit.provider_ack.clone());
    let bytes = serde_json::to_vec(&new).unwrap();
    ack::validate_transition(&serde_json::to_vec(&old.ledger).unwrap(), &bytes, audit).unwrap();
    let mut next = copy(old);
    next.ledger = new;
    next.head = "e".repeat(40);
    next.blob = "f".repeat(40);
    AckPublication {
        old: copy(old),
        new: next,
        audit: Box::new(audit.clone()),
    }
}

// Fixture-only V3 builder. This is deliberately NOT a production constructor,
// nor a bridge from loopback ValidatedSuccess to Google ACK evidence.
fn stage_ack(
    backend: &Bare,
    old: &AuthoritativeSnapshot,
    root: &source::SourceSession,
) -> AckPublication {
    let p = ack_fixture(old);
    let audit = &p.audit;
    let new = &p.new.ledger;
    let bytes = serde_json::to_vec(new).unwrap();
    let g = git(&backend.worker, root);
    let blob = text(&g, &["hash-object", "-w", "--stdin"], &bytes);
    let record = serde_json::json!({
        "schemaVersion":"3", "stateRef":STATE_REF, "path":old.path,
        "binding":old.ledger.binding, "ledgerBlobId":blob,
        "candidateSha256":audit.evidence_digest.strip_prefix("sha256:").unwrap(),
        "sourceCommit":audit.old_pending.intended_source.commit, "operation":"acknowledge",
        "logicalId":LOGICAL, "parserChangeAccepted":false, "baseCommit":old.head,
        "expectedBlobId":old.blob, "proof":{"kind":"Acknowledgement","value":audit},
        "provenanceFloor":"synthetic_fixture"
    });
    let message = format!("hive-calendar-state v3\n\n{record}\n");
    let head = commit(&g, new, Some(&old.head), message.as_bytes());
    // Check the actual committed hash/tree/parent and strict transition, not a
    // mutable local ApplyReceipt or just a purported publication status.
    let committed = backend.at(&backend.worker, &head, root).unwrap();
    assert_eq!(committed.blob, blob);
    let raw = text(&g, &["cat-file", "commit", &head], b"");
    let (headers, body) = raw.split_once("\n\n").unwrap();
    assert_eq!(
        headers
            .lines()
            .filter_map(|s| s.strip_prefix("parent "))
            .collect::<Vec<_>>(),
        [old.head.as_str()]
    );
    assert_eq!(body, message.trim());
    let stored: serde_json::Value = crate::ledger::decode_unique_json(
        body.strip_prefix("hive-calendar-state v3\n\n")
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(stored, record);
    let stored_audit: ack::AckAudit =
        crate::ledger::decode_unique_json(&serde_json::to_vec(&stored["proof"]["value"]).unwrap())
            .unwrap();
    ack::validate_transition(
        &serde_json::to_vec(&old.ledger).unwrap(),
        &serde_json::to_vec(&committed.ledger).unwrap(),
        &stored_audit,
    )
    .unwrap();
    AckPublication {
        old: copy(old),
        new: committed,
        audit: Box::new(stored_audit),
    }
}

#[test]
fn bare_synthetic_v3_ack_only_confirmed_remote_cas_completes() {
    for mode in 0..3 {
        let mut backend = Bare::new();
        let root = operation();
        let binding = backend.binding.clone();
        assert!(matches!(
            invoke(&mut backend, &binding, &root).unwrap(),
            InvocationOutcome::Ready { .. }
        ));
        let old = backend.observe(&root).unwrap();
        let publication = stage_ack(&backend, &old, &root);
        // Creating H2 objects locally is not publication or completion.
        assert_eq!(backend.observe(&root).unwrap().head, old.head);
        if mode == 1 {
            backend.lost = true;
        }
        if mode == 2 {
            let g = git(&backend.authority, &root);
            let advanced = commit(
                &g,
                &old.ledger,
                Some(&old.head),
                b"concurrent identical pending\n",
            );
            text(&g, &["update-ref", STATE_REF, &advanced, &old.head], b"");
        }
        let outcome = publish_ack(&mut backend, &publication, &root);
        assert_eq!(
            outcome,
            if mode == 0 {
                Completion::Completed
            } else {
                Completion::AckPublicationUnknown
            }
        );
        if mode != 2 {
            assert_eq!(backend.observe(&root).unwrap().head, publication.new.head);
        } else {
            assert!(
                backend.observe(&root).unwrap().ledger.entries[0]
                    .pending
                    .is_some()
            );
        }
    }
}

#[test]
fn rejected_publication_never_mints() {
    let mut backend = Fake::new(false);
    backend.fault = Fault::Rejected;
    let binding = backend.current.ledger.binding.clone();
    assert!(matches!(
        invoke(&mut backend, &binding, &operation()).unwrap(),
        InvocationOutcome::NotAcquired
    ));
    assert!(backend.current.ledger.entries[0].pending.is_none());
}

#[test]
fn malformed_authoritative_ledger_fails_before_staging() {
    let mut backend = Bare::new();
    let root = operation();
    let old = backend.observe(&root).unwrap();
    let mut mixed_case = copy(&old);
    mixed_case.path = "ledger.JsOn".into();
    mixed_case.validate(&backend.binding).unwrap();
    let mut wrong_binding = backend.binding.clone();
    wrong_binding.source_repo_id.push('!');
    assert!(mixed_case.validate(&wrong_binding).is_err());
    for len in [40, 64] {
        let mut basis = copy(&mixed_case);
        basis.head = "a".repeat(len);
        basis.blob = "b".repeat(len);
        basis.validate(&backend.binding).unwrap();
        let mut zero_head = copy(&basis);
        zero_head.head = "0".repeat(len);
        assert!(zero_head.validate(&backend.binding).is_err());
        basis.blob = "0".repeat(len);
        assert!(basis.validate(&backend.binding).is_err());
    }
    let g = git(&backend.authority, &root);
    let mut malformed = old.ledger.clone();
    malformed.entries[0].logical_id = "invalid".into();
    let head = commit(
        &g,
        &malformed,
        Some(&old.head),
        b"malformed source fixture\n",
    );
    text(&g, &["update-ref", STATE_REF, &head, &old.head], b"");
    let binding = backend.binding.clone();
    assert!(invoke(&mut backend, &binding, &root).is_err());
    assert_eq!(backend.stages, 0);
    assert_eq!(text(&g, &["rev-parse", "--verify", STATE_REF], b""), head);
}

#[test]
fn ack_changed_basis_or_unrelated_transition_never_completes() {
    for mutation in 0..5 {
        let mut backend = Fake::new(false);
        backend.ack = AckPublicationOutcome::Confirmed;
        let mut p = ack_fixture(&snapshot(fixture(false), 'c'));
        match mutation {
            0 => p.new.authority_id.push('!'),
            1 => p.new.origin = ImmutableEndpointOrigin::Google,
            2 => p.new.ledger.entries[0].source_ack.expected.text.push('!'),
            3 => p.audit.base_commit = "a".repeat(40),
            _ => p.new.head = p.old.head.clone(),
        }
        assert_eq!(
            publish_ack(&mut backend, &p, &operation()),
            Completion::AckPublicationPending
        );
    }
}

#[test]
fn prepare_diff_rejects_unrelated_evidence_noop_and_invalid_intents() {
    for mutation in 0..12 {
        let invocation = Invocation::fresh().unwrap();
        let old = snapshot(clean(true), 'a');
        let mut p = publication(&invocation, &old);
        let entry = &mut p.new.ledger.entries[0];
        match mutation {
            0 => entry.lifecycle = Lifecycle::Enrolled,
            1 => entry.provider_ack.as_mut().unwrap().etag = "\"changed\"".into(),
            2 => entry.source_ack.expected.text.push('!'),
            3 => entry.pending.as_mut().unwrap().phase = OperationPhase::Uncertain,
            4 => entry.pending.as_mut().unwrap().base_etag = Some("\"wrong\"".into()),
            5 => entry.pending.as_mut().unwrap().kind = OperationKind::Delete,
            6 => entry.pending.as_mut().unwrap().target_event_id.push('!'),
            7 => {
                entry.pending.as_mut().unwrap().desired_payload_hash =
                    Some(format!("sha256:{}", "0".repeat(64)))
            }
            8 => entry.pending.as_mut().unwrap().operation_id = LOGICAL.into(),
            9 => p.new.ledger.source_watermark = "f".repeat(40),
            10 => {
                entry.pending = None;
            }
            _ => p.new.head = old.head.clone(),
        }
        assert!(
            p.validate(&invocation, &old).is_err(),
            "mutation {mutation}"
        );
    }
    let invocation = Invocation::fresh().unwrap();
    let mut old = snapshot(clean(true), 'a');
    old.ledger.entries[0]
        .provider_ack
        .as_mut()
        .unwrap()
        .last_payload_hash = fixture(true).entries[0]
        .pending
        .as_ref()
        .unwrap()
        .desired_payload_hash
        .clone()
        .unwrap();
    assert!(
        publication(&invocation, &old)
            .validate(&invocation, &old)
            .is_err()
    );
}

#[test]
fn ack_cancellation_after_confirmed_remote_boundary_is_unknown() {
    let mut backend = Fake::new(false);
    backend.fault = Fault::CancelAfter;
    backend.ack = AckPublicationOutcome::Confirmed;
    let p = ack_fixture(&snapshot(fixture(false), 'c'));
    assert_eq!(
        publish_ack(&mut backend, &p, &operation()),
        Completion::AckPublicationUnknown
    );
}

#[test]
fn loopback_recovery_get_404_and_410_never_acquire() {
    let _guard = http_test_guard();
    for status in [404, 410] {
        let server = Server::new(vec![principal(), Reply::raw(status, b"{}".to_vec())]);
        let transport = server.transport();
        let mut backend = Fake::new(false);
        backend.current.ledger = fixture(false);
        backend.current.origin = transport.origin;
        let binding = backend.current.ledger.binding.clone();
        assert!(matches!(
            invoke(&mut backend, &binding, &operation()).unwrap(),
            InvocationOutcome::RecoverOnly(_)
        ));
        let intent =
            PreparedFrozenIntent::prepare(&backend.current.ledger, &binding, LOGICAL, OPERATION)
                .unwrap();
        assert!(matches!(
            transport.reconcile(&token(), &intent),
            Err(super::super::TransportError::Missing)
        ));
        let requests = server.seen();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
        assert!(matches!(
            invoke(&mut backend, &binding, &operation()).unwrap(),
            InvocationOutcome::RecoverOnly(_)
        ));
        assert_eq!(backend.publications, 0);
    }
}
