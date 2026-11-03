//! Terminal evidence is admitted only from the private checked HTTP result.
//! Serializable audit metadata is never an authority or an operator-JSON route.
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ImmutableEndpointOrigin, PreparedFrozenIntent, SuccessCause, ValidatedSuccess, strong_etag,
};
use crate::{
    Binding, Ledger, LedgerChangeProof, LedgerEntry, LedgerLocation, Lifecycle, OperationKind,
    PendingOperation, PreparedLedgerChange, ProviderAck, StateOperation, ledger, operations,
    source, state_git,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AckOrigin {
    GoogleAuthenticated,
    #[cfg(test)]
    SyntheticFixture,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AckCause {
    MutationResponse,
    ReconciledObservation,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AckAudit {
    pub(crate) schema_version: String,
    pub(crate) binding: Binding,
    pub(crate) base_commit: String,
    pub(crate) path: String,
    pub(crate) old_blob_id: String,
    pub(crate) logical_id: String,
    pub(crate) operation_id: String,
    pub(crate) intent_digest: String,
    pub(crate) old_pending: PendingOperation,
    pub(crate) provider_ack: ProviderAck,
    pub(crate) cause: AckCause,
    pub(crate) origin: AckOrigin,
    pub(crate) evidence_digest: String,
}

// Frozen v1 encoding: domain, then u64-BE byte length and compact serde JSON of
// this explicitly ordered tuple. Binding/PendingOperation/ProviderAck retain their
// declared field order, null options and snake_case enum strings. No Value maps;
// evidence_digest itself is excluded. Changes require a new domain version.
pub(crate) fn evidence_digest(a: &AckAudit) -> Result<String> {
    digest(
        b"hive-calendar-ack-evidence:v1\0",
        &(
            &a.schema_version,
            &a.binding,
            &a.base_commit,
            &a.path,
            &a.old_blob_id,
            &a.logical_id,
            &a.operation_id,
            &a.intent_digest,
            &a.old_pending,
            &a.provider_ack,
            a.cause,
            a.origin,
        ),
    )
}

fn digest(domain: &[u8], value: &impl Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn oid(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn validate_audit(a: &AckAudit) -> Result<()> {
    ensure!(a.schema_version == "1", "unsupported ACK audit version");
    ensure!(
        matches!(a.base_commit.len(), 40 | 64)
            && oid(&a.base_commit, a.base_commit.len())
            && oid(&a.old_blob_id, a.base_commit.len()),
        "invalid ACK Git basis"
    );
    ensure!(
        a.path.len() <= 4096
            && a.path.to_ascii_lowercase().ends_with(".json")
            && !a.path.contains('\\')
            && !a.path.chars().any(char::is_control)
            && a.path.split('/').count() <= 64
            && a.path
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git")),
        "invalid ACK ledger path"
    );
    ensure!(
        a.old_pending.operation_id == a.operation_id && a.old_pending.kind != OperationKind::Delete,
        "invalid ACK operation"
    );
    ensure!(
        operations::intent_digest(&a.binding, &a.logical_id, &a.old_pending)? == a.intent_digest,
        "ACK intent digest mismatch"
    );
    operations::frozen_event(&a.old_pending)?;
    ensure!(
        a.provider_ack.event_id == a.old_pending.target_event_id
            && Some(&a.provider_ack.last_payload_hash)
                == a.old_pending.desired_payload_hash.as_ref(),
        "ACK result mismatch"
    );
    strong_etag(&a.provider_ack.etag).map_err(|_| anyhow!("invalid ACK strong etag"))?;
    if let Some(etag) = &a.old_pending.base_etag {
        strong_etag(etag).map_err(|_| anyhow!("invalid ACK base etag"))?;
    }
    // Strict ledger validation supplies canonical identities, event IDs, source
    // shapes and lifecycle constraints without reading any source repository.
    let update = a.old_pending.kind == OperationKind::Update;
    let ledger = Ledger {
        schema_version: "1".into(),
        binding: a.binding.clone(),
        source_watermark: a.old_pending.intended_source.commit.clone(),
        entries: vec![LedgerEntry {
            logical_id: a.logical_id.clone(),
            lifecycle: if update {
                Lifecycle::Active
            } else {
                Lifecycle::Enrolled
            },
            source_ack: a.old_pending.intended_source.clone(),
            provider_ack: update.then(|| ProviderAck {
                event_id: a.old_pending.target_event_id.clone(),
                etag: a.old_pending.base_etag.clone().unwrap_or_default(),
                last_payload_hash: a.provider_ack.last_payload_hash.clone(),
            }),
            pending: Some(a.old_pending.clone()),
            tombstone: None,
        }],
    };
    PreparedFrozenIntent::prepare(&ledger, &a.binding, &a.logical_id, &a.operation_id)
        .map_err(|_| anyhow!("invalid ACK frozen intent"))?;
    ensure!(
        evidence_digest(a)? == a.evidence_digest,
        "ACK evidence digest mismatch"
    );
    Ok(())
}

fn reduce(old: &Ledger, a: &AckAudit) -> Result<Ledger> {
    let mut next = old.clone();
    let entry = next
        .entries
        .iter_mut()
        .find(|e| e.logical_id == a.logical_id)
        .ok_or_else(|| anyhow!("ACK selected entry missing"))?;
    ensure!(
        entry.pending.as_ref() == Some(&a.old_pending),
        "ACK pending basis mismatch"
    );
    entry.lifecycle = Lifecycle::Active;
    entry.provider_ack = Some(a.provider_ack.clone());
    entry.pending = None;
    Ok(next)
}

pub(crate) fn validate_transition(old_bytes: &[u8], new_bytes: &[u8], a: &AckAudit) -> Result<()> {
    validate_audit(a)?;
    let old = ledger::decode(old_bytes, &a.binding)?;
    let new = ledger::decode(new_bytes, &a.binding)?;
    PreparedFrozenIntent::prepare(&old, &a.binding, &a.logical_id, &a.operation_id)
        .map_err(|_| anyhow!("invalid ACK old intent"))?;
    ensure!(
        new == reduce(&old, a)?,
        "ACK changed unrelated ledger evidence"
    );
    Ok(())
}

pub(crate) struct SealedAckProof {
    audit: Box<AckAudit>,
    seal: AckOrigin,
}

impl SealedAckProof {
    pub(crate) fn audit(&self) -> &AckAudit {
        &self.audit
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(self.seal == self.audit.origin, "ACK origin seal mismatch");
        validate_audit(&self.audit)
    }
}

#[cfg(test)]
pub(crate) fn fixture_proof(mut audit: AckAudit) -> Result<SealedAckProof> {
    audit.origin = AckOrigin::SyntheticFixture;
    audit.evidence_digest = evidence_digest(&audit)?;
    validate_audit(&audit)?;
    Ok(SealedAckProof {
        audit: Box::new(audit),
        seal: AckOrigin::SyntheticFixture,
    })
}

/// Immutable pre-HTTP basis. No notes, parser executable or historical snapshot.
pub(super) struct PinnedContext {
    location: LedgerLocation,
    old_blob_id: String,
    old_ledger: Ledger,
    old_bytes: Vec<u8>,
    logical_id: String,
    old_pending: PendingOperation,
    intent_digest: String,
    context_digest: String,
}

impl PinnedContext {
    pub(super) fn load(
        location: &LedgerLocation,
        binding: &Binding,
        logical: &str,
        operation_id: &str,
        operation: &source::SourceSession,
    ) -> Result<Self> {
        operation.check()?;
        let blob = source::read_json_blob_before(
            &location.repository,
            &location.commit,
            &location.path,
            Instant::now() + Duration::from_secs(120),
        );
        operation.check()?;
        let blob = blob?;
        let old = ledger::decode(&blob.content, binding)?;
        Self::from_blob(
            location.clone(),
            blob.object_id,
            old,
            blob.content,
            logical,
            operation_id,
        )
    }

    pub(super) fn from_blob(
        location: LedgerLocation,
        old_blob_id: String,
        old_ledger: Ledger,
        old_bytes: Vec<u8>,
        logical: &str,
        operation: &str,
    ) -> Result<Self> {
        let intent =
            PreparedFrozenIntent::prepare(&old_ledger, &old_ledger.binding, logical, operation)
                .map_err(|_| anyhow!("invalid pinned intent"))?;
        let old_pending = old_ledger
            .entries
            .iter()
            .find(|e| e.logical_id == logical)
            .and_then(|e| e.pending.clone())
            .ok_or_else(|| anyhow!("missing pinned intent"))?;
        let context_digest = digest(
            b"hive-calendar-ack-context:v1\0",
            &(
                &old_ledger.binding,
                &location.commit,
                &location.path,
                &old_blob_id,
                logical,
                operation,
                &intent.digest,
                &old_pending,
            ),
        )?;
        Ok(Self {
            location,
            old_blob_id,
            old_ledger,
            old_bytes,
            logical_id: logical.into(),
            old_pending,
            intent_digest: intent.digest,
            context_digest,
        })
    }

    pub(super) fn attach(&self, intent: &mut PreparedFrozenIntent) -> Result<()> {
        let expected = PreparedFrozenIntent::prepare(
            &self.old_ledger,
            &self.old_ledger.binding,
            &self.logical_id,
            &self.old_pending.operation_id,
        )
        .map_err(|_| anyhow!("invalid pinned intent"))?;
        ensure!(
            intent.binding == expected.binding
                && intent.logical_id == expected.logical_id
                && intent.operation_id == expected.operation_id
                && intent.digest == expected.digest
                && intent.phase == expected.phase
                && intent.kind == expected.kind
                && intent.event_id == expected.event_id
                && intent.desired_hash == expected.desired_hash
                && intent.prior == expected.prior
                && ledger::payload_hash(&intent.event) == expected.desired_hash
                && intent
                    .context_digest
                    .as_ref()
                    .is_none_or(|d| d == &self.context_digest),
            "pinned intent mismatch"
        );
        intent.context_digest = Some(self.context_digest.clone());
        intent.ledger_basis = Some((
            self.location.commit.clone(),
            self.location.path.clone(),
            self.old_blob_id.clone(),
        ));
        Ok(())
    }

    fn check_result_basis(&self, success: &ValidatedSuccess) -> Result<()> {
        ensure!(
            success.context_digest.as_ref() == Some(&self.context_digest)
                && success.binding == self.old_ledger.binding
                && success.logical_id == self.logical_id
                && success.operation_id == self.old_pending.operation_id
                && success.intent_digest == self.intent_digest
                && success.event_id == self.old_pending.target_event_id
                && Some(&success.payload_hash) == self.old_pending.desired_payload_hash.as_ref(),
            "ACK pinned result mismatch"
        );
        strong_etag(&success.etag).map_err(|_| anyhow!("invalid ACK strong etag"))?;
        Ok(())
    }

    pub(super) fn admit(self, success: ValidatedSuccess) -> Result<PinnedAckTransaction> {
        self.check_result_basis(&success)?;
        ensure!(
            success.origin == ImmutableEndpointOrigin::Google,
            "ACK requires Google endpoint origin"
        );
        let mut audit = AckAudit {
            schema_version: "1".into(),
            binding: success.binding,
            base_commit: self.location.commit.clone(),
            path: self.location.path.clone(),
            old_blob_id: self.old_blob_id.clone(),
            logical_id: success.logical_id,
            operation_id: success.operation_id,
            intent_digest: success.intent_digest,
            old_pending: self.old_pending.clone(),
            provider_ack: ProviderAck {
                event_id: success.event_id,
                etag: success.etag,
                last_payload_hash: success.payload_hash,
            },
            cause: match success.cause {
                SuccessCause::MutationResponse => AckCause::MutationResponse,
                SuccessCause::ReconciledObservation => AckCause::ReconciledObservation,
            },
            origin: AckOrigin::GoogleAuthenticated,
            evidence_digest: String::new(),
        };
        audit.evidence_digest = evidence_digest(&audit)?;
        validate_audit(&audit)?;
        Ok(PinnedAckTransaction {
            context: self,
            proof: SealedAckProof {
                audit: Box::new(audit),
                seal: AckOrigin::GoogleAuthenticated,
            },
        })
    }
}

/// The original basis and sealed observation survive every verification/CAS error.
pub(super) struct PinnedAckTransaction {
    context: PinnedContext,
    proof: SealedAckProof,
}

#[cfg(test)]
pub(super) fn fixture_context(old: Ledger) -> PinnedContext {
    let bytes = serde_json::to_vec(&old).unwrap();
    PinnedContext::from_blob(
        LedgerLocation {
            repository: "never-read".into(),
            commit: "e".repeat(40),
            path: "ledger.json".into(),
        },
        "f".repeat(40),
        old,
        bytes,
        super::tests::LOGICAL,
        super::tests::OPERATION,
    )
    .unwrap()
}

impl PinnedAckTransaction {
    fn prepared(&self) -> Result<PreparedLedgerChange> {
        self.proof.validate()?;
        let a = self.proof.audit();
        let candidate_ledger = serde_json::to_vec(&reduce(&self.context.old_ledger, a)?)?;
        validate_transition(&self.context.old_bytes, &candidate_ledger, a)?;
        // Only this child can re-seal the same immutable admitted observation. The
        // proof type itself is not Clone and no audit-to-production-proof API exists.
        let proof = SealedAckProof {
            audit: Box::new(a.clone()),
            seal: self.proof.seal,
        };
        Ok(PreparedLedgerChange {
            candidate_sha256: a
                .evidence_digest
                .strip_prefix("sha256:")
                .ok_or_else(|| anyhow!("invalid ACK digest"))?
                .into(),
            operation: StateOperation::Acknowledge,
            logical_id: a.logical_id.clone(),
            source_commit: a.old_pending.intended_source.commit.clone(),
            binding: a.binding.clone(),
            parser_change_accepted: false,
            base_commit: a.base_commit.clone(),
            path: a.path.clone(),
            expected_blob_id: Some(a.old_blob_id.clone()),
            candidate_ledger,
            proof: LedgerChangeProof::Acknowledgement(proof),
        })
    }

    pub(super) fn verify(&self, operation: &source::SourceSession) -> Result<crate::ApplyReceipt> {
        operation.check()?;
        state_git::verify(
            &self.context.location.repository,
            "refs/heads/hive-calendar-state",
            &self.prepared()?,
            false,
            Instant::now() + Duration::from_secs(120),
            operation,
        )
    }

    pub(super) fn apply(&self, operation: &source::SourceSession) -> Result<crate::ApplyReceipt> {
        operation.check()?;
        state_git::apply(
            &self.context.location.repository,
            "refs/heads/hive-calendar-state",
            &self.prepared()?,
            false,
            Instant::now() + Duration::from_secs(120),
            operation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OperationPhase;

    fn audit(c: &PinnedContext) -> AckAudit {
        let mut a = AckAudit {
            schema_version: "1".into(),
            binding: c.old_ledger.binding.clone(),
            base_commit: c.location.commit.clone(),
            path: c.location.path.clone(),
            old_blob_id: c.old_blob_id.clone(),
            logical_id: c.logical_id.clone(),
            operation_id: c.old_pending.operation_id.clone(),
            intent_digest: c.intent_digest.clone(),
            old_pending: c.old_pending.clone(),
            provider_ack: ProviderAck {
                event_id: c.old_pending.target_event_id.clone(),
                etag: "\"new\"".into(),
                last_payload_hash: c.old_pending.desired_payload_hash.clone().unwrap(),
            },
            cause: AckCause::ReconciledObservation,
            origin: AckOrigin::SyntheticFixture,
            evidence_digest: String::new(),
        };
        a.evidence_digest = evidence_digest(&a).unwrap();
        a
    }

    #[test]
    fn fixture_reducer_preserves_current_source_and_all_other_evidence() {
        for update in [false, true] {
            for phase in [OperationPhase::Prepared, OperationPhase::Uncertain] {
                let mut old = super::super::tests::fixture(update);
                old.entries[0].pending.as_mut().unwrap().phase = phase;
                old.entries[0].source_ack.expected.text = "Current source, not intended".into();
                old.entries[0].source_ack.parser_sha256 = "9".repeat(64);
                let mut other = old.entries[0].clone();
                other.logical_id = "33333333-3333-4333-8333-333333333333".into();
                other.source_ack.path = "other.md".into();
                other.pending = None;
                other.lifecycle = Lifecycle::Enrolled;
                other.provider_ack = None;
                old.entries.push(other);
                let c = fixture_context(old.clone());
                let a = audit(&c);
                let new = reduce(&old, &a).unwrap();
                let bytes = serde_json::to_vec(&new).unwrap();
                validate_transition(&c.old_bytes, &bytes, &a).unwrap();
                assert_eq!(new.entries[0].source_ack, old.entries[0].source_ack);
                assert_ne!(new.entries[0].source_ack, a.old_pending.intended_source);
                assert_eq!(new.entries[1], old.entries[1]);
                assert_eq!(new.source_watermark, old.source_watermark);
                for mutation in 0..6 {
                    let mut bad = new.clone();
                    match mutation {
                        0 => bad.entries[0].source_ack = a.old_pending.intended_source.clone(),
                        1 => bad.source_watermark = "0".repeat(40),
                        2 => bad.entries.reverse(),
                        3 => bad.entries[1].source_ack.expected.text.push('!'),
                        4 => bad.entries[0].pending = Some(a.old_pending.clone()),
                        _ => {
                            bad.entries[0].provider_ack.as_mut().unwrap().etag = "\"other\"".into()
                        }
                    }
                    assert!(
                        validate_transition(&c.old_bytes, &serde_json::to_vec(&bad).unwrap(), &a)
                            .is_err()
                    );
                }
                let mut changed_phase = old.clone();
                changed_phase.entries[0].pending.as_mut().unwrap().phase = match phase {
                    OperationPhase::Prepared => OperationPhase::Uncertain,
                    OperationPhase::Uncertain => OperationPhase::Prepared,
                };
                assert!(
                    validate_transition(&serde_json::to_vec(&changed_phase).unwrap(), &bytes, &a)
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn fixture_audit_mutations_and_metadata_cannot_supply_authority() {
        let c = fixture_context(super::super::tests::fixture(false));
        let a = audit(&c);
        let decoded: AckAudit =
            ledger::decode_unique_json(&serde_json::to_vec(&a).unwrap()).unwrap();
        assert_eq!(decoded, a); // Metadata only; no Deserialize on SealedAckProof.
        for mutation in 0..14 {
            let mut bad = a.clone();
            match mutation {
                0 => bad.logical_id = "INVALID".into(),
                1 => bad.operation_id = "INVALID".into(),
                2 => bad.intent_digest = format!("sha256:{}", "0".repeat(64)),
                3 => bad.provider_ack.etag = "W/\"weak\"".into(),
                4 => bad.provider_ack.event_id = "other0".into(),
                5 => bad.provider_ack.last_payload_hash = format!("sha256:{}", "0".repeat(64)),
                6 => bad.old_pending.kind = OperationKind::Delete,
                7 => bad.old_pending.intended_source.expected.text.push('!'),
                8 => bad.old_pending.target_event_id = "other0".into(),
                9 => bad.old_pending.base_etag = Some("\"unexpected\"".into()),
                10 => bad.base_commit = "A".repeat(40),
                11 => bad.old_blob_id = "g".repeat(40),
                12 => bad.path = "../ledger.json".into(),
                _ => bad.schema_version = "2".into(),
            }
            bad.evidence_digest = evidence_digest(&bad).unwrap();
            assert!(validate_audit(&bad).is_err(), "mutation {mutation}");
        }
        let mut claimed = a;
        claimed.origin = AckOrigin::GoogleAuthenticated;
        let proof = fixture_proof(claimed).unwrap();
        assert_eq!(proof.audit().origin, AckOrigin::SyntheticFixture);
        proof.validate().unwrap();
        let mut forged_metadata = proof.audit().clone();
        forged_metadata.origin = AckOrigin::GoogleAuthenticated;
        forged_metadata.evidence_digest = evidence_digest(&forged_metadata).unwrap();
        assert!(
            SealedAckProof {
                audit: Box::new(forged_metadata),
                seal: AckOrigin::SyntheticFixture
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn pinned_attachment_binds_base_blob_path_and_full_pending_phase() {
        let old = super::super::tests::fixture(false);
        let c = fixture_context(old.clone());
        let mut intent = PreparedFrozenIntent::prepare(
            &old,
            &old.binding,
            &c.logical_id,
            &c.old_pending.operation_id,
        )
        .unwrap();
        assert!(intent.context_digest.is_none());
        c.attach(&mut intent).unwrap();
        for mutation in 0..6 {
            let mut other = fixture_context(old.clone());
            match mutation {
                0 => other.location.commit = "0".repeat(40),
                1 => other.location.path = "other.json".into(),
                2 => other.old_blob_id = "0".repeat(40),
                3 => {
                    other.old_ledger.entries[0].pending.as_mut().unwrap().phase =
                        OperationPhase::Uncertain
                }
                4 => other.old_ledger.binding.calendar_id = "other@example.test".into(),
                _ => other.old_ledger.entries[0]
                    .pending
                    .as_mut()
                    .unwrap()
                    .intended_source
                    .expected
                    .text
                    .push('!'),
            }
            let bytes = serde_json::to_vec(&other.old_ledger).unwrap();
            let rebuilt = PinnedContext::from_blob(
                other.location,
                other.old_blob_id,
                other.old_ledger,
                bytes,
                &c.logical_id,
                &c.old_pending.operation_id,
            );
            if let Ok(other) = rebuilt {
                assert!(other.attach(&mut intent).is_err());
            }
        }
    }

    #[test]
    fn synthetic_transaction_rebuilds_identical_change_without_reading_latest_or_source() {
        let c = fixture_context(super::super::tests::fixture(true));
        let proof = fixture_proof(audit(&c)).unwrap();
        let tx = PinnedAckTransaction { context: c, proof };
        let first = tx.prepared().unwrap();
        let second = tx.prepared().unwrap();
        assert_eq!(first.base_commit, second.base_commit);
        assert_eq!(first.expected_blob_id, second.expected_blob_id);
        assert_eq!(first.candidate_sha256, second.candidate_sha256);
        assert_eq!(first.candidate_ledger, second.candidate_ledger);
        assert!(
            ledger::decode(&first.candidate_ledger, &first.binding)
                .unwrap()
                .entries[0]
                .pending
                .is_none()
        );
        let operation =
            source::SourceSession::operation(Instant::now() + Duration::from_secs(120)).unwrap();
        operation.cancel();
        assert!(tx.verify(&operation).is_err());
        assert!(tx.apply(&operation).is_err());
        assert_eq!(
            first.candidate_ledger,
            tx.prepared().unwrap().candidate_ledger
        );
    }

    #[test]
    fn evidence_encoding_v1_golden_and_phase_cause_origin_binding() {
        let c = fixture_context(super::super::tests::fixture(false));
        let a = audit(&c);
        // Independently computed using Node crypto, u64-BE lengths and ordered JSON.
        assert_eq!(
            a.evidence_digest,
            "sha256:6de81d3eb27626ccc246f3ec214ba3b38bf885507b22fa21d119d4044be74d89"
        );
        for mutation in 0..8 {
            let mut other = a.clone();
            match mutation {
                0 => other.base_commit = "0".repeat(40),
                1 => other.path = "other.json".into(),
                2 => other.old_blob_id = "0".repeat(40),
                3 => other.old_pending.phase = OperationPhase::Uncertain,
                4 => other.cause = AckCause::MutationResponse,
                5 => other.origin = AckOrigin::GoogleAuthenticated,
                6 => other.binding.calendar_id = "other@example.test".into(),
                _ => other.provider_ack.etag = "\"other\"".into(),
            }
            assert_ne!(evidence_digest(&other).unwrap(), a.evidence_digest);
        }
        let mut other = a.clone();
        other.evidence_digest.clear();
        assert_eq!(evidence_digest(&other).unwrap(), a.evidence_digest);
    }

    #[test]
    fn matching_loopback_metadata_is_not_authority_and_mismatches_are_rejected() {
        for mutation in 0..9 {
            let c = fixture_context(super::super::tests::fixture(false));
            let a = audit(&c);
            let mut success = ValidatedSuccess {
                context_digest: Some(c.context_digest.clone()),
                binding: a.binding.clone(),
                logical_id: a.logical_id,
                operation_id: a.operation_id,
                intent_digest: a.intent_digest,
                event_id: a.provider_ack.event_id,
                etag: a.provider_ack.etag,
                payload_hash: a.provider_ack.last_payload_hash,
                cause: SuccessCause::ReconciledObservation,
                origin: ImmutableEndpointOrigin::Loopback(12345),
            };
            c.check_result_basis(&success).unwrap();
            match mutation {
                0 => success.context_digest = None,
                1 => success.context_digest = Some(format!("sha256:{}", "0".repeat(64))),
                2 => success.binding.calendar_id.push('!'),
                3 => success.logical_id.push('!'),
                4 => success.operation_id.push('!'),
                5 => success.intent_digest.push('!'),
                6 => success.event_id.push('!'),
                7 => success.payload_hash.push('!'),
                _ => success.etag = "W/\"weak\"".into(),
            }
            assert!(c.check_result_basis(&success).is_err());
            assert!(c.admit(success).is_err());
        }
    }

    #[test]
    fn committed_pin_needs_only_ledger_objects_not_notes_or_parser() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("state.git");
        let output = std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("HOME", temp.path())
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["init", "--bare", "--object-format=sha1"])
            .arg(&repo)
            .output()
            .unwrap();
        assert!(output.status.success());
        let deadline = Instant::now() + Duration::from_secs(120);
        let git = source::GitSession::before(&repo, deadline).unwrap();
        let old = super::super::tests::fixture(false);
        let bytes = serde_json::to_vec(&old).unwrap();
        let blob = String::from_utf8(
            git.run(&["hash-object", "-w", "--stdin"], &bytes, 1024)
                .unwrap(),
        )
        .unwrap();
        let tree = String::from_utf8(
            git.run(
                &["mktree"],
                format!("100644 blob {}\tledger.json\n", blob.trim()).as_bytes(),
                1024,
            )
            .unwrap(),
        )
        .unwrap();
        let commit = String::from_utf8(
            git.run(
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@localhost",
                    "commit-tree",
                    tree.trim(),
                ],
                b"fixture ledger only\n",
                1024,
            )
            .unwrap(),
        )
        .unwrap();
        let location = LedgerLocation {
            repository: repo,
            commit: commit.trim().into(),
            path: "ledger.json".into(),
        };
        let root = source::SourceSession::operation(deadline).unwrap();
        let c = PinnedContext::load(
            &location,
            &old.binding,
            super::super::tests::LOGICAL,
            super::super::tests::OPERATION,
            &root,
        )
        .unwrap();
        assert_eq!(c.old_blob_id, blob.trim());
        let proof = fixture_proof(audit(&c)).unwrap();
        let tx = PinnedAckTransaction { context: c, proof };
        assert!(tx.prepared().is_ok());
        let mut wrong_binding = old.binding.clone();
        wrong_binding.calendar_id = "other@example.test".into();
        assert!(
            PinnedContext::load(
                &location,
                &wrong_binding,
                super::super::tests::LOGICAL,
                super::super::tests::OPERATION,
                &root
            )
            .is_err()
        );
        let mut wrong_base = location.clone();
        wrong_base.commit = "0".repeat(40);
        assert!(
            PinnedContext::load(
                &wrong_base,
                &old.binding,
                super::super::tests::LOGICAL,
                super::super::tests::OPERATION,
                &root
            )
            .is_err()
        );
        root.cancel();
        assert!(tx.verify(&root).is_err());
        assert!(tx.apply(&root).is_err());
        assert!(tx.prepared().is_ok());
    }
}
