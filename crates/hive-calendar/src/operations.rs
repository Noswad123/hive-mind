//! Frozen provider intents and explicitly unverified, non-persistable simulations.
//! No transport or authenticated-success constructor exists in this module.
use std::{
    fs::OpenOptions,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ApplyOptions, Binding, EntrySelection, Ledger, LedgerLocation, Lifecycle, OperationKind,
    OperationPhase, PendingOperation, PrepareOptions, PreparedLedgerChange, PreviewOptions,
    ProviderAck, ProviderOperationProof, SimulationOptions, SourceTask, StateOperation,
    TaskMetadata, UncertainOptions, ledger,
    planner::{self, PlanningContext},
    projection, source,
};

const OBSERVATION_LIMIT: usize = 1024 * 1024;
const CANDIDATE_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationBase {
    pub commit: String,
    pub path: String,
    #[serde(rename = "gitBlobID")]
    pub git_blob_id: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationTransition {
    Prepare,
    MarkUncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationCandidate {
    pub schema_version: String,
    pub kind: String,
    pub operation: OperationTransition,
    pub logical_id: String,
    pub operation_id: String,
    pub source_commit: String,
    pub parser_sha256: String,
    pub base: OperationBase,
    pub candidate_ledger: Ledger,
    pub intent_digest: String,
    pub publication_ready: bool,
    pub remote_verified: bool,
    pub ledger_head_verified: bool,
    pub requires_ledger_compare_and_swap: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservationPayload {
    pub summary: String,
    pub start_date: String,
    pub end_date: String,
    pub transparency: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorObservation {
    pub schema_version: String,
    pub provenance: String,
    pub binding: Binding,
    pub logical_id: String,
    pub operation_id: String,
    pub intent_digest: String,
    pub event_id: String,
    pub etag: String,
    pub managed_logical_id: String,
    pub payload: ObservationPayload,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AckSimulation {
    pub schema_version: String,
    pub kind: String,
    pub simulation_only: bool,
    pub persistence_allowed: bool,
    pub remote_verified: bool,
    pub publication_ready: bool,
    pub ledger_head_verified: bool,
    pub base: OperationBase,
    pub logical_id: String,
    pub operation_id: String,
    pub intent_digest: String,
    pub observation_sha256: String,
    pub observation: OperatorObservation,
    pub hypothetical_ledger: Ledger,
    pub diagnostics: Vec<String>,
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        && value.bytes().any(|b| b != b'0' && b != b'-')
}

fn before(deadline: Instant, operation: &source::SourceSession) -> Result<()> {
    operation.check()?;
    ensure!(
        Instant::now() < deadline,
        "calendar operation deadline exceeded"
    );
    Ok(())
}

fn field(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value.as_bytes());
}

fn optional_field(hash: &mut Sha256, value: Option<&str>) {
    hash.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        field(hash, value);
    }
}

fn binding_fields(hash: &mut Sha256, binding: &Binding) {
    for value in [
        &binding.provider,
        &binding.account_id,
        &binding.calendar_id,
        &binding.source_repo_id,
    ] {
        field(hash, value);
    }
}

/// A deterministic reservation, NOT proof of absence, authentication or ownership.
/// SHA-256(domain || length-prefixed provider/account/calendar/repo/logical ID),
/// rendered as `h1` followed by lowercase hex (a subset of Google's base32hex).
pub fn reserved_event_id(binding: &Binding, logical_id: &str) -> Result<String> {
    ledger::validate_binding(binding)?;
    ensure!(uuid(logical_id), "invalid operation logical identity");
    let mut hash = Sha256::new();
    hash.update(b"hive-calendar-event:v1\0");
    binding_fields(&mut hash, binding);
    field(&mut hash, logical_id);
    Ok(format!("h1{:x}", hash.finalize()))
}

/// Frozen v1 binary encoding, independent of JSON member order and current notes.
/// After the domain: binding's provider/accountId/calendarId/sourceRepoId, logicalId,
/// kind (create/update/delete), operationId; then intendedSource's commit,
/// parserSha256, path, sourceId, line, mwId, gitBlobID, sourceRevision, stableId,
/// expected's text, literalText, done, due, raw, start, estimate; finally targetEventId,
/// baseEtag, desiredPayloadHash. Strings are u64 big-endian UTF-8 byte length + bytes;
/// line is u64 big-endian; booleans are one byte (0/1); optional strings have a 0/1
/// presence byte before any string. CRLF bytes are retained. Phase is excluded.
pub fn intent_digest(
    binding: &Binding,
    logical_id: &str,
    pending: &PendingOperation,
) -> Result<String> {
    ledger::validate_binding(binding)?;
    ensure!(
        uuid(logical_id) && uuid(&pending.operation_id),
        "invalid operation identity"
    );
    ledger::validate_source(
        &pending.intended_source,
        pending.intended_source.commit.len(),
    )?;
    ensure!(
        matches!(pending.intended_source.commit.len(), 40 | 64),
        "invalid operation source commit"
    );
    let mut hash = Sha256::new();
    hash.update(b"hive-calendar-provider-intent:v1\0");
    binding_fields(&mut hash, binding);
    field(&mut hash, logical_id);
    field(
        &mut hash,
        match pending.kind {
            OperationKind::Create => "create",
            OperationKind::Update => "update",
            OperationKind::Delete => "delete",
        },
    );
    field(&mut hash, &pending.operation_id);
    let ack = &pending.intended_source;
    for value in [&ack.commit, &ack.parser_sha256, &ack.path, &ack.source_id] {
        field(&mut hash, value);
    }
    hash.update((ack.line as u64).to_be_bytes());
    for value in [&ack.mw_id, &ack.git_blob_id, &ack.source_revision] {
        field(&mut hash, value);
    }
    optional_field(&mut hash, ack.stable_id.as_deref());
    field(&mut hash, &ack.expected.text);
    field(&mut hash, &ack.expected.literal_text);
    hash.update([u8::from(ack.expected.done)]);
    for value in [
        &ack.expected.due,
        &ack.expected.raw,
        &ack.expected.start,
        &ack.expected.estimate,
        &pending.target_event_id,
    ] {
        field(&mut hash, value);
    }
    optional_field(&mut hash, pending.base_etag.as_deref());
    optional_field(&mut hash, pending.desired_payload_hash.as_deref());
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn selected(ledger: &Ledger, selection: &EntrySelection) -> Result<usize> {
    let mut matches = ledger
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| match selection {
            EntrySelection::LogicalId(id) => entry.logical_id == *id,
            EntrySelection::AcknowledgedLocation { path, line } => {
                entry.source_ack.path == *path && entry.source_ack.line == *line
            }
        });
    let (index, _) = matches
        .next()
        .ok_or_else(|| anyhow!("operation entry not found"))?;
    ensure!(
        matches.next().is_none(),
        "ambiguous operation entry selection"
    );
    Ok(index)
}

fn fresh_id(
    ledger: &Ledger,
    mut entropy: impl FnMut(&mut [u8]) -> Result<()>,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<String> {
    for _ in 0..16 {
        check()?;
        let mut bytes = [0u8; 16];
        entropy(&mut bytes)?;
        check()?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let mut id = String::with_capacity(36);
        for (index, byte) in bytes.iter().enumerate() {
            use std::fmt::Write;
            if [4, 6, 8, 10].contains(&index) {
                id.push('-');
            }
            write!(id, "{byte:02x}").expect("String write");
        }
        if ledger.entries.iter().all(|entry| {
            entry.logical_id != id
                && entry.pending.as_ref().is_none_or(|p| p.operation_id != id)
                && entry
                    .tombstone
                    .as_ref()
                    .and_then(|t| t.operation_id.as_ref())
                    != Some(&id)
        }) {
            return Ok(id);
        }
    }
    Err(anyhow!("operation identity allocation failed"))
}

fn canonical(ledger: &Ledger, binding: &Binding) -> Result<Vec<u8>> {
    let bytes =
        serde_json::to_vec(ledger).map_err(|_| anyhow!("operation ledger encoding failed"))?;
    ledger::decode(&bytes, binding)?;
    Ok(bytes)
}

fn finish(
    ledger: Ledger,
    base: OperationBase,
    index: usize,
    transition: OperationTransition,
    source_commit: String,
    parser_sha256: String,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<OperationCandidate> {
    check()?;
    canonical(&ledger, &ledger.binding)?;
    check()?;
    let entry = &ledger.entries[index];
    let pending = entry
        .pending
        .as_ref()
        .ok_or_else(|| anyhow!("operation pending intent missing"))?;
    let digest = intent_digest(&ledger.binding, &entry.logical_id, pending)?;
    check()?;
    let candidate = OperationCandidate {
        schema_version: "2".into(),
        kind: "calendar-operation-candidate".into(),
        operation: transition,
        logical_id: entry.logical_id.clone(),
        operation_id: pending.operation_id.clone(),
        source_commit,
        parser_sha256,
        base,
        intent_digest: digest,
        candidate_ledger: ledger,
        publication_ready: false,
        remote_verified: false,
        ledger_head_verified: false,
        requires_ledger_compare_and_swap: true,
        diagnostics: vec![
            "candidate_only_not_persisted".into(),
            "remote_not_verified".into(),
            "ledger_head_not_verified".into(),
            "ledger_compare_and_swap_required".into(),
            "executor_not_implemented".into(),
        ],
    };
    check()?;
    Ok(candidate)
}

fn prepare_context(
    context: PlanningContext,
    selection: &EntrySelection,
    entropy: impl FnMut(&mut [u8]) -> Result<()>,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<OperationCandidate> {
    check()?;
    let PlanningContext {
        target,
        ledger,
        basis,
    } = context;
    let mut ledger = ledger.ok_or_else(|| anyhow!("committed operation ledger required"))?;
    let basis = basis.ok_or_else(|| anyhow!("committed operation ledger basis required"))?;
    canonical(&ledger, &ledger.binding)?;
    check()?;
    ensure!(
        ledger.entries.iter().all(|entry| entry.pending.is_none()),
        "operation reconciliation required: pending intent exists"
    );
    let index = selected(&ledger, selection)?;
    let entry = &ledger.entries[index];
    ensure!(
        entry.lifecycle != Lifecycle::Retired,
        "retired operation entry cannot be prepared"
    );
    let plan = planner::build_plan(&target, &ledger.binding, Some(&ledger), None)?;
    check()?;
    ensure!(
        plan.entries
            .iter()
            .find(|e| e.logical_id == entry.logical_id)
            .is_some_and(|e| e.current_source.is_some()),
        "operation requires exact current source acknowledgement and parser basis"
    );
    let ack = &entry.source_ack;
    let task = target
        .items
        .iter()
        .find(|task| task.source_path == ack.path && task.line == ack.line && task.id == ack.mw_id)
        .ok_or_else(|| anyhow!("operation source missing"))?;
    let evidence = projection::due_evidence(task)?;
    ensure!(
        !task.done && evidence.diagnostic.is_none(),
        "operation requires open source with valid due"
    );
    let event = evidence
        .event
        .ok_or_else(|| anyhow!("operation requires open source with valid due"))?;
    let hash = ledger::payload_hash(&event);
    let (kind, target_event_id, base_etag) = match entry.lifecycle {
        Lifecycle::Enrolled => (
            OperationKind::Create,
            reserved_event_id(&ledger.binding, &entry.logical_id)?,
            None,
        ),
        Lifecycle::Active => {
            let provider = entry
                .provider_ack
                .as_ref()
                .ok_or_else(|| anyhow!("operation provider basis missing"))?;
            ensure!(
                provider.last_payload_hash != hash,
                "operation is a no-op: managed payload matches provider acknowledgement"
            );
            (
                OperationKind::Update,
                provider.event_id.clone(),
                Some(provider.etag.clone()),
            )
        }
        Lifecycle::Retired => unreachable!("checked above"),
    };
    let intended_source = entry.source_ack.clone();
    check()?;
    let operation_id = fresh_id(&ledger, entropy, check)?;
    ledger.entries[index].pending = Some(PendingOperation {
        operation_id,
        kind,
        phase: OperationPhase::Prepared,
        intended_source,
        target_event_id,
        base_etag,
        desired_payload_hash: Some(hash),
    });
    finish(
        ledger,
        OperationBase {
            commit: basis.commit,
            path: basis.path,
            git_blob_id: basis.git_blob_id,
        },
        index,
        OperationTransition::Prepare,
        target.commit,
        target.parser_sha256,
        check,
    )
}

pub(crate) fn prepare(options: &PrepareOptions) -> Result<OperationCandidate> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let operation = source::SourceSession::operation(deadline)?;
    let mut check = || before(deadline, &operation);
    check()?;
    let context = planner::load_context(
        &options.source,
        &options.binding,
        Some(&options.ledger),
        deadline,
    );
    check()?;
    prepare_context(
        context?,
        &options.entry,
        |bytes| getrandom::fill(bytes).map_err(|_| anyhow!("operation entropy unavailable")),
        &mut check,
    )
}

// Frozen metadata consistency only. This does NOT replay historical graph acks or
// authenticate remote evidence. Recovery must not depend on today's notes or MW.
pub(crate) fn frozen_event(pending: &PendingOperation) -> Result<crate::ProspectiveEvent> {
    ensure!(
        pending.kind != OperationKind::Delete,
        "legacy delete intent is read-only and unsupported"
    );
    let ack = &pending.intended_source;
    let expected = &ack.expected;
    let task = SourceTask {
        id: ack.mw_id.clone(),
        stable_id: ack.stable_id.clone(),
        identity_kind: String::new(),
        source_revision: ack.source_revision.clone(),
        source_id: ack.source_id.clone(),
        source_path: ack.path.clone(),
        line: ack.line,
        text: expected.text.clone(),
        literal_text: expected.literal_text.clone(),
        done: expected.done,
        metadata: TaskMetadata {
            due: expected.due.clone(),
            raw: expected.raw.clone(),
            start: expected.start.clone(),
            estimate: expected.estimate.clone(),
        },
    };
    let evidence = projection::due_evidence(&task)?;
    ensure!(
        !task.done && evidence.diagnostic.is_none(),
        "invalid frozen operation source payload"
    );
    let event = evidence
        .event
        .ok_or_else(|| anyhow!("invalid frozen operation source payload"))?;
    ensure!(
        pending.desired_payload_hash.as_deref() == Some(ledger::payload_hash(&event).as_str()),
        "frozen operation payload digest mismatch"
    );
    Ok(event)
}

fn load_frozen(
    location: &LedgerLocation,
    binding: &Binding,
    deadline: Instant,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<(Ledger, OperationBase)> {
    check()?;
    ledger::validate_binding(binding)?;
    let blob = source::read_json_blob_before(
        &location.repository,
        &location.commit,
        &location.path,
        deadline,
    );
    check()?;
    let blob = blob?;
    let ledger = ledger::decode(&blob.content, binding)?;
    check()?;
    Ok((
        ledger,
        OperationBase {
            commit: location.commit.clone(),
            path: location.path.clone(),
            git_blob_id: blob.object_id,
        },
    ))
}

fn matching_pending<'a>(
    ledger: &'a Ledger,
    index: usize,
    operation_id: &str,
) -> Result<&'a PendingOperation> {
    let entry = &ledger.entries[index];
    let pending = entry
        .pending
        .as_ref()
        .ok_or_else(|| anyhow!("operation pending intent missing"))?;
    ensure!(
        pending.operation_id == operation_id,
        "operation identity mismatch"
    );
    frozen_event(pending)?;
    if pending.kind == OperationKind::Create {
        ensure!(
            pending.target_event_id == reserved_event_id(&ledger.binding, &entry.logical_id)?,
            "operation reserved event identity mismatch"
        );
    }
    Ok(pending)
}

fn uncertain_context(
    mut ledger: Ledger,
    base: OperationBase,
    selection: &EntrySelection,
    operation_id: &str,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<OperationCandidate> {
    check()?;
    canonical(&ledger, &ledger.binding)?;
    check()?;
    let index = selected(&ledger, selection)?;
    let pending = matching_pending(&ledger, index, operation_id)?;
    ensure!(
        pending.phase == OperationPhase::Prepared,
        "operation already uncertain: no transition"
    );
    let source_commit = pending.intended_source.commit.clone();
    let parser_sha256 = pending.intended_source.parser_sha256.clone();
    check()?;
    ledger.entries[index]
        .pending
        .as_mut()
        .expect("checked pending")
        .phase = OperationPhase::Uncertain;
    finish(
        ledger,
        base,
        index,
        OperationTransition::MarkUncertain,
        source_commit,
        parser_sha256,
        check,
    )
}

pub(crate) fn mark_uncertain(options: &UncertainOptions) -> Result<OperationCandidate> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let operation = source::SourceSession::operation(deadline)?;
    let mut check = || before(deadline, &operation);
    let (ledger, base) = load_frozen(&options.ledger, &options.binding, deadline, &mut check)?;
    uncertain_context(
        ledger,
        base,
        &options.entry,
        &options.operation_id,
        &mut check,
    )
}

fn approved_uuid(id: &str) -> Result<[u8; 16]> {
    ensure!(
        uuid(id) && id.as_bytes()[14] == b'4' && b"89ab".contains(&id.as_bytes()[19]),
        "invalid approved operation identity"
    );
    let mut digits = id.bytes().filter(|b| *b != b'-');
    let nibble = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
    let mut bytes = [0; 16];
    for byte in &mut bytes {
        *byte = nibble(digits.next().expect("validated UUID")) * 16
            + nibble(digits.next().expect("validated UUID"));
    }
    Ok(bytes)
}

fn validate_envelope(
    candidate: &OperationCandidate,
    options: &ApplyOptions,
    digest: &str,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    check()?;
    ensure!(
        hex(digest, 64) && digest == options.expected_candidate_sha256,
        "operation candidate digest mismatch"
    );
    ensure!(
        candidate.schema_version == "2" && candidate.kind == "calendar-operation-candidate",
        "unsupported operation candidate envelope"
    );
    ensure!(
        !candidate.publication_ready
            && !candidate.remote_verified
            && !candidate.ledger_head_verified
            && candidate.requires_ledger_compare_and_swap,
        "invalid operation candidate safety flags"
    );
    ensure!(
        !options.create_ref && !options.accept_parser_change,
        "operation cannot initialize state or accept parser changes"
    );
    ensure!(
        candidate.source_commit == options.source.commit,
        "operation candidate source mismatch"
    );
    ensure!(
        candidate.base.path == options.ledger_path,
        "operation candidate ledger path mismatch"
    );
    ensure!(
        matches!(candidate.base.commit.len(), 40 | 64)
            && hex(&candidate.base.commit, candidate.base.commit.len())
            && hex(&candidate.base.git_blob_id, candidate.base.commit.len()),
        "invalid operation candidate base"
    );
    ensure!(
        hex(&candidate.parser_sha256, 64),
        "invalid operation candidate parser digest"
    );
    ensure!(
        uuid(&candidate.logical_id) && uuid(&candidate.operation_id),
        "invalid operation candidate identity"
    );
    check()?;
    canonical(&candidate.candidate_ledger, &options.binding)?;
    check()?;
    ensure!(
        serde_json::to_vec(candidate)
            .map_err(|_| anyhow!("operation candidate encoding failed"))?
            .len()
            <= CANDIDATE_LIMIT,
        "operation candidate limit exceeded"
    );
    check()
}

fn handoff(
    candidate: &OperationCandidate,
    replayed: OperationCandidate,
    options: &ApplyOptions,
    digest: &str,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<PreparedLedgerChange> {
    check()?;
    ensure!(
        candidate == &replayed,
        "operation candidate replay mismatch"
    );
    check()?;
    let bytes = canonical(&replayed.candidate_ledger, &options.binding)?;
    check()?;
    let change = PreparedLedgerChange {
        candidate_sha256: digest.into(),
        operation: match replayed.operation {
            OperationTransition::Prepare => StateOperation::Prepare,
            OperationTransition::MarkUncertain => StateOperation::MarkUncertain,
        },
        logical_id: replayed.logical_id,
        source_commit: replayed.source_commit,
        binding: options.binding.clone(),
        parser_change_accepted: false,
        base_commit: replayed.base.commit,
        path: replayed.base.path,
        expected_blob_id: Some(replayed.base.git_blob_id),
        candidate_ledger: bytes,
        proof: crate::LedgerChangeProof::Intent(ProviderOperationProof {
            operation_id: replayed.operation_id,
            intent_digest: replayed.intent_digest,
        }),
    };
    check()?;
    Ok(change)
}

pub(crate) fn validate_candidate(
    candidate: &OperationCandidate,
    options: &ApplyOptions,
    candidate_sha256: &str,
    deadline: Instant,
    operation: &source::SourceSession,
) -> Result<PreparedLedgerChange> {
    let mut check = || before(deadline, operation);
    validate_envelope(candidate, options, candidate_sha256, &mut check)?;
    let location = LedgerLocation {
        repository: options.ledger_repository.clone(),
        commit: candidate.base.commit.clone(),
        path: options.ledger_path.clone(),
    };
    let selection = EntrySelection::LogicalId(candidate.logical_id.clone());
    check()?;
    let replayed = match candidate.operation {
        OperationTransition::Prepare => {
            let options = PrepareOptions {
                source: PreviewOptions {
                    repository: options.source.repository.clone(),
                    commit: options.source.commit.clone(),
                    mw_binary: options.source.mw_binary.clone(),
                },
                binding: options.binding.clone(),
                ledger: location,
                entry: selection,
            };
            let approved = approved_uuid(&candidate.operation_id)?;
            let context = planner::load_context(
                &options.source,
                &options.binding,
                Some(&options.ledger),
                deadline,
            );
            check()?;
            let context = context?;
            ensure!(
                context
                    .basis
                    .as_ref()
                    .is_some_and(|b| b.git_blob_id == candidate.base.git_blob_id),
                "operation candidate ledger blob mismatch"
            );
            prepare_context(
                context,
                &options.entry,
                |bytes| {
                    bytes.copy_from_slice(&approved);
                    Ok(())
                },
                &mut check,
            )?
        }
        OperationTransition::MarkUncertain => {
            let (ledger, base) = load_frozen(&location, &options.binding, deadline, &mut check)?;
            ensure!(
                base == candidate.base,
                "operation candidate ledger blob mismatch"
            );
            uncertain_context(
                ledger,
                base,
                &selection,
                &candidate.operation_id,
                &mut check,
            )?
        }
    };
    handoff(candidate, replayed, options, candidate_sha256, &mut check)
}

fn capture_observation(
    path: &Path,
    expected: &str,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<(OperatorObservation, String)> {
    check()?;
    ensure!(hex(expected, 64), "invalid approved observation digest");
    ensure!(path.is_absolute(), "observation file must be absolute");
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    anyhow::bail!("observation capture unsupported on this platform");
    check()?;
    let mut file = options
        .open(path)
        .map_err(|_| anyhow!("observation file unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| anyhow!("observation file unavailable"))?;
    ensure!(metadata.is_file(), "observation file must be regular");
    ensure!(
        metadata.len() <= OBSERVATION_LIMIT as u64,
        "observation file limit exceeded"
    );
    let mut bytes = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        check()?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| anyhow!("observation file read failed"))?;
        if count == 0 {
            break;
        }
        ensure!(
            bytes.len() + count <= OBSERVATION_LIMIT,
            "observation file limit exceeded"
        );
        bytes.extend_from_slice(&buffer[..count]);
    }
    check()?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    ensure!(digest == expected, "observation digest mismatch");
    let observation = ledger::decode_unique_json(&bytes)
        .map_err(|_| anyhow!("invalid observation JSON or schema"))?;
    check()?;
    Ok((observation, digest))
}

fn simulate_context(
    ledger: Ledger,
    base: OperationBase,
    selection: &EntrySelection,
    operation_id: &str,
    observation: OperatorObservation,
    observation_sha256: String,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<AckSimulation> {
    check()?;
    canonical(&ledger, &ledger.binding)?;
    check()?;
    let index = selected(&ledger, selection)?;
    let entry = &ledger.entries[index];
    let pending = matching_pending(&ledger, index, operation_id)?;
    let digest = intent_digest(&ledger.binding, &entry.logical_id, pending)?;
    ensure!(
        observation.schema_version == "1" && observation.provenance == "operator_asserted",
        "observation must be an unverified operator assertion"
    );
    ensure!(
        observation.binding == ledger.binding
            && observation.logical_id == entry.logical_id
            && observation.managed_logical_id == entry.logical_id
            && observation.operation_id == pending.operation_id
            && observation.intent_digest == digest
            && observation.event_id == pending.target_event_id,
        "observation frozen intent mismatch"
    );
    ensure!(
        !observation.etag.trim().is_empty()
            && observation.etag.len() <= 1024
            && !observation.etag.chars().any(char::is_control),
        "invalid observation etag"
    );
    let frozen = frozen_event(pending)?;
    ensure!(
        observation.payload.summary == frozen.summary
            && observation.payload.start_date == frozen.start.date
            && observation.payload.end_date == frozen.end.date
            && observation.payload.transparency == "transparent",
        "observation managed payload mismatch"
    );
    let payload_hash = ledger::payload_hash(&crate::ProspectiveEvent {
        summary: observation.payload.summary.clone(),
        start: crate::CalendarDate {
            date: observation.payload.start_date.clone(),
        },
        end: crate::CalendarDate {
            date: observation.payload.end_date.clone(),
        },
        availability: "Available",
        transparency: "transparent",
    });
    ensure!(
        pending.desired_payload_hash.as_ref() == Some(&payload_hash),
        "observation payload digest mismatch"
    );
    check()?;
    let logical_id = entry.logical_id.clone();
    let operation_id = pending.operation_id.clone();
    // This reducer's result is ONLY hypothetical. Operator JSON is never evidence
    // for a durable provider ACK; future transport needs an authenticated boundary.
    let mut hypothetical_ledger = ledger;
    let entry = &mut hypothetical_ledger.entries[index];
    entry.lifecycle = Lifecycle::Active;
    entry.provider_ack = Some(ProviderAck {
        event_id: observation.event_id.clone(),
        etag: observation.etag.clone(),
        last_payload_hash: payload_hash,
    });
    entry.pending = None;
    canonical(&hypothetical_ledger, &hypothetical_ledger.binding)?;
    check()?;
    let result = AckSimulation {
        schema_version: "2".into(),
        kind: "calendar-provider-simulation".into(),
        simulation_only: true,
        persistence_allowed: false,
        remote_verified: false,
        publication_ready: false,
        ledger_head_verified: false,
        base,
        logical_id,
        operation_id,
        intent_digest: digest,
        observation_sha256,
        observation,
        hypothetical_ledger,
        diagnostics: vec![
            "operator_assertion_not_authentication".into(),
            "simulation_only_not_persisted".into(),
            "remote_not_verified".into(),
        ],
    };
    check()?;
    Ok(result)
}

pub(crate) fn simulate_ack(options: &SimulationOptions) -> Result<AckSimulation> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let operation = source::SourceSession::operation(deadline)?;
    let mut check = || before(deadline, &operation);
    check()?;
    let (observation, digest) = capture_observation(
        &options.observation_path,
        &options.expected_observation_sha256,
        &mut check,
    )?;
    let (ledger, base) = load_frozen(&options.ledger, &options.binding, deadline, &mut check)?;
    simulate_context(
        ledger,
        base,
        &options.entry,
        &options.operation_id,
        observation,
        digest,
        &mut check,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommittedSnapshot, LedgerEntry, RemoteDisposition, SourceBlob, Tombstone};
    use serde_json::json;
    use std::{collections::BTreeMap, fs, path::PathBuf, process::Command};

    const LOGICAL_ID: &str = "11111111-1111-4111-8111-111111111111";
    const OPERATION_ID: &str = "12121212-1212-4212-9212-121212121212";

    fn binding() -> Binding {
        Binding {
            provider: "google_calendar".into(),
            account_id: format!("google-sub-v1:{}", "a".repeat(64)),
            calendar_id: "calendar@example.test".into(),
            source_repo_id: "operation-fixture".into(),
        }
    }

    fn base() -> OperationBase {
        OperationBase {
            commit: "f".repeat(40),
            path: "calendar.json".into(),
            git_blob_id: "e".repeat(40),
        }
    }

    fn selection() -> EntrySelection {
        EntrySelection::LogicalId(LOGICAL_ID.into())
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(120)
    }

    fn target() -> CommittedSnapshot {
        let literal_text = "- [ ] Ship due:2026-11-01\r".to_owned();
        CommittedSnapshot {
            commit: "2".repeat(40),
            parser_sha256: "a".repeat(64),
            files: BTreeMap::from([(
                "tasks.md".into(),
                SourceBlob {
                    object_id: "b".repeat(40),
                    content: format!("{literal_text}\n").into_bytes(),
                },
            )]),
            items: vec![SourceTask {
                id: "tasks:1:0".into(),
                stable_id: None,
                identity_kind: "positional".into(),
                source_revision: format!("sha256:{}", "c".repeat(64)),
                source_id: "tasks".into(),
                source_path: "tasks.md".into(),
                line: 1,
                text: "Ship due:2026-11-01".into(),
                literal_text,
                done: false,
                metadata: TaskMetadata {
                    due: "2026-11-01".into(),
                    raw: String::new(),
                    start: String::new(),
                    estimate: String::new(),
                },
            }],
        }
    }

    fn original(active: bool) -> Ledger {
        let target = target();
        let mut source_ack = planner::observation(&target, &target.items[0]).unwrap();
        source_ack.commit = "1".repeat(40);
        Ledger {
            schema_version: "1".into(),
            binding: binding(),
            source_watermark: "1".repeat(40),
            entries: vec![LedgerEntry {
                logical_id: LOGICAL_ID.into(),
                lifecycle: if active {
                    Lifecycle::Active
                } else {
                    Lifecycle::Enrolled
                },
                source_ack,
                provider_ack: active.then(|| ProviderAck {
                    event_id: "event0".into(),
                    etag: "\"old\"".into(),
                    last_payload_hash: format!("sha256:{}", "d".repeat(64)),
                }),
                pending: None,
                tombstone: None,
            }],
        }
    }

    fn context(ledger: Ledger) -> PlanningContext {
        let base = base();
        PlanningContext {
            target: target(),
            ledger: Some(ledger),
            basis: Some(planner::LedgerBasis {
                commit: base.commit,
                path: base.path,
                git_blob_id: base.git_blob_id,
            }),
        }
    }

    fn entropy(bytes: &mut [u8]) -> Result<()> {
        bytes.fill(0x12);
        Ok(())
    }

    fn prepared(active: bool) -> OperationCandidate {
        prepare_context(
            context(original(active)),
            &selection(),
            entropy,
            &mut || Ok(()),
        )
        .unwrap()
    }

    fn uncertain(candidate: &OperationCandidate) -> OperationCandidate {
        uncertain_context(
            candidate.candidate_ledger.clone(),
            candidate.base.clone(),
            &selection(),
            &candidate.operation_id,
            &mut || Ok(()),
        )
        .unwrap()
    }

    fn observation(candidate: &OperationCandidate) -> OperatorObservation {
        let pending = candidate.candidate_ledger.entries[0]
            .pending
            .as_ref()
            .unwrap();
        let event = frozen_event(pending).unwrap();
        OperatorObservation {
            schema_version: "1".into(),
            provenance: "operator_asserted".into(),
            binding: binding(),
            logical_id: LOGICAL_ID.into(),
            operation_id: candidate.operation_id.clone(),
            intent_digest: candidate.intent_digest.clone(),
            event_id: pending.target_event_id.clone(),
            etag: "\"observed-version\"".into(),
            managed_logical_id: LOGICAL_ID.into(),
            payload: ObservationPayload {
                summary: event.summary,
                start_date: event.start.date,
                end_date: event.end.date,
                transparency: "transparent".into(),
            },
        }
    }

    fn simulation(
        candidate: &OperationCandidate,
        observation: OperatorObservation,
    ) -> Result<AckSimulation> {
        simulate_context(
            candidate.candidate_ledger.clone(),
            candidate.base.clone(),
            &selection(),
            &candidate.operation_id,
            observation,
            "a".repeat(64),
            &mut || Ok(()),
        )
    }

    fn apply_options(candidate: &OperationCandidate) -> ApplyOptions {
        ApplyOptions {
            source: PreviewOptions {
                repository: "/unavailable/source".into(),
                commit: candidate.source_commit.clone(),
                mw_binary: "/unavailable/mw".into(),
            },
            binding: binding(),
            ledger_repository: "/unavailable/ledger".into(),
            ledger_path: base().path,
            state_ref: "refs/hive/calendar/test".into(),
            candidate_path: "/unavailable/candidate".into(),
            expected_candidate_sha256: "a".repeat(64),
            create_ref: false,
            accept_parser_change: false,
        }
    }

    fn add_unselected(ledger: &mut Ledger) {
        let mut entry = ledger.entries[0].clone();
        entry.logical_id = "33333333-3333-4333-8333-333333333333".into();
        entry.source_ack.path = "other.md".into();
        entry.lifecycle = Lifecycle::Retired;
        entry.provider_ack = Some(ProviderAck {
            event_id: "other0".into(),
            etag: "unchanged".into(),
            last_payload_hash: format!("sha256:{}", "d".repeat(64)),
        });
        entry.pending = None;
        entry.tombstone = Some(Tombstone {
            reason: "retained".into(),
            source_commit: "1".repeat(40),
            operation_id: Some("44444444-4444-4444-8444-444444444444".into()),
            remote_disposition: RemoteDisposition::Retained,
        });
        ledger.entries.push(entry);
    }

    #[test]
    fn event_id_independent_golden_and_namespace_changes() {
        // Computed independently with Python hashlib and struct.pack(">Q", len).
        let golden = "h156c7016bae17c927369fe4b4281a47a6f9cdb793c9d26964dfa1cd4e55d64663";
        assert_eq!(reserved_event_id(&binding(), LOGICAL_ID).unwrap(), golden);
        assert!(
            golden
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'v').contains(&b))
        );
        for key in ["accountId", "calendarId", "sourceRepoId"] {
            let mut value = serde_json::to_value(binding()).unwrap();
            value[key] = match key {
                "accountId" => json!(format!("google-sub-v1:{}", "b".repeat(64))),
                "calendarId" => json!("other@example.test"),
                _ => json!("other-repo"),
            };
            let other: Binding = serde_json::from_value(value).unwrap();
            assert_ne!(reserved_event_id(&other, LOGICAL_ID).unwrap(), golden);
        }
        assert_ne!(reserved_event_id(&binding(), OPERATION_ID).unwrap(), golden);
        let mut bad = binding();
        bad.provider = "not-google".into();
        assert!(reserved_event_id(&bad, LOGICAL_ID).is_err());
        for bad in [
            "not-a-uuid",
            "00000000-0000-0000-0000-000000000000",
            "AAAAAAAA-1111-4111-8111-111111111111",
        ] {
            assert!(reserved_event_id(&binding(), bad).is_err());
        }
        let mut ledger = original(false);
        ledger.entries[0].source_ack.expected.text = "Different title and date".into();
        assert_eq!(
            reserved_event_id(&ledger.binding, &ledger.entries[0].logical_id).unwrap(),
            golden
        );
    }

    #[test]
    fn intent_digest_independent_golden_and_phase_invariance() {
        let candidate = prepared(false);
        assert_eq!(candidate.operation_id, OPERATION_ID);
        assert_eq!(
            candidate.intent_digest,
            "sha256:de276606f74e8b6f65c22d60d5c5c31faf8e2d2ca8a020b2d94f25d566713ffd"
        );
        assert_eq!(candidate.intent_digest, uncertain(&candidate).intent_digest);
        let pending = candidate.candidate_ledger.entries[0]
            .pending
            .as_ref()
            .unwrap();
        let mut lf = pending.clone();
        lf.intended_source.expected.literal_text.pop();
        assert_ne!(
            intent_digest(&binding(), LOGICAL_ID, &lf).unwrap(),
            candidate.intent_digest
        );
        let mut reordered = serde_json::to_value(pending).unwrap();
        let object = reordered.as_object_mut().unwrap();
        let old = object.remove("intendedSource").unwrap();
        object.insert("intendedSource".into(), old);
        let decoded: PendingOperation =
            ledger::decode_unique_json(&serde_json::to_vec(&reordered).unwrap()).unwrap();
        assert_eq!(
            intent_digest(&binding(), LOGICAL_ID, &decoded).unwrap(),
            candidate.intent_digest
        );
    }

    #[test]
    fn digest_binds_all_immutable_intent_fields() {
        let candidate = prepared(true);
        let pending = candidate.candidate_ledger.entries[0]
            .pending
            .as_ref()
            .unwrap();
        let value = serde_json::to_value(pending).unwrap();
        for (pointer, replacement) in [
            ("/kind", json!("create")),
            ("/operationId", json!(LOGICAL_ID)),
            ("/targetEventId", json!("other0")),
            ("/baseEtag", json!(null)),
            ("/desiredPayloadHash", json!(null)),
            ("/intendedSource/commit", json!("3".repeat(40))),
            ("/intendedSource/parserSha256", json!("f".repeat(64))),
            ("/intendedSource/path", json!("other.md")),
            ("/intendedSource/sourceId", json!("another")),
            ("/intendedSource/line", json!(2)),
            ("/intendedSource/gitBlobID", json!("f".repeat(40))),
            (
                "/intendedSource/sourceRevision",
                json!(format!("sha256:{}", "f".repeat(64))),
            ),
            ("/intendedSource/expected/text", json!("changed")),
            (
                "/intendedSource/expected/literalText",
                json!("- [ ] changed"),
            ),
            ("/intendedSource/expected/done", json!(true)),
            ("/intendedSource/expected/due", json!("2026-11-02")),
            ("/intendedSource/expected/raw", json!("changed")),
            ("/intendedSource/expected/start", json!("changed")),
            ("/intendedSource/expected/estimate", json!("changed")),
        ] {
            let mut changed = value.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            // Keep dependent positional identity valid when altering its components.
            if pointer.ends_with("/sourceId") {
                changed["intendedSource"]["mwId"] = json!("another:1:0");
            }
            if pointer.ends_with("/line") {
                changed["intendedSource"]["mwId"] = json!("tasks:2:0");
            }
            let changed: PendingOperation = serde_json::from_value(changed).unwrap();
            assert_ne!(
                intent_digest(&binding(), LOGICAL_ID, &changed).unwrap(),
                candidate.intent_digest,
                "{pointer}"
            );
        }
        let mut changed = pending.clone();
        changed.intended_source.stable_id = Some("e".repeat(32));
        changed.intended_source.mw_id = format!("task:{}", "e".repeat(32));
        assert_ne!(
            intent_digest(&binding(), LOGICAL_ID, &changed).unwrap(),
            candidate.intent_digest
        );
        let mut other_binding = binding();
        other_binding.source_repo_id.push('2');
        assert_ne!(
            intent_digest(&other_binding, LOGICAL_ID, pending).unwrap(),
            candidate.intent_digest
        );
        assert_ne!(
            intent_digest(&binding(), OPERATION_ID, pending).unwrap(),
            candidate.intent_digest
        );
    }

    #[test]
    fn uuid_v4_bits_os_entropy_fixed_replay_and_bounded_collisions() {
        let mut check = || Ok(());
        let ledger = original(false);
        for _ in 0..8 {
            let id = fresh_id(
                &ledger,
                |b| getrandom::fill(b).map_err(|_| anyhow!("test entropy")),
                &mut check,
            )
            .unwrap();
            assert_eq!(id.as_bytes()[14], b'4');
            assert!(b"89ab".contains(&id.as_bytes()[19]));
            assert_eq!(
                fresh_id(
                    &ledger,
                    |b| {
                        b.copy_from_slice(&approved_uuid(&id)?);
                        Ok(())
                    },
                    &mut check
                )
                .unwrap(),
                id
            );
        }
        assert!(approved_uuid("00000000-0000-0000-0000-000000000001").is_err());
        let mut collision = ledger.clone();
        collision.entries[0].logical_id = OPERATION_ID.into();
        let mut count = 0;
        assert!(
            fresh_id(
                &collision,
                |b| {
                    count += 1;
                    entropy(b)
                },
                &mut check
            )
            .is_err()
        );
        assert_eq!(count, 16);
        let mut collision = prepared(false).candidate_ledger;
        assert!(fresh_id(&collision, entropy, &mut check).is_err());
        collision.entries[0].pending = None;
        collision.entries[0].tombstone = Some(Tombstone {
            reason: "test".into(),
            source_commit: "1".repeat(40),
            operation_id: Some(OPERATION_ID.into()),
            remote_disposition: RemoteDisposition::Unknown,
        });
        assert!(fresh_id(&collision, entropy, &mut check).is_err());
        assert!(fresh_id(&ledger, |_| Err(anyhow!("entropy unavailable")), &mut check).is_err());
        let mut count = 0;
        let id = fresh_id(
            &ledger,
            |bytes| {
                count += 1;
                if count == 1 {
                    bytes.copy_from_slice(&approved_uuid(LOGICAL_ID)?);
                } else {
                    bytes.fill(0x12);
                }
                Ok(())
            },
            &mut check,
        )
        .unwrap();
        assert_eq!(id, OPERATION_ID);
        assert_eq!(count, 2);
    }

    #[test]
    fn prepare_only_adds_selected_pending_and_preserves_order_and_watermark() {
        for active in [false, true] {
            let mut original = original(active);
            add_unselected(&mut original);
            let candidate = prepare_context(
                context(original.clone()),
                &selection(),
                entropy,
                &mut || Ok(()),
            )
            .unwrap();
            let mut restored = candidate.candidate_ledger.clone();
            restored.entries[0].pending = None;
            assert_eq!(restored, original);
            let pending = candidate.candidate_ledger.entries[0]
                .pending
                .as_ref()
                .unwrap();
            assert_eq!(pending.intended_source, original.entries[0].source_ack);
            assert_eq!(pending.phase, OperationPhase::Prepared);
            assert_eq!(
                pending.kind,
                if active {
                    OperationKind::Update
                } else {
                    OperationKind::Create
                }
            );
            assert_eq!(pending.base_etag, active.then(|| "\"old\"".into()));
            assert_eq!(candidate.source_commit, "2".repeat(40));
            assert_eq!(candidate.candidate_ledger.source_watermark, "1".repeat(40));
            assert_eq!(candidate.base, base());
            assert!(
                !candidate.publication_ready
                    && !candidate.remote_verified
                    && !candidate.ledger_head_verified
            );
            assert!(candidate.requires_ledger_compare_and_swap);
        }
    }

    #[test]
    fn prepare_selection_uses_logical_id_or_unique_old_ack_location() {
        let candidate = prepare_context(
            context(original(false)),
            &EntrySelection::AcknowledgedLocation {
                path: "tasks.md".into(),
                line: 1,
            },
            entropy,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(candidate, prepared(false));
        assert!(
            selected(
                &original(false),
                &EntrySelection::LogicalId(OPERATION_ID.into())
            )
            .is_err()
        );
        let mut ledger = original(false);
        add_unselected(&mut ledger);
        ledger.entries[1].source_ack.path = "tasks.md".into();
        assert!(
            selected(
                &ledger,
                &EntrySelection::AcknowledgedLocation {
                    path: "tasks.md".into(),
                    line: 1
                }
            )
            .is_err()
        );
    }

    #[test]
    fn prepare_refuses_noop_pending_retired_and_missing_committed_basis() {
        let mut noop = original(true);
        noop.entries[0]
            .provider_ack
            .as_mut()
            .unwrap()
            .last_payload_hash = ledger::payload_hash(
            &projection::due_evidence(&target().items[0])
                .unwrap()
                .event
                .unwrap(),
        );
        assert!(
            prepare_context(context(noop), &selection(), entropy, &mut || Ok(()))
                .unwrap_err()
                .to_string()
                .contains("no-op")
        );
        let pending = prepared(false).candidate_ledger;
        assert!(prepare_context(context(pending), &selection(), entropy, &mut || Ok(())).is_err());
        let mut other_pending = original(false);
        add_unselected(&mut other_pending);
        let mut other = prepared(true).candidate_ledger.entries.remove(0);
        other.logical_id = "55555555-5555-4555-8555-555555555555".into();
        other.source_ack.path = "pending.md".into();
        other.pending.as_mut().unwrap().intended_source.path = "pending.md".into();
        other_pending.entries.push(other);
        assert!(
            prepare_context(
                context(other_pending),
                &selection(),
                entropy,
                &mut || Ok(())
            )
            .unwrap_err()
            .to_string()
            .contains("pending intent")
        );
        let mut retired = original(false);
        add_unselected(&mut retired);
        let retired_id = retired.entries[1].logical_id.clone();
        assert!(
            prepare_context(
                context(retired),
                &EntrySelection::LogicalId(retired_id),
                entropy,
                &mut || Ok(())
            )
            .unwrap_err()
            .to_string()
            .contains("retired")
        );
        let mut missing = context(original(false));
        missing.basis = None;
        assert!(prepare_context(missing, &selection(), entropy, &mut || Ok(())).is_err());
        let mut missing = context(original(false));
        missing.ledger = None;
        assert!(prepare_context(missing, &selection(), entropy, &mut || Ok(())).is_err());
    }

    #[test]
    fn prepare_never_auto_reacks_changed_source_or_parser() {
        for change in 0..7 {
            let mut ctx = context(original(false));
            match change {
                0 => ctx.target.parser_sha256 = "f".repeat(64),
                1 => ctx.target.items[0].text.push('!'),
                2 => ctx.target.items[0].source_path = "moved.md".into(),
                3 => ctx.target.items.clear(),
                4 => ctx.target.files.get_mut("tasks.md").unwrap().object_id = "f".repeat(40),
                5 => ctx.target.items[0].metadata.due = "2026-11-02".into(),
                _ => ctx.target.items[0].done = true,
            }
            assert!(
                prepare_context(ctx, &selection(), entropy, &mut || Ok(())).is_err(),
                "{change}"
            );
        }
    }

    #[test]
    fn prepare_requires_open_due_evidence_even_when_exact_ack_is_current() {
        for change in 0..5 {
            let mut ctx = context(original(false));
            let task = &mut ctx.target.items[0];
            match change {
                0 => task.done = true,
                1 => {
                    task.literal_text = "- [ ] Ship".into();
                    task.metadata.due.clear();
                }
                2 => {
                    task.literal_text = "- [ ] Ship due:2026-02-30".into();
                    task.metadata.due = "2026-02-30".into();
                }
                3 => task.metadata.raw = "due:2026-11-01".into(),
                _ => {
                    task.literal_text = "- [ ] Ship due:9999-12-31".into();
                    task.metadata.due = "9999-12-31".into();
                }
            }
            ctx.ledger.as_mut().unwrap().entries[0].source_ack =
                planner::observation(&ctx.target, &ctx.target.items[0]).unwrap();
            assert!(prepare_context(ctx, &selection(), entropy, &mut || Ok(())).is_err());
        }
    }

    #[test]
    fn uncertain_only_changes_phase_uses_frozen_root_metadata_and_is_not_idempotent() {
        for active in [false, true] {
            let mut candidate = prepared(active);
            add_unselected(&mut candidate.candidate_ledger);
            let marked = uncertain(&candidate);
            let mut restored = marked.candidate_ledger.clone();
            restored.entries[0].pending.as_mut().unwrap().phase = OperationPhase::Prepared;
            assert_eq!(restored, candidate.candidate_ledger);
            assert_eq!(marked.source_commit, "1".repeat(40));
            assert_ne!(marked.source_commit, candidate.source_commit);
            assert_eq!(marked.parser_sha256, candidate.parser_sha256);
            assert_eq!(marked.intent_digest, candidate.intent_digest);
            assert!(
                uncertain_context(
                    marked.candidate_ledger,
                    marked.base,
                    &selection(),
                    OPERATION_ID,
                    &mut || Ok(())
                )
                .unwrap_err()
                .to_string()
                .contains("no transition")
            );
            assert!(
                uncertain_context(
                    candidate.candidate_ledger,
                    candidate.base,
                    &selection(),
                    LOGICAL_ID,
                    &mut || Ok(())
                )
                .is_err()
            );
        }
    }

    #[test]
    fn frozen_recovery_refuses_delete_corrupt_payload_and_wrong_create_target() {
        let candidate = prepared(false);
        for change in 0..3 {
            let mut ledger = candidate.candidate_ledger.clone();
            let pending = ledger.entries[0].pending.as_mut().unwrap();
            match change {
                0 => pending.desired_payload_hash = Some(format!("sha256:{}", "f".repeat(64))),
                1 => pending.target_event_id = "other0".into(),
                _ => pending.intended_source.expected.done = true,
            }
            assert!(
                uncertain_context(ledger, base(), &selection(), OPERATION_ID, &mut || Ok(()))
                    .is_err()
            );
        }
        let mut legacy = prepared(true).candidate_ledger;
        let pending = legacy.entries[0].pending.as_mut().unwrap();
        pending.kind = OperationKind::Delete;
        pending.desired_payload_hash = None;
        assert!(
            canonical(&legacy, &binding()).is_ok(),
            "V1 decoder stays compatible"
        );
        assert!(
            uncertain_context(legacy, base(), &selection(), OPERATION_ID, &mut || Ok(()))
                .unwrap_err()
                .to_string()
                .contains("read-only")
        );
    }

    #[test]
    fn strict_owned_shapes_reject_unknown_fields_and_duplicate_keys_deeply() {
        let candidate = prepared(true);
        let candidate_value = serde_json::to_value(&candidate).unwrap();
        for pointer in [
            "",
            "/base",
            "/candidateLedger",
            "/candidateLedger/binding",
            "/candidateLedger/entries/0",
            "/candidateLedger/entries/0/sourceAck",
            "/candidateLedger/entries/0/sourceAck/expected",
            "/candidateLedger/entries/0/providerAck",
            "/candidateLedger/entries/0/pending",
            "/candidateLedger/entries/0/pending/intendedSource",
            "/candidateLedger/entries/0/pending/intendedSource/expected",
        ] {
            let mut bad = candidate_value.clone();
            bad.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("private-unknown".into(), json!("private-value"));
            let error = ledger::decode_unique_json::<OperationCandidate>(
                &serde_json::to_vec(&bad).unwrap(),
            )
            .unwrap_err()
            .to_string();
            assert!(!error.contains("private"));
        }
        let observation_value = serde_json::to_value(observation(&candidate)).unwrap();
        for pointer in ["", "/binding", "/payload"] {
            let mut bad = observation_value.clone();
            bad.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("remoteVerified".into(), json!(true));
            assert!(
                ledger::decode_unique_json::<OperatorObservation>(
                    &serde_json::to_vec(&bad).unwrap()
                )
                .is_err()
            );
        }
        let bytes = serde_json::to_string(&candidate).unwrap();
        for key in ["operationId", "gitBlobID", "text", "provider", "phase"] {
            let needle = format!("\"{key}\":");
            let duplicate = bytes.replacen(&needle, &format!("\"{key}\":null,{needle}"), 1);
            assert!(
                ledger::decode_unique_json::<OperationCandidate>(duplicate.as_bytes()).is_err()
            );
        }
        for version in [json!(2), json!("1"), json!("02")] {
            let mut bad = candidate_value.clone();
            bad["schemaVersion"] = version;
            if let Ok(decoded) =
                ledger::decode_unique_json::<OperationCandidate>(&serde_json::to_vec(&bad).unwrap())
            {
                assert!(
                    validate_envelope(
                        &decoded,
                        &apply_options(&candidate),
                        &"a".repeat(64),
                        &mut || Ok(())
                    )
                    .is_err()
                );
            }
        }
        assert_eq!(candidate_value["candidateLedger"]["schemaVersion"], "1");
        assert_eq!(candidate_value["schemaVersion"], "2");
    }

    #[test]
    fn complete_envelope_replay_rejects_ledger_and_metadata_tampering() {
        let mut original = original(true);
        add_unselected(&mut original);
        let candidate = prepare_context(
            context(original.clone()),
            &selection(),
            entropy,
            &mut || Ok(()),
        )
        .unwrap();
        let options = apply_options(&candidate);
        let value = serde_json::to_value(&candidate).unwrap();
        for (pointer, replacement) in [
            ("/sourceCommit", json!("3".repeat(40))),
            ("/parserSha256", json!("f".repeat(64))),
            ("/base/gitBlobID", json!("d".repeat(40))),
            ("/base/commit", json!("d".repeat(40))),
            ("/base/path", json!("different.json")),
            ("/intentDigest", json!(format!("sha256:{}", "f".repeat(64)))),
            (
                "/operationId",
                json!("55555555-5555-4555-8555-555555555555"),
            ),
            ("/operation", json!("mark_uncertain")),
            ("/publicationReady", json!(true)),
            ("/remoteVerified", json!(true)),
            ("/ledgerHeadVerified", json!(true)),
            ("/requiresLedgerCompareAndSwap", json!(false)),
            ("/diagnostics/0", json!("forged")),
            ("/candidateLedger/sourceWatermark", json!("2".repeat(40))),
            (
                "/candidateLedger/entries/0/providerAck/etag",
                json!("forged"),
            ),
            (
                "/candidateLedger/entries/0/sourceAck/expected/raw",
                json!("forged"),
            ),
            (
                "/candidateLedger/entries/0/pending/intendedSource/expected/text",
                json!("forged"),
            ),
            (
                "/candidateLedger/entries/0/pending/phase",
                json!("uncertain"),
            ),
            (
                "/candidateLedger/entries/0/pending/desiredPayloadHash",
                json!(format!("sha256:{}", "f".repeat(64))),
            ),
            (
                "/candidateLedger/entries/1/tombstone/reason",
                json!("forged"),
            ),
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = replacement;
            let bad: OperationCandidate =
                ledger::decode_unique_json(&serde_json::to_vec(&bad).unwrap()).unwrap();
            let replayed = prepare_context(
                context(original.clone()),
                &selection(),
                entropy,
                &mut || Ok(()),
            )
            .unwrap();
            assert!(
                handoff(&bad, replayed, &options, &"a".repeat(64), &mut || Ok(())).is_err(),
                "{pointer}"
            );
        }
        let mut reordered = candidate.clone();
        reordered.candidate_ledger.entries.reverse();
        assert!(
            handoff(
                &reordered,
                candidate.clone(),
                &options,
                &"a".repeat(64),
                &mut || Ok(())
            )
            .is_err()
        );
        let change = handoff(
            &candidate,
            candidate.clone(),
            &options,
            &"a".repeat(64),
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(change.operation, StateOperation::Prepare);
        assert!(!change.parser_change_accepted);
        let crate::LedgerChangeProof::Intent(proof) = &change.proof else {
            panic!("expected an intent proof");
        };
        assert_eq!(proof.operation_id, OPERATION_ID);
        assert_eq!(proof.intent_digest, candidate.intent_digest);
        assert_eq!(
            change.candidate_ledger,
            canonical(&candidate.candidate_ledger, &binding()).unwrap()
        );
    }

    #[test]
    fn envelope_checks_binding_caps_approval_and_forbidden_legacy_flags() {
        let candidate = prepared(false);
        let mut options = apply_options(&candidate);
        validate_envelope(&candidate, &options, &"a".repeat(64), &mut || Ok(())).unwrap();
        for flag in 0..2 {
            options.create_ref = flag == 0;
            options.accept_parser_change = flag == 1;
            assert!(
                validate_envelope(&candidate, &options, &"a".repeat(64), &mut || Ok(())).is_err()
            );
        }
        options = apply_options(&candidate);
        assert!(validate_envelope(&candidate, &options, &"b".repeat(64), &mut || Ok(())).is_err());
        options.binding.calendar_id = "other@example.test".into();
        assert!(validate_envelope(&candidate, &options, &"a".repeat(64), &mut || Ok(())).is_err());
        let mut huge = candidate.clone();
        huge.diagnostics = vec!["x".repeat(CANDIDATE_LIMIT)];
        assert!(
            validate_envelope(
                &huge,
                &apply_options(&candidate),
                &"a".repeat(64),
                &mut || Ok(())
            )
            .is_err()
        );
        let mut huge_ledger = candidate.clone();
        huge_ledger.candidate_ledger.entries[0]
            .source_ack
            .expected
            .raw = "x".repeat(1024 * 1024);
        assert!(
            validate_envelope(
                &huge_ledger,
                &apply_options(&candidate),
                &"a".repeat(64),
                &mut || Ok(())
            )
            .is_err()
        );
    }

    #[test]
    fn hypothetical_success_is_unverified_and_changes_only_provider_state() {
        for active in [false, true] {
            for uncertain_phase in [false, true] {
                let mut candidate = prepared(active);
                add_unselected(&mut candidate.candidate_ledger);
                if uncertain_phase {
                    candidate = uncertain(&candidate);
                }
                let observed = observation(&candidate);
                let simulation = simulation(&candidate, observed.clone()).unwrap();
                assert!(simulation.simulation_only);
                assert!(
                    !simulation.persistence_allowed
                        && !simulation.remote_verified
                        && !simulation.publication_ready
                        && !simulation.ledger_head_verified
                );
                assert_eq!(simulation.kind, "calendar-provider-simulation");
                assert_eq!(simulation.observation, observed);
                assert!(
                    simulation
                        .diagnostics
                        .contains(&"operator_assertion_not_authentication".into())
                );
                let entry = &simulation.hypothetical_ledger.entries[0];
                assert_eq!(entry.lifecycle, Lifecycle::Active);
                assert!(entry.pending.is_none());
                assert_eq!(entry.provider_ack.as_ref().unwrap().etag, observed.etag);
                let mut restored = simulation.hypothetical_ledger.clone();
                restored.entries[0] = candidate.candidate_ledger.entries[0].clone();
                assert_eq!(restored, candidate.candidate_ledger);
                assert!(
                    ledger::decode_unique_json::<OperationCandidate>(
                        &serde_json::to_vec(&simulation).unwrap()
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn simulation_refuses_every_observation_mismatch_and_fake_authentication() {
        let candidate = prepared(true);
        let value = serde_json::to_value(observation(&candidate)).unwrap();
        for (pointer, replacement) in [
            ("/schemaVersion", json!("2")),
            ("/provenance", json!("verified_http")),
            ("/binding/calendarId", json!("other@example.test")),
            ("/logicalId", json!(OPERATION_ID)),
            ("/managedLogicalId", json!(OPERATION_ID)),
            ("/operationId", json!(LOGICAL_ID)),
            ("/intentDigest", json!(format!("sha256:{}", "f".repeat(64)))),
            ("/eventId", json!("other0")),
            ("/etag", json!("")),
            ("/etag", json!("bad\r\netag")),
            ("/etag", json!("x".repeat(1025))),
            ("/payload/summary", json!("forged")),
            ("/payload/startDate", json!("2026-11-01T00:00:00Z")),
            ("/payload/endDate", json!("2026-11-03")),
            ("/payload/transparency", json!("opaque")),
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = replacement;
            let bad: OperatorObservation = serde_json::from_value(bad).unwrap();
            assert!(simulation(&candidate, bad).is_err(), "{pointer}");
        }
        let mut without_pending = candidate.clone();
        without_pending.candidate_ledger.entries[0].pending = None;
        assert!(simulation(&without_pending, observation(&candidate)).is_err());
    }

    #[test]
    fn capture_is_single_bounded_regular_nofollow_hash_approved_unique_json_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("observation.json");
        let observed = observation(&prepared(false));
        let bytes = serde_json::to_vec(&observed).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        fs::write(&path, &bytes).unwrap();
        let (captured, captured_hash) =
            capture_observation(&path, &digest, &mut || Ok(())).unwrap();
        fs::write(&path, b"changed after capture").unwrap();
        assert_eq!(captured, observed);
        assert_eq!(captured_hash, digest);
        assert!(simulation(&prepared(false), captured).is_ok());
        fs::remove_file(&path).unwrap();
        assert!(capture_observation(&path, &digest, &mut || Ok(())).is_err());
        for bad in [
            "a".repeat(63),
            "A".repeat(64),
            format!("sha256:{digest}"),
            String::new(),
        ] {
            assert!(
                capture_observation(&path, &bad, &mut || Ok(()))
                    .unwrap_err()
                    .to_string()
                    .contains("digest")
            );
        }
        assert!(capture_observation(Path::new("relative"), &digest, &mut || Ok(())).is_err());
        assert!(capture_observation(temp.path(), &digest, &mut || Ok(())).is_err());
        fs::write(&path, &bytes).unwrap();
        assert!(capture_observation(&path, &"b".repeat(64), &mut || Ok(())).is_err());
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(OBSERVATION_LIMIT as u64 + 1).unwrap();
        assert_eq!(
            capture_observation(&path, &digest, &mut || Ok(()))
                .unwrap_err()
                .to_string(),
            "observation file limit exceeded"
        );
        let mut padded = bytes.clone();
        padded.resize(OBSERVATION_LIMIT, b' ');
        fs::write(&path, &padded).unwrap();
        let padded_digest = format!("{:x}", Sha256::digest(&padded));
        assert!(capture_observation(&path, &padded_digest, &mut || Ok(())).is_ok());
        let json = String::from_utf8(bytes).unwrap();
        for key in ["schemaVersion", "operationId", "provider", "summary"] {
            let needle = format!("\"{key}\":");
            let duplicated = json.replacen(&needle, &format!("\"{key}\":null,{needle}"), 1);
            fs::write(&path, &duplicated).unwrap();
            let digest = format!("{:x}", Sha256::digest(duplicated.as_bytes()));
            let error = capture_observation(&path, &digest, &mut || Ok(()))
                .unwrap_err()
                .to_string();
            assert_eq!(error, "invalid observation JSON or schema");
        }
        #[cfg(unix)]
        {
            use std::{
                ffi::CString,
                os::unix::{ffi::OsStrExt, fs::symlink},
            };
            let link = temp.path().join("link");
            symlink(&path, &link).unwrap();
            assert_eq!(
                capture_observation(&link, &digest, &mut || Ok(()))
                    .unwrap_err()
                    .to_string(),
                "observation file unavailable"
            );
            let fifo = temp.path().join("fifo");
            let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
            assert_eq!(
                capture_observation(&fifo, &digest, &mut || Ok(()))
                    .unwrap_err()
                    .to_string(),
                "observation file must be regular"
            );
        }
    }

    #[test]
    fn root_cancellation_precedes_io_and_survives_cpu_replay_to_handoff() {
        let candidate = prepared(false);
        let options = apply_options(&candidate);
        let deadline = deadline();
        let operation = source::SourceSession::operation(deadline).unwrap();
        operation.cancel();
        assert_eq!(
            validate_candidate(&candidate, &options, &"a".repeat(64), deadline, &operation)
                .err()
                .unwrap()
                .to_string(),
            "source operation cancelled"
        );
        assert_eq!(
            capture_observation(Path::new("relative"), "bad", &mut || before(
                deadline, &operation
            ))
            .unwrap_err()
            .to_string(),
            "source operation cancelled"
        );
        let run = |check: &mut dyn FnMut() -> Result<()>| -> Result<PreparedLedgerChange> {
            let mut check = || check();
            validate_envelope(&candidate, &options, &"a".repeat(64), &mut check)?;
            let replayed =
                prepare_context(context(original(false)), &selection(), entropy, &mut check)?;
            handoff(&candidate, replayed, &options, &"a".repeat(64), &mut check)
        };
        let mut count = 0;
        run(&mut || {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert!(count >= 15);
        for cancel_at in 1..=count {
            let operation = source::SourceSession::operation(deadline).unwrap();
            let mut seen = 0;
            let error = run(&mut || {
                seen += 1;
                if seen == cancel_at {
                    operation.cancel();
                }
                before(deadline, &operation)
            })
            .err()
            .unwrap();
            assert_eq!(error.to_string(), "source operation cancelled");
        }
    }

    #[test]
    fn cancellation_at_each_frozen_recovery_and_simulation_checkpoint() {
        let candidate = prepared(false);
        for simulate in [false, true] {
            let run = |check: &mut dyn FnMut() -> Result<()>| -> Result<()> {
                let mut check = || check();
                if simulate {
                    simulate_context(
                        candidate.candidate_ledger.clone(),
                        base(),
                        &selection(),
                        OPERATION_ID,
                        observation(&candidate),
                        "a".repeat(64),
                        &mut check,
                    )?;
                } else {
                    uncertain_context(
                        candidate.candidate_ledger.clone(),
                        base(),
                        &selection(),
                        OPERATION_ID,
                        &mut check,
                    )?;
                }
                Ok(())
            };
            let mut count = 0;
            run(&mut || {
                count += 1;
                Ok(())
            })
            .unwrap();
            for cancel_at in 1..=count {
                let operation = source::SourceSession::operation(deadline()).unwrap();
                let mut seen = 0;
                assert!(
                    run(&mut || {
                        seen += 1;
                        if seen == cancel_at {
                            operation.cancel();
                        }
                        operation.check()
                    })
                    .is_err()
                );
            }
        }
    }

    // Every write below is confined to a disposable fixture, never user Git state.
    struct GitFixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        repo: PathBuf,
    }

    impl GitFixture {
        fn new() -> Self {
            let temp = tempfile::tempdir_in("/tmp").unwrap();
            let root = temp.path().canonicalize().unwrap();
            let repo = root.join("repo");
            fs::create_dir(&repo).unwrap();
            let result = Self {
                _temp: temp,
                root,
                repo,
            };
            result.git(&["init", "--quiet"]);
            result
        }
        fn git(&self, args: &[&str]) -> String {
            let output = Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &self.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args([
                    "-c",
                    "user.name=Operation Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-C",
                ])
                .arg(&self.repo)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "isolated fixture Git failed");
            String::from_utf8(output.stdout).unwrap().trim().into()
        }
        fn commit(&self) -> String {
            self.git(&["add", "--all"]);
            self.git(&["commit", "--quiet", "-m", "isolated operation fixture"]);
            self.git(&["rev-parse", "HEAD"])
        }
        fn save_ledger(&self, ledger: &Ledger) -> LedgerLocation {
            fs::write(
                self.repo.join("calendar.json"),
                canonical(ledger, &binding()).unwrap(),
            )
            .unwrap();
            LedgerLocation {
                repository: self.repo.clone(),
                commit: self.commit(),
                path: "calendar.json".into(),
            }
        }
    }

    #[test]
    fn committed_frozen_recovery_and_simulation_need_no_source_or_mw() {
        let fixture = GitFixture::new();
        let prepared = prepared(false);
        let location = fixture.save_ledger(&prepared.candidate_ledger);
        let uncertain_options = UncertainOptions {
            binding: binding(),
            ledger: location.clone(),
            entry: selection(),
            operation_id: OPERATION_ID.into(),
        };
        let candidate = mark_uncertain(&uncertain_options).unwrap();
        assert_eq!(candidate.source_commit, "1".repeat(40));
        let mut options = apply_options(&candidate);
        options.ledger_repository = fixture.repo.clone();
        let operation = source::SourceSession::operation(deadline()).unwrap();
        let change = validate_candidate(
            &candidate,
            &options,
            &"a".repeat(64),
            deadline(),
            &operation,
        )
        .unwrap();
        assert_eq!(change.operation, StateOperation::MarkUncertain);
        let crate::LedgerChangeProof::Intent(proof) = &change.proof else {
            panic!("expected an intent proof");
        };
        assert_eq!(proof.operation_id, OPERATION_ID);
        assert_eq!(
            change.source_commit,
            prepared.candidate_ledger.entries[0].source_ack.commit
        );
        // Simulate a failed persistence handoff: no source or ledger mutation occurred,
        // so the exact pending operation remains recoverable from the original base.
        drop(change);
        assert_eq!(mark_uncertain(&uncertain_options).unwrap(), candidate);
        assert_eq!(fixture.git(&["rev-parse", "HEAD"]), location.commit);
        assert_eq!(fixture.git(&["status", "--porcelain"]), "");
        let path = fixture.root.join("observation.json");
        let bytes = serde_json::to_vec(&observation(&candidate)).unwrap();
        fs::write(&path, &bytes).unwrap();
        let simulation_options = SimulationOptions {
            binding: binding(),
            ledger: location.clone(),
            entry: selection(),
            operation_id: OPERATION_ID.into(),
            observation_path: path,
            expected_observation_sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let simulation = simulate_ack(&simulation_options).unwrap();
        assert!(simulation.simulation_only && !simulation.persistence_allowed);
        assert_eq!(
            simulation.hypothetical_ledger.entries[0].source_ack,
            prepared.candidate_ledger.entries[0].source_ack
        );
        let mut wrong = candidate.clone();
        wrong.operation_id = LOGICAL_ID.into();
        assert!(
            validate_candidate(&wrong, &options, &"a".repeat(64), deadline(), &operation).is_err()
        );
        let mut wrong = candidate.clone();
        wrong.base.git_blob_id = "d".repeat(40);
        assert!(
            validate_candidate(&wrong, &options, &"a".repeat(64), deadline(), &operation).is_err()
        );
        let mut wrong = candidate.clone();
        wrong.source_commit = "2".repeat(40);
        options.source.commit = wrong.source_commit.clone();
        assert!(
            validate_candidate(&wrong, &options, &"a".repeat(64), deadline(), &operation).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn real_prepare_replays_history_once_and_frozen_success_survives_later_note_deletion() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = GitFixture::new();
        let snapshot = target();
        fs::write(
            fixture.repo.join("tasks.md"),
            &snapshot.files["tasks.md"].content,
        )
        .unwrap();
        let source_commit = fixture.commit();
        let task = &snapshot.items[0];
        let task_json = json!({
            "id": task.id, "stableId": null, "identityKind": "positional", "sourceRevision": format!("sha256:{:x}", Sha256::digest(&snapshot.files["tasks.md"].content)),
            "sourceId": task.source_id, "sourcePath": task.source_path, "line": 1, "text": task.text, "done": false,
            "metadata": {"due": task.metadata.due, "raw": "", "start": "", "estimate": ""}
        });
        let mw = fixture.root.join("mw");
        let script = format!(
            "#!/bin/sh\nset -eu\n[ \"$#\" = 3 ] && [ \"$1\" = todos ] && [ \"$2\" = snapshot ] && [ \"$3\" = --json ]\nprintf '{{\"schemaVersion\":\"1\",\"source\":\"saved-markdown\",\"notesDir\":\"%s\",\"dashboardPath\":\"%s\",\"items\":[{task_json}]}}' \"$NOTES_DIR\" \"$DASHBOARD_PATH\"\n"
        );
        fs::write(&mw, &script).unwrap();
        fs::set_permissions(&mw, fs::Permissions::from_mode(0o700)).unwrap();
        let source = PreviewOptions {
            repository: fixture.repo.clone(),
            commit: source_commit.clone(),
            mw_binary: mw.clone(),
        };
        let actual =
            source::read_snapshot(&source.repository, &source.commit, &source.mw_binary).unwrap();
        let mut ledger = original(false);
        ledger.source_watermark = source_commit.clone();
        ledger.entries[0].source_ack = planner::observation(&actual, &actual.items[0]).unwrap();
        let location = fixture.save_ledger(&ledger);
        let prepare_options = PrepareOptions {
            source,
            binding: binding(),
            ledger: location.clone(),
            entry: selection(),
        };
        let candidate = prepare(&prepare_options).unwrap();
        let mut options = apply_options(&candidate);
        options.source = PreviewOptions {
            repository: fixture.repo.clone(),
            commit: source_commit.clone(),
            mw_binary: mw.clone(),
        };
        options.ledger_repository = fixture.repo.clone();
        let operation = source::SourceSession::operation(deadline()).unwrap();
        let first = validate_candidate(
            &candidate,
            &options,
            &"a".repeat(64),
            deadline(),
            &operation,
        )
        .unwrap();
        let second = validate_candidate(
            &candidate,
            &options,
            &"a".repeat(64),
            deadline(),
            &operation,
        )
        .unwrap();
        assert_eq!(first.candidate_ledger, second.candidate_ledger);
        let crate::LedgerChangeProof::Intent(proof) = &first.proof else {
            panic!("expected an intent proof");
        };
        assert_eq!(proof.operation_id, candidate.operation_id);
        assert_eq!(fixture.git(&["rev-parse", "HEAD"]), location.commit);
        assert_eq!(fixture.git(&["status", "--porcelain"]), "");
        // Parser drift cannot be silently accepted for prepare.
        fs::write(&mw, format!("{script}# changed parser artifact\n")).unwrap();
        assert!(
            validate_candidate(
                &candidate,
                &options,
                &"a".repeat(64),
                deadline(),
                &operation
            )
            .is_err()
        );
        fs::write(&mw, script).unwrap();
        // Forging original metadata must fail BEFORE the proposed pending is added.
        let mut forged = ledger.clone();
        forged.entries[0].source_ack.expected.raw = "forged original history".into();
        let forged_location = fixture.save_ledger(&forged);
        let mut forged_candidate = candidate.clone();
        forged_candidate.base.commit = forged_location.commit;
        forged_candidate.base.git_blob_id = fixture.git(&["rev-parse", "HEAD:calendar.json"]);
        assert!(
            validate_candidate(
                &forged_candidate,
                &options,
                &"a".repeat(64),
                deadline(),
                &operation
            )
            .err()
            .unwrap()
            .to_string()
            .contains("source evidence mismatch")
        );
        let pending_location = fixture.save_ledger(&candidate.candidate_ledger);
        // Commit later deletion and remove MW itself. Frozen operations still work.
        fs::remove_file(fixture.repo.join("tasks.md")).unwrap();
        fixture.commit();
        fs::remove_file(mw).unwrap();
        let marked = mark_uncertain(&UncertainOptions {
            binding: binding(),
            ledger: pending_location.clone(),
            entry: selection(),
            operation_id: candidate.operation_id.clone(),
        })
        .unwrap();
        options.source.commit = marked.source_commit.clone();
        assert!(
            validate_candidate(&marked, &options, &"a".repeat(64), deadline(), &operation).is_ok()
        );
        let observed = observation(&candidate);
        let observation_path = fixture.root.join("observed.json");
        let bytes = serde_json::to_vec(&observed).unwrap();
        fs::write(&observation_path, &bytes).unwrap();
        let simulated = simulate_ack(&SimulationOptions {
            binding: binding(),
            ledger: pending_location,
            entry: selection(),
            operation_id: candidate.operation_id,
            observation_path,
            expected_observation_sha256: format!("{:x}", Sha256::digest(&bytes)),
        })
        .unwrap();
        assert_eq!(simulated.observation, observed);
        assert!(!simulated.persistence_allowed && !simulated.remote_verified);
        assert_eq!(
            simulated.hypothetical_ledger.entries[0].source_ack,
            ledger.entries[0].source_ack
        );
    }
}
