//! Credential-free Calendar planning and explicitly approved local ledger persistence.
//! The private HTTP adapter is dormant: no public provider activation, credential
//! wiring, network synchronization, or automatic source edits.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use serde::Deserialize;

mod approval;
mod candidates;
// Deliberately unwired: production dispatch authority and credential wiring are
// separate activation gates. The private adapter is exercised through loopback tests.
#[allow(
    dead_code,
    reason = "Calendar transport is intentionally not activated"
)]
mod calendar_transport;
mod ledger;
mod operations;
mod planner;
mod projection;
mod source;
mod state_git;
pub mod task_snapshot;

pub use candidates::{CandidateBase, CandidateOperation, LedgerCandidate};
pub use ledger::{
    Binding, ExpectedTask, Ledger, LedgerEntry, Lifecycle, OperationKind, OperationPhase,
    PendingOperation, ProviderAck, RemoteDisposition, SourceAck, Tombstone, payload_hash,
};
pub use operations::{
    AckSimulation, ObservationPayload, OperationBase, OperationCandidate, OperationTransition,
    OperatorObservation, intent_digest, reserved_event_id,
};
pub use planner::{EntryPlan, LedgerBasis, Plan, PlanCounts, PlanDecision, UnenrolledObservation};
pub use projection::{
    CalendarDate, ItemSource, Preview, PreviewCounts, PreviewItem, ProspectiveEvent,
};
pub use state_git::ApplyReceipt;

/// Explicit inputs only; no desktop configuration, credentials, or ambient source root.
#[derive(Debug)]
pub struct PreviewOptions {
    pub repository: PathBuf,
    pub commit: String,
    pub mw_binary: PathBuf,
}

pub fn preview(options: &PreviewOptions) -> Result<Preview> {
    let snapshot = source::read_snapshot(&options.repository, &options.commit, &options.mw_binary)?;
    projection::project(&snapshot)
}

/// Ledger planning is advisory only: no enrollment, provider access, or Git writes.
#[derive(Debug)]
pub struct PlanOptions {
    pub source: PreviewOptions,
    pub binding: Binding,
    pub ledger: LedgerInput,
}

#[derive(Debug)]
pub enum LedgerInput {
    Committed {
        repository: PathBuf,
        commit: String,
        path: String,
    },
    /// Explicit observation-only bootstrap; never a fallback for missing state.
    BootstrapPreview,
}

pub fn plan(options: &PlanOptions) -> Result<Plan> {
    planner::plan(options)
}

/// A committed publication-state location; the current branch head is not inferred.
#[derive(Clone, Debug)]
pub struct LedgerLocation {
    pub repository: PathBuf,
    pub commit: String,
    pub path: String,
}

/// Explicit human-selected current source location, not a durable task identity.
#[derive(Clone, Debug)]
pub struct TaskSelection {
    pub path: String,
    pub line: usize,
}

#[derive(Clone, Debug)]
pub enum EntrySelection {
    LogicalId(String),
    AcknowledgedLocation { path: String, line: usize },
}

#[derive(Debug)]
pub struct EnrollmentOptions {
    pub source: PreviewOptions,
    pub binding: Binding,
    pub ledger: LedgerLocation,
    pub selection: TaskSelection,
    /// Only propose initialization after proving this path absent at the base commit.
    pub initialize_ledger: bool,
}

#[derive(Debug)]
pub struct ReackOptions {
    pub source: PreviewOptions,
    pub binding: Binding,
    pub ledger: LedgerLocation,
    pub entry: EntrySelection,
    pub selection: TaskSelection,
    /// Explicitly approve the selected task's newly parsed evidence after a parser change.
    pub accept_parser_change: bool,
}

/// Generate a candidate ledger only. This does not persist enrollment or publish.
pub fn enroll(options: &EnrollmentOptions) -> Result<LedgerCandidate> {
    candidates::enroll(options)
}

/// Propose new evidence under an existing logical ID, preserving publication evidence.
pub fn reack(options: &ReackOptions) -> Result<LedgerCandidate> {
    candidates::reack(options)
}

#[derive(Debug)]
pub struct PrepareOptions {
    pub source: PreviewOptions,
    pub binding: Binding,
    pub ledger: LedgerLocation,
    pub entry: EntrySelection,
}

/// Monotonic recovery evidence; it does not require today's note or parser to exist.
#[derive(Debug)]
pub struct UncertainOptions {
    pub binding: Binding,
    pub ledger: LedgerLocation,
    pub entry: EntrySelection,
    pub operation_id: String,
}

/// An explicitly unverified operator assertion. Ordinary apply cannot persist its result.
#[derive(Debug)]
pub struct SimulationOptions {
    pub binding: Binding,
    pub ledger: LedgerLocation,
    pub entry: EntrySelection,
    pub operation_id: String,
    pub observation_path: PathBuf,
    pub expected_observation_sha256: String,
}

pub fn prepare_operation(options: &PrepareOptions) -> Result<OperationCandidate> {
    operations::prepare(options)
}

pub fn mark_uncertain(options: &UncertainOptions) -> Result<OperationCandidate> {
    operations::mark_uncertain(options)
}

pub fn simulate_acknowledgement(options: &SimulationOptions) -> Result<AckSimulation> {
    operations::simulate_ack(options)
}

/// State commit operations, separate from the frozen V1 enrollment-candidate vocabulary.
#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StateOperation {
    Enroll,
    Reack,
    Prepare,
    MarkUncertain,
    Acknowledge,
}

impl From<CandidateOperation> for StateOperation {
    fn from(operation: CandidateOperation) -> Self {
        match operation {
            CandidateOperation::Enroll => Self::Enroll,
            CandidateOperation::Reack => Self::Reack,
        }
    }
}

/// Explicit local-only apply intent. Nothing infers the authoritative remote ref.
#[derive(Debug)]
pub struct ApplyOptions {
    pub source: PreviewOptions,
    pub binding: Binding,
    pub ledger_repository: PathBuf,
    pub ledger_path: String,
    pub state_ref: String,
    pub candidate_path: PathBuf,
    /// Independently approved SHA-256 of the exact captured candidate-file bytes.
    pub expected_candidate_sha256: String,
    /// Only initialize an absent dedicated ref; never adopt or overwrite a foreign ref.
    pub create_ref: bool,
    pub accept_parser_change: bool,
}

pub fn verify_candidate(options: &ApplyOptions) -> Result<ApplyReceipt> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let operation = source::SourceSession::operation(deadline)?;
    let change = approval::validate(options, deadline, &operation)?;
    operation.check()?;
    state_git::verify(
        &options.ledger_repository,
        &options.state_ref,
        &change,
        options.create_ref,
        deadline,
        &operation,
    )
}

/// Persist only to the dedicated local state ref, using an atomic old-OID check.
pub fn apply_candidate(options: &ApplyOptions) -> Result<ApplyReceipt> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let operation = source::SourceSession::operation(deadline)?;
    let change = approval::validate(options, deadline, &operation)?;
    operation.check()?;
    state_git::apply(
        &options.ledger_repository,
        &options.state_ref,
        &change,
        options.create_ref,
        deadline,
        &operation,
    )
}

/// Internal handoff: validated transition and canonical ledger bytes, not untrusted JSON.
pub(crate) struct PreparedLedgerChange {
    pub candidate_sha256: String,
    pub operation: StateOperation,
    pub logical_id: String,
    pub source_commit: String,
    pub binding: Binding,
    pub parser_change_accepted: bool,
    pub base_commit: String,
    pub path: String,
    pub expected_blob_id: Option<String>,
    pub candidate_ledger: Vec<u8>,
    pub proof: LedgerChangeProof,
}

/// Distinct in-process authority for source changes, frozen intents, and terminal ACKs.
pub(crate) enum LedgerChangeProof {
    Source,
    Intent(ProviderOperationProof),
    Acknowledgement(calendar_transport::ack::SealedAckProof),
}

pub(crate) struct ProviderOperationProof {
    pub operation_id: String,
    pub intent_digest: String,
}

pub(crate) struct CommittedSnapshot {
    pub commit: String,
    pub parser_sha256: String,
    pub files: BTreeMap<String, SourceBlob>,
    pub items: Vec<SourceTask>,
}

pub(crate) struct SourceBlob {
    pub object_id: String,
    pub content: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceTask {
    pub id: String,
    pub stable_id: Option<String>,
    pub identity_kind: String,
    pub source_revision: String,
    pub source_id: String,
    pub source_path: String,
    pub line: usize,
    pub text: String,
    /// Original committed checkbox line, before MW's title/area normalization.
    #[serde(skip)]
    pub literal_text: String,
    pub done: bool,
    pub metadata: TaskMetadata,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct TaskMetadata {
    pub due: String,
    pub raw: String,
    pub start: String,
    pub estimate: String,
}
