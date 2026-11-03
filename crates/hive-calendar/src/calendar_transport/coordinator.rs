//! Creator-only acquisition, not a lease or cross-service fencing protocol.
//! No production backend exists. A future backend must atomically compare the
//! exact OLD object ID at the authority, not infer success from push or readback.
//! The original creator may dispatch late; there is no revocation or takeover.
//! Source eligibility remains the preparer's responsibility; this layer has no
//! latest-note input. The fresh view is fail-closed, not cross-service atomicity.
use anyhow::{Result, anyhow, ensure};

use super::{
    Binding, DispatchPermit, ImmutableEndpointOrigin, Ledger, OperationKind, OperationPhase,
    PreparedFrozenIntent, ack, dispatch::RemoteDispatchAuthority,
};
use crate::{LedgerLocation, Lifecycle, source};

const STATE_REF: &str = "refs/heads/hive-calendar-state";

pub(super) struct Invocation {
    nonce: String,
}

impl Invocation {
    fn fresh() -> Result<Self> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| anyhow!("coordinator entropy unavailable"))?;
        Ok(Self {
            nonce: bytes.iter().map(|b| format!("{b:02x}")).collect(),
        })
    }
}

// Snapshots are obtained anew by observe, never deserialized as capabilities.
pub(super) struct AuthoritativeSnapshot {
    authority_id: String,
    state_ref: String,
    head: String,
    path: String,
    blob: String,
    ledger: Ledger,
    origin: ImmutableEndpointOrigin,
}

impl AuthoritativeSnapshot {
    fn validate(&self, binding: &Binding) -> Result<()> {
        let oid = |s: &str| {
            s.len() == self.head.len()
                && s.bytes().any(|b| b != b'0')
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        ensure!(
            !self.authority_id.is_empty()
                && self.state_ref == STATE_REF
                && matches!(self.head.len(), 40 | 64)
                && oid(&self.head)
                && oid(&self.blob)
                && self.path.to_ascii_lowercase().ends_with(".json")
                && self.path.len() <= 4096
                && !self.path.contains('\\')
                && !self.path.chars().any(char::is_control)
                && self.path.split('/').count() <= 64
                && self.path.split('/').all(|s| !s.is_empty()
                    && s != "."
                    && s != ".."
                    && !s.eq_ignore_ascii_case(".git")),
            "invalid authoritative basis"
        );
        let bytes = serde_json::to_vec(&self.ledger)?;
        crate::ledger::decode(&bytes, binding)?;
        Ok(())
    }

    fn same_basis(&self, other: &Self) -> bool {
        self.authority_id == other.authority_id
            && self.state_ref == other.state_ref
            && self.head == other.head
            && self.path == other.path
            && self.blob == other.blob
            && self.ledger == other.ledger
            && self.origin == other.origin
    }
}

struct PreparedPublication {
    old: AuthoritativeSnapshot,
    new: AuthoritativeSnapshot,
    logical_id: String,
    operation_id: String,
    intent_digest: String,
    invocation_nonce: String,
}

impl PreparedPublication {
    // Only a staging backend can supply the local fresh-staging fact. Public
    // ApplyReceipt fields, already_applied and committed pending are not that fact.
    fn validate(&self, invocation: &Invocation, observed: &AuthoritativeSnapshot) -> Result<()> {
        self.old.validate(&observed.ledger.binding)?;
        self.new.validate(&observed.ledger.binding)?;
        ensure!(self.old.same_basis(observed), "stale staging basis");
        ensure!(
            self.invocation_nonce == invocation.nonce
                && self.new.head != self.old.head
                && self.new.blob != self.old.blob
                && self.new.authority_id == self.old.authority_id
                && self.new.state_ref == self.old.state_ref
                && self.new.path == self.old.path
                && self.new.origin == self.old.origin
                && self.old.ledger.entries.iter().all(|e| e.pending.is_none()),
            "invalid fresh publication"
        );
        let intent = PreparedFrozenIntent::prepare(
            &self.new.ledger,
            &self.new.ledger.binding,
            &self.logical_id,
            &self.operation_id,
        )
        .map_err(|_| anyhow!("invalid frozen publication"))?;
        ensure!(
            intent.phase == OperationPhase::Prepared && intent.digest == self.intent_digest,
            "publication intent mismatch"
        );
        let mut expected = self.old.ledger.clone();
        let entry = expected
            .entries
            .iter_mut()
            .find(|e| e.logical_id == self.logical_id)
            .ok_or_else(|| anyhow!("missing selected entry"))?;
        let pending = self
            .new
            .ledger
            .entries
            .iter()
            .find(|e| e.logical_id == self.logical_id)
            .and_then(|e| e.pending.as_ref())
            .ok_or_else(|| anyhow!("missing pending"))?;
        ensure!(
            entry.tombstone.is_none() && pending.intended_source == entry.source_ack,
            "prepare source basis changed"
        );
        match (entry.lifecycle, &entry.provider_ack, pending.kind) {
            (Lifecycle::Enrolled, None, OperationKind::Create) => {
                ensure!(pending.base_etag.is_none(), "create base etag");
            }
            (Lifecycle::Active, Some(prior), OperationKind::Update) => {
                ensure!(
                    pending.base_etag.as_ref() == Some(&prior.etag)
                        && pending.target_event_id == prior.event_id
                        && pending.desired_payload_hash.as_ref() != Some(&prior.last_payload_hash),
                    "update base mismatch or no-op"
                );
            }
            _ => return Err(anyhow!("prepare lifecycle mismatch")),
        }
        ensure!(
            self.old
                .ledger
                .entries
                .iter()
                .all(|e| e.logical_id != pending.operation_id
                    && e.tombstone.as_ref().and_then(|t| t.operation_id.as_ref())
                        != Some(&pending.operation_id)),
            "operation identity collision"
        );
        entry.pending = Some(pending.clone());
        ensure!(
            expected == self.new.ledger,
            "prepare changed unrelated evidence"
        );
        Ok(())
    }
}

// Move-only, no Serde, no public constructor. Only confirmed backend CAS code
// constructs this receipt. Neither observation nor local staging can construct it.
pub(super) struct FreshCasReceipt {
    publication: Box<PreparedPublication>,
}

enum PublicationOutcome {
    FreshWon(FreshCasReceipt),
    Rejected,
    Unknown,
}

mod sealed {
    pub(super) trait Sealed {}
}

trait AuthoritativeState: sealed::Sealed {
    fn observe(&mut self, operation: &source::SourceSession) -> Result<AuthoritativeSnapshot>;
    // A builder must verify actual commit/blob objects and the sole H0 parent.
    // None means no fresh stage in this invocation, including already_applied.
    fn stage(
        &mut self,
        invocation: &Invocation,
        old: &AuthoritativeSnapshot,
        operation: &source::SourceSession,
    ) -> Result<Option<PreparedPublication>>;
    fn publish(
        &mut self,
        publication: PreparedPublication,
        operation: &source::SourceSession,
    ) -> PublicationOutcome;
    fn publish_ack(
        &mut self,
        publication: &AckPublication,
        operation: &source::SourceSession,
    ) -> AckPublicationOutcome;
}

pub(super) struct PendingMetadata {
    head: String,
    logical_id: String,
    operation_id: String,
    phase: OperationPhase,
}

pub(super) enum InvocationOutcome {
    Ready {
        permit: Box<DispatchPermit>,
        intent: Box<PreparedFrozenIntent>,
        context: Box<ack::PinnedContext>,
    },
    RecoverOnly(Vec<PendingMetadata>),
    NotAcquired,
    PublicationUnknown,
}

fn invoke(
    backend: &mut impl AuthoritativeState,
    binding: &Binding,
    operation: &source::SourceSession,
) -> Result<InvocationOutcome> {
    operation.check()?;
    let invocation = Invocation::fresh()?;
    let observed = backend.observe(operation)?;
    operation.check()?;
    observed.validate(binding)?;
    let pending: Vec<_> = observed
        .ledger
        .entries
        .iter()
        .filter_map(|entry| {
            entry.pending.as_ref().map(|p| PendingMetadata {
                head: observed.head.clone(),
                logical_id: entry.logical_id.clone(),
                operation_id: p.operation_id.clone(),
                phase: p.phase,
            })
        })
        .collect();
    if !pending.is_empty() {
        return Ok(InvocationOutcome::RecoverOnly(pending));
    }
    let Some(publication) = backend.stage(&invocation, &observed, operation)? else {
        return Ok(InvocationOutcome::NotAcquired);
    };
    publication.validate(&invocation, &observed)?;
    operation.check()?;
    let receipt = match backend.publish(publication, operation) {
        PublicationOutcome::FreshWon(receipt) => receipt,
        PublicationOutcome::Rejected => return Ok(InvocationOutcome::NotAcquired),
        PublicationOutcome::Unknown => return Ok(InvocationOutcome::PublicationUnknown),
    };
    // A lost reply or cancellation after a possible CAS is never reacquisition.
    if operation.check().is_err() {
        return Ok(InvocationOutcome::PublicationUnknown);
    }
    let latest = match backend.observe(operation) {
        Ok(snapshot) if operation.check().is_ok() => snapshot,
        _ => return Ok(InvocationOutcome::PublicationUnknown),
    };
    let result = RemoteDispatchAuthority::from_fresh(receipt, invocation, &observed, latest);
    Ok(finish_acquisition(result, operation))
}

fn finish_acquisition(
    result: Result<InvocationOutcome>,
    operation: &source::SourceSession,
) -> InvocationOutcome {
    // Discard even a built permit if CPU validation outlived the root session.
    // Consumers must still check before HTTP: handoff is not atomic fencing or
    // revocation, and the consumed receipt cannot be retried or reacquired.
    if operation.check().is_err() {
        return InvocationOutcome::PublicationUnknown;
    }
    result.unwrap_or(InvocationOutcome::PublicationUnknown)
}

impl FreshCasReceipt {
    pub(super) fn mint(
        self,
        invocation: Invocation,
        observed: &AuthoritativeSnapshot,
        latest: AuthoritativeSnapshot,
    ) -> Result<InvocationOutcome> {
        let p = *self.publication;
        p.validate(&invocation, observed)?;
        latest.validate(&p.new.ledger.binding)?;
        ensure!(
            p.new.same_basis(&latest),
            "authoritative view changed before mint"
        );
        let mut intent = PreparedFrozenIntent::prepare(
            &p.new.ledger,
            &p.new.ledger.binding,
            &p.logical_id,
            &p.operation_id,
        )
        .map_err(|_| anyhow!("invalid intent"))?;
        let context = ack::PinnedContext::from_blob(
            LedgerLocation {
                // No local repository is claimed by this policy-only snapshot pin.
                repository: Default::default(),
                commit: p.new.head.clone(),
                path: p.new.path.clone(),
            },
            p.new.blob.clone(),
            p.new.ledger.clone(),
            serde_json::to_vec(&p.new.ledger)?,
            &p.logical_id,
            &p.operation_id,
        )?;
        context.attach(&mut intent)?;
        let permit = DispatchPermit {
            context_digest: intent.context_digest.clone(),
            ledger_head: Some(p.new.head),
            owner_nonce: Some(invocation.nonce),
            authority: Some(p.new.authority_id),
            binding: intent.binding.clone(),
            logical_id: intent.logical_id.clone(),
            operation_id: intent.operation_id.clone(),
            digest: intent.digest.clone(),
            kind: intent.kind,
            origin: p.new.origin,
        };
        Ok(InvocationOutcome::Ready {
            permit: Box::new(permit),
            intent: Box::new(intent),
            context: Box::new(context),
        })
    }
}

// Separate policy seam: no production constructor. A future builder must verify
// actual H1->H2 V3 ACK ownership/transition objects, not trust ApplyReceipt fields.
struct AckPublication {
    old: AuthoritativeSnapshot,
    new: AuthoritativeSnapshot,
    audit: Box<ack::AckAudit>,
}

impl AckPublication {
    fn validate(&self) -> Result<()> {
        self.old.validate(&self.old.ledger.binding)?;
        self.new.validate(&self.old.ledger.binding)?;
        ensure!(
            self.old.authority_id == self.new.authority_id
                && self.old.state_ref == self.new.state_ref
                && self.old.path == self.new.path
                && self.old.origin == self.new.origin
                && self.old.head != self.new.head
                && self.old.blob != self.new.blob
                && self.audit.base_commit == self.old.head
                && self.audit.path == self.old.path
                && self.audit.old_blob_id == self.old.blob,
            "ACK publication basis mismatch"
        );
        ack::validate_transition(
            &serde_json::to_vec(&self.old.ledger)?,
            &serde_json::to_vec(&self.new.ledger)?,
            &self.audit,
        )
    }
}

enum AckPublicationOutcome {
    Confirmed,
    Rejected,
    Unknown,
}
#[derive(Debug, PartialEq, Eq)]
enum Completion {
    Completed,
    AckPublicationPending,
    AckPublicationUnknown,
}

fn publish_ack(
    backend: &mut impl AuthoritativeState,
    publication: &AckPublication,
    operation: &source::SourceSession,
) -> Completion {
    if operation.check().is_err() || publication.validate().is_err() {
        return Completion::AckPublicationPending;
    }
    let outcome = backend.publish_ack(publication, operation);
    if operation.check().is_err() {
        return Completion::AckPublicationUnknown;
    }
    match outcome {
        AckPublicationOutcome::Confirmed => Completion::Completed,
        AckPublicationOutcome::Rejected => Completion::AckPublicationPending,
        AckPublicationOutcome::Unknown => Completion::AckPublicationUnknown,
    }
}

#[cfg(test)]
mod tests;
