//! Local-only, files-backend state commits. The operator MUST exclusively
//! administer this dedicated bare repository during an operation. Preflight and
//! pre-CAS checks cannot exclude concurrent worktree/configuration administration.
//! Ref CAS serializes competing ledger writers, not repository administrators.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
    time::Instant,
};

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    Binding, LedgerChangeProof, PreparedLedgerChange, StateOperation,
    calendar_transport::ack::{self, AckAudit, AckCause, AckOrigin},
    source::{GitSession, SourceSession},
};

const STATE_REF: &str = "refs/heads/hive-calendar-state";
const MARKER: &str = "hive-calendar-state v1\n\n";
// Intentional upgrade boundary: V1-only clients must reject a V2-owned tip.
const MARKER_V2: &str = "hive-calendar-state v2\n\n";
const MARKER_V3: &str = "hive-calendar-state v3\n\n";
const IDENTITY: &str = "Hive Calendar <hive-calendar@localhost>";
// Individual objects fit the existing transport's input bound; the entire
// visited tree graph (including reconstructed objects) is capped separately.
const OBJECT_CAP: usize = 1024 * 1024;
const GRAPH_CAP: usize = 64 * 1024 * 1024;
const ENTRY_CAP: usize = 100_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReceipt {
    pub schema_version: &'static str,
    pub status: &'static str,
    pub state_ref: String,
    pub ledger_path: String,
    pub binding: Binding,
    pub candidate_sha256: String,
    pub source_commit: String,
    pub base_commit: String,
    pub commit: String,
    #[serde(rename = "ledgerBlobID")]
    pub ledger_blob_id: String,
    pub operation: StateOperation,
    pub logical_id: String,
    pub parser_change_accepted: bool,
    pub local_ref_updated: bool,
    pub publication_ready: bool,
    pub remote_verified: bool,
    pub remote_ref_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ack_cause: Option<AckCause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provenance_floor: Option<AckOrigin>,
}

// Field order, marker, JSON encoding and final LF are the v1 wire format.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema_version: String,
    state_ref: String,
    path: String,
    binding: Binding,
    ledger_blob_id: String,
    candidate_sha256: String,
    source_commit: String,
    operation: String,
    logical_id: String,
    parser_change_accepted: bool,
    base_commit: String,
    expected_blob_id: Option<String>,
}

// V3 is a reader boundary, including for later source/intent continuations.
// These are local ownership receipts, not signatures or live authentication.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnedRecordV3 {
    schema_version: String,
    state_ref: String,
    path: String,
    binding: Binding,
    ledger_blob_id: String,
    candidate_sha256: String,
    source_commit: String,
    operation: String,
    logical_id: String,
    parser_change_accepted: bool,
    base_commit: String,
    expected_blob_id: Option<String>,
    proof: RecordProof,
    provenance_floor: AckOrigin,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", deny_unknown_fields)]
enum RecordProof {
    Source,
    Intent(IntentRecord),
    Acknowledgement(Box<AckAudit>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IntentRecord {
    operation_id: String,
    intent_digest: String,
}

impl OwnedRecordV3 {
    fn new(record: Record, proof: RecordProof, provenance_floor: AckOrigin) -> Self {
        Self {
            schema_version: "3".into(),
            state_ref: record.state_ref,
            path: record.path,
            binding: record.binding,
            ledger_blob_id: record.ledger_blob_id,
            candidate_sha256: record.candidate_sha256,
            source_commit: record.source_commit,
            operation: record.operation,
            logical_id: record.logical_id,
            parser_change_accepted: record.parser_change_accepted,
            base_commit: record.base_commit,
            expected_blob_id: record.expected_blob_id,
            proof,
            provenance_floor,
        }
    }

    fn common(&self) -> Record {
        Record {
            schema_version: "1".into(),
            state_ref: self.state_ref.clone(),
            path: self.path.clone(),
            binding: self.binding.clone(),
            ledger_blob_id: self.ledger_blob_id.clone(),
            candidate_sha256: self.candidate_sha256.clone(),
            source_commit: self.source_commit.clone(),
            operation: self.operation.clone(),
            logical_id: self.logical_id.clone(),
            parser_change_accepted: self.parser_change_accepted,
            base_commit: self.base_commit.clone(),
            expected_blob_id: self.expected_blob_id.clone(),
        }
    }

    fn message(&self) -> Result<Vec<u8>> {
        let mut bytes = MARKER_V3.as_bytes().to_vec();
        bytes.extend(serde_json::to_vec(self)?);
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn intent_record(&self, proof: &IntentRecord) -> OwnedRecordV2 {
        OwnedRecordV2::new(self.common(), &proof.operation_id, &proof.intent_digest)
    }

    fn validate(&self, len: usize) -> Result<()> {
        let mut common = self.common();
        common.operation = "enroll".into();
        common.validate(len)?;
        ensure!(
            self.schema_version == "3" && self.expected_blob_id.is_some(),
            "invalid V3 ownership receipt"
        );
        match &self.proof {
            RecordProof::Source => ensure!(
                matches!(self.operation.as_str(), "enroll" | "reack"),
                "source proof operation mismatch"
            ),
            RecordProof::Intent(proof) => self.intent_record(proof).validate(len)?,
            RecordProof::Acknowledgement(audit) => {
                ack::validate_audit(audit)?;
                ensure!(
                    self.operation == "acknowledge"
                        && !self.parser_change_accepted
                        && audit.binding == self.binding
                        && audit.base_commit == self.base_commit
                        && audit.path == self.path
                        && Some(&audit.old_blob_id) == self.expected_blob_id.as_ref()
                        && audit.logical_id == self.logical_id
                        && audit.evidence_digest.strip_prefix("sha256:")
                            == Some(self.candidate_sha256.as_str())
                        && audit.old_pending.intended_source.commit == self.source_commit,
                    "ACK ownership receipt mismatch"
                );
                ensure!(
                    self.provenance_floor
                        == inherited_floor(Some(self.provenance_floor), Some(audit.origin)),
                    "ACK provenance floor mismatch"
                );
            }
        }
        Ok(())
    }

    fn validate_transition(&self, old: &[u8], new: &[u8]) -> Result<()> {
        if let RecordProof::Acknowledgement(audit) = &self.proof {
            return ack::validate_transition(old, new, audit);
        }
        let mut before = crate::ledger::decode(old, &self.binding)?;
        let after = crate::ledger::decode(new, &self.binding)?;
        let index = before
            .entries
            .iter()
            .position(|e| e.logical_id == self.logical_id);
        let entry = after
            .entries
            .iter()
            .find(|e| e.logical_id == self.logical_id)
            .ok_or_else(|| anyhow!("V3 selected entry missing"))?;
        match &self.proof {
            RecordProof::Intent(proof) => {
                self.intent_record(proof).validate_ledger(new)?;
                if self.operation == "prepare" {
                    ensure!(
                        before.entries.iter().all(|e| e.pending.is_none()),
                        "V3 prepare with pending intent"
                    );
                }
                let index = index.ok_or_else(|| anyhow!("V3 intent base entry missing"))?;
                let pending = entry
                    .pending
                    .as_ref()
                    .ok_or_else(|| anyhow!("V3 pending intent missing"))?;
                let prior = &mut before.entries[index];
                if self.operation == "prepare" {
                    ensure!(
                        prior.pending.is_none()
                            && prior.tombstone.is_none()
                            && prior.source_ack == pending.intended_source,
                        "V3 prepare basis mismatch"
                    );
                    match (&prior.lifecycle, &prior.provider_ack, pending.kind) {
                        (crate::Lifecycle::Enrolled, None, crate::OperationKind::Create) => {
                            ensure!(pending.base_etag.is_none(), "V3 create etag mismatch")
                        }
                        (crate::Lifecycle::Active, Some(ack), crate::OperationKind::Update) => {
                            ensure!(
                                pending.target_event_id == ack.event_id
                                    && pending.base_etag.as_ref() == Some(&ack.etag)
                                    && pending.desired_payload_hash.as_ref()
                                        != Some(&ack.last_payload_hash),
                                "V3 update basis mismatch"
                            )
                        }
                        _ => return Err(anyhow!("V3 prepare lifecycle mismatch")),
                    }
                    prior.pending = Some(pending.clone());
                } else {
                    let old_pending = prior
                        .pending
                        .as_mut()
                        .ok_or_else(|| anyhow!("V3 uncertain base intent missing"))?;
                    ensure!(
                        old_pending.phase == crate::OperationPhase::Prepared,
                        "V3 uncertain base phase mismatch"
                    );
                    old_pending.phase = crate::OperationPhase::Uncertain;
                }
            }
            RecordProof::Source => {
                if self.operation == "enroll" {
                    ensure!(
                        !self.parser_change_accepted
                            && before.entries.iter().all(|e| e.pending.is_none()),
                        "V3 enrollment with pending intent or parser acceptance"
                    );
                    ensure!(
                        index.is_none()
                            && entry.lifecycle == crate::Lifecycle::Enrolled
                            && entry.provider_ack.is_none()
                            && entry.pending.is_none()
                            && entry.tombstone.is_none(),
                        "V3 enrollment mismatch"
                    );
                    before.entries.push(entry.clone());
                } else {
                    let index = index.ok_or_else(|| anyhow!("V3 reack base entry missing"))?;
                    ensure!(
                        before.entries[index].lifecycle != crate::Lifecycle::Retired
                            && before.entries[index].pending.is_none(),
                        "V3 retired or pending source change"
                    );
                    ensure!(
                        self.parser_change_accepted
                            == (before.entries[index].source_ack.parser_sha256
                                != entry.source_ack.parser_sha256),
                        "V3 parser acceptance mismatch"
                    );
                    before.entries[index].source_ack = entry.source_ack.clone();
                }
                ensure!(
                    entry.source_ack.commit == self.source_commit
                        && after.source_watermark == self.source_commit,
                    "V3 source commit mismatch"
                );
                before.source_watermark = after.source_watermark.clone();
            }
            RecordProof::Acknowledgement(_) => unreachable!(),
        }
        ensure!(
            before == after,
            "V3 transition changed unrelated ledger state"
        );
        Ok(())
    }
}

fn inherited_floor(parent: Option<AckOrigin>, origin: Option<AckOrigin>) -> AckOrigin {
    // SyntheticFixture does not exist in production: serde rejects that tag in
    // ACK audits AND continuation floors, even after the pending intent is gone.
    #[cfg(test)]
    if matches!(parent, Some(AckOrigin::SyntheticFixture))
        || matches!(origin, Some(AckOrigin::SyntheticFixture))
    {
        return AckOrigin::SyntheticFixture;
    }
    let _ = (parent, origin);
    AckOrigin::GoogleAuthenticated
}

impl Record {
    fn message(&self) -> Result<Vec<u8>> {
        let mut bytes = MARKER.as_bytes().to_vec();
        bytes.extend(serde_json::to_vec(self)?);
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn validate(&self, len: usize) -> Result<()> {
        ensure!(
            self.schema_version == "1"
                && self.state_ref == STATE_REF
                && valid_path(&self.path)
                && oid(&self.ledger_blob_id, len)
                && oid(&self.candidate_sha256, 64)
                && (oid(&self.source_commit, 40) || oid(&self.source_commit, 64))
                && oid(&self.base_commit, len)
                && self.expected_blob_id.as_deref().is_none_or(|v| oid(v, len))
                && matches!(self.operation.as_str(), "enroll" | "reack")
                && !self.logical_id.is_empty()
                && self.logical_id.len() <= 4096
                && !self.logical_id.chars().any(char::is_control),
            "invalid state ownership receipt"
        );
        crate::ledger::validate_binding(&self.binding)
    }
}

// Deliberately separate from the frozen V1 vocabulary and wire encoding.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnedRecordV2 {
    schema_version: String,
    state_ref: String,
    path: String,
    binding: Binding,
    ledger_blob_id: String,
    candidate_sha256: String,
    source_commit: String,
    operation: String,
    logical_id: String,
    parser_change_accepted: bool,
    base_commit: String,
    expected_blob_id: Option<String>,
    operation_id: String,
    intent_digest: String,
}

impl OwnedRecordV2 {
    fn new(record: Record, operation_id: &str, intent_digest: &str) -> Self {
        Self {
            schema_version: "2".into(),
            state_ref: record.state_ref,
            path: record.path,
            binding: record.binding,
            ledger_blob_id: record.ledger_blob_id,
            candidate_sha256: record.candidate_sha256,
            source_commit: record.source_commit,
            operation: record.operation,
            logical_id: record.logical_id,
            parser_change_accepted: record.parser_change_accepted,
            base_commit: record.base_commit,
            expected_blob_id: record.expected_blob_id,
            operation_id: operation_id.into(),
            intent_digest: intent_digest.into(),
        }
    }

    fn message(&self) -> Result<Vec<u8>> {
        let mut bytes = MARKER_V2.as_bytes().to_vec();
        bytes.extend(serde_json::to_vec(self)?);
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn common(&self) -> Record {
        Record {
            schema_version: "1".into(),
            state_ref: self.state_ref.clone(),
            path: self.path.clone(),
            binding: self.binding.clone(),
            ledger_blob_id: self.ledger_blob_id.clone(),
            candidate_sha256: self.candidate_sha256.clone(),
            source_commit: self.source_commit.clone(),
            operation: "enroll".into(),
            logical_id: self.logical_id.clone(),
            parser_change_accepted: self.parser_change_accepted,
            base_commit: self.base_commit.clone(),
            expected_blob_id: self.expected_blob_id.clone(),
        }
    }

    fn validate(&self, len: usize) -> Result<()> {
        self.common().validate(len)?;
        ensure!(
            self.schema_version == "2"
                && matches!(self.operation.as_str(), "prepare" | "mark_uncertain")
                && !self.parser_change_accepted
                && self.expected_blob_id.is_some()
                && operation_uuid(&self.operation_id)
                && digest(&self.intent_digest),
            "invalid state ownership receipt"
        );
        Ok(())
    }

    fn validate_ledger(&self, bytes: &[u8]) -> Result<()> {
        let ledger = crate::ledger::decode(bytes, &self.binding)?;
        let entry = ledger
            .entries
            .iter()
            .find(|e| e.logical_id == self.logical_id)
            .ok_or_else(|| anyhow!("owned operation entry missing"))?;
        let pending = entry
            .pending
            .as_ref()
            .ok_or_else(|| anyhow!("owned operation pending intent missing"))?;
        let phase = if self.operation == "prepare" {
            crate::OperationPhase::Prepared
        } else {
            crate::OperationPhase::Uncertain
        };
        ensure!(
            pending.operation_id == self.operation_id
                && pending.phase == phase
                && (self.operation != "mark_uncertain"
                    || pending.intended_source.commit == self.source_commit)
                && crate::operations::intent_digest(&self.binding, &self.logical_id, pending)?
                    == self.intent_digest,
            "owned operation intent mismatch"
        );
        // V2 owns only the supported create/update intents, not legacy deletes.
        // Reconstruct the frozen payload without reading current source or MW.
        let ack = &pending.intended_source;
        let task = crate::SourceTask {
            id: ack.mw_id.clone(),
            stable_id: ack.stable_id.clone(),
            identity_kind: String::new(),
            source_revision: ack.source_revision.clone(),
            source_id: ack.source_id.clone(),
            source_path: ack.path.clone(),
            line: ack.line,
            text: ack.expected.text.clone(),
            literal_text: ack.expected.literal_text.clone(),
            done: ack.expected.done,
            metadata: crate::TaskMetadata {
                due: ack.expected.due.clone(),
                raw: ack.expected.raw.clone(),
                start: ack.expected.start.clone(),
                estimate: ack.expected.estimate.clone(),
            },
        };
        let evidence = crate::projection::due_evidence(&task)?;
        ensure!(
            pending.kind != crate::OperationKind::Delete
                && !task.done
                && evidence.diagnostic.is_none()
                && evidence.event.as_ref().is_some_and(|event| {
                    pending.desired_payload_hash.as_ref() == Some(&crate::payload_hash(event))
                }),
            "owned operation payload mismatch"
        );
        if pending.kind == crate::OperationKind::Create {
            ensure!(
                pending.target_event_id
                    == crate::operations::reserved_event_id(&self.binding, &self.logical_id)?,
                "owned operation event mismatch"
            );
        }
        Ok(())
    }
}

fn operation_uuid(value: &str) -> bool {
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

fn digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|v| {
        v.len() == 64
            && v.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

struct Object {
    kind: &'static str,
    bytes: Vec<u8>,
    oid: String,
}

#[derive(Clone)]
struct Entry {
    mode: Vec<u8>,
    name: Vec<u8>,
    oid: String,
}

impl Entry {
    fn key(&self) -> Vec<u8> {
        let mut key = self.name.clone();
        if self.mode == b"40000" {
            key.push(b'/');
        }
        key
    }
}

struct Commit {
    tree: String,
    parents: Vec<String>,
    time: i64,
    message: Vec<u8>,
    bytes: Vec<u8>,
}

struct Preparation {
    git: GitSession,
    len: usize,
    trees: BTreeMap<String, Vec<Entry>>,
    bytes: usize,
    entries: usize,
    objects: Vec<Object>,
}

fn oid(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && value.bytes().any(|b| b != b'0')
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.to_ascii_lowercase().ends_with(".json")
        && path.split('/').count() <= 64
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git"))
}

/// Inspect metadata without following the final component. Repository internals
/// must be operator/root owned and not writable by other users.
fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(anyhow!("state repository metadata unavailable")),
    };
    ensure!(
        !m.file_type().is_symlink(),
        "state repository symlink refused"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            (m.uid() == unsafe { libc::geteuid() } || m.uid() == 0) && m.mode() & 0o022 == 0,
            "state repository must be operator-owned and not group/world writable"
        );
    }
    ensure!(
        m.is_dir() || m.is_file(),
        "nonregular state repository metadata"
    );
    Ok(Some(m))
}

fn file(path: &Path, cap: usize) -> Result<Option<Vec<u8>>> {
    let Some(m) = metadata(path)? else {
        return Ok(None);
    };
    ensure!(
        m.is_file() && m.len() <= cap as u64,
        "invalid state metadata file"
    );
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut f = options
        .open(path)
        .map_err(|_| anyhow!("state metadata unreadable"))?;
    ensure!(f.metadata()?.is_file(), "invalid state metadata file");
    let mut bytes = Vec::new();
    (&mut f)
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("state metadata unreadable"))?;
    ensure!(bytes.len() <= cap, "state metadata size limit exceeded");
    Ok(Some(bytes))
}

fn scan(git: &GitSession, path: &Path, depth: usize, count: &mut usize) -> Result<()> {
    git.check()?;
    ensure!(
        depth <= 64 && *count < ENTRY_CAP,
        "state metadata traversal limit exceeded"
    );
    *count += 1;
    let Some(m) = metadata(path)? else {
        return Ok(());
    };
    if m.is_dir() {
        for entry in fs::read_dir(path).map_err(|_| anyhow!("state metadata unreadable"))? {
            let entry = entry.map_err(|_| anyhow!("state metadata unreadable"))?;
            scan(git, &entry.path(), depth + 1, count)?;
        }
    }
    Ok(())
}

fn repository(git: &GitSession) -> Result<usize> {
    git.check()?;
    let repo = git.repo();
    ensure!(
        metadata(repo)?.is_some_and(|m| m.is_dir()),
        "state repository must be a directory"
    );
    // Only the actual bare Git directory is accepted, never a worktree, .git
    // indirection, common-dir indirection, alternate object store or reftable.
    for name in ["objects", "refs", "info", "logs"] {
        scan(git, &repo.join(name), 0, &mut 0)?;
    }
    for name in [
        "commondir",
        "gitdir",
        "shallow",
        "objects/info/alternates",
        "objects/info/http-alternates",
    ] {
        ensure!(
            metadata(&repo.join(name))?.is_none(),
            "state repository indirection refused"
        );
    }
    for name in ["objects", "refs"] {
        ensure!(
            metadata(&repo.join(name))?.is_some_and(|m| m.is_dir()),
            "missing state object/ref directory"
        );
    }
    for name in ["config", "config.worktree", "HEAD", "packed-refs"] {
        file(&repo.join(name), OBJECT_CAP)?;
    }
    if let Some(m) = metadata(&repo.join("worktrees"))? {
        ensure!(m.is_dir(), "invalid worktree registry");
        ensure!(
            fs::read_dir(repo.join("worktrees"))?.next().is_none(),
            "registered worktrees refused, including unborn worktrees"
        );
    }
    ensure!(
        git.run(&["rev-parse", "--is-bare-repository"], b"", 1024)? == b"true\n",
        "dedicated bare state repository required"
    );
    let actual = git.run(&["rev-parse", "--absolute-git-dir"], b"", 8192)?;
    let mut expected = repo.as_os_str().as_encoded_bytes().to_vec();
    expected.push(b'\n');
    ensure!(actual == expected, "state Git directory mismatch");
    ensure!(
        git.run(&["rev-parse", "--show-ref-format"], b"", 1024)? == b"files\n",
        "state repository requires files ref backend"
    );
    match git
        .run(&["rev-parse", "--show-object-format=storage"], b"", 1024)?
        .as_slice()
    {
        b"sha1\n" => Ok(40),
        b"sha256\n" => Ok(64),
        _ => Err(anyhow!("unsupported state object format")),
    }
}

fn current(git: &GitSession, len: usize) -> Result<Option<String>> {
    // for-each-ref alone omits dangling symrefs; inspect the known loose path too.
    let loose = file(&git.repo().join(STATE_REF), 1024)?;
    let loose_oid = loose
        .as_ref()
        .map(|bytes| {
            let value = std::str::from_utf8(bytes)
                .ok()
                .and_then(|s| s.strip_suffix('\n'))
                .filter(|s| oid(s, len))
                .ok_or_else(|| anyhow!("symbolic or malformed state ref refused"))?;
            Ok::<_, anyhow::Error>(value.to_owned())
        })
        .transpose()?;
    let bytes = git.run(
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(symref)%00",
            "refs/heads",
        ],
        b"",
        GRAPH_CAP,
    )?;
    let mut found = None;
    for line in bytes.split(|b| *b == b'\n').filter(|s| !s.is_empty()) {
        let fields: Vec<_> = line.split(|b| *b == 0).collect();
        ensure!(
            fields.len() == 4 && fields[3].is_empty(),
            "invalid state ref query"
        );
        let name = std::str::from_utf8(fields[0]).map_err(|_| anyhow!("invalid ref encoding"))?;
        ensure!(
            !name.starts_with(&format!("{STATE_REF}/"))
                && !STATE_REF.starts_with(&format!("{name}/")),
            "state ref namespace collision"
        );
        if name == STATE_REF {
            let value =
                std::str::from_utf8(fields[1]).map_err(|_| anyhow!("invalid ref object ID"))?;
            ensure!(
                fields[2].is_empty() && oid(value, len) && found.is_none(),
                "symbolic or malformed state ref refused"
            );
            found = Some(value.to_owned());
        }
    }
    ensure!(
        loose_oid.is_none() || loose_oid == found,
        "state loose ref/query mismatch"
    );
    Ok(found)
}

impl Preparation {
    fn hash(&self, kind: &str, bytes: &[u8], write: bool) -> Result<String> {
        self.git.check()?;
        ensure!(
            bytes.len() <= OBJECT_CAP,
            "state object size limit exceeded"
        );
        let mut args = vec!["hash-object", "--no-filters", "-t", kind, "--stdin"];
        if write {
            args.push("-w");
        }
        let out = self.git.run(&args, bytes, 1024)?;
        let value = std::str::from_utf8(&out)
            .ok()
            .and_then(|s| s.strip_suffix('\n'))
            .filter(|s| oid(s, self.len))
            .ok_or_else(|| anyhow!("invalid computed state object ID"))?;
        Ok(value.into())
    }

    fn read(&mut self, kind: &str, id: &str) -> Result<Vec<u8>> {
        self.git.check()?;
        ensure!(oid(id, self.len), "invalid state object ID");
        // Batch framing proves the type, size and requested full ID. Reading a
        // missing/corrupt object is never interpreted as path/ref absence.
        let out = self.git.run(
            &["cat-file", "--batch"],
            format!("{id}\n").as_bytes(),
            OBJECT_CAP + 256,
        )?;
        let n = out
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| anyhow!("invalid state object framing"))?;
        let header =
            std::str::from_utf8(&out[..n]).map_err(|_| anyhow!("invalid state object header"))?;
        let fields: Vec<_> = header.split(' ').collect();
        ensure!(
            fields.len() == 3 && fields[0] == id && fields[1] == kind,
            "missing or incorrect state object type"
        );
        let size = fields[2]
            .parse::<usize>()
            .map_err(|_| anyhow!("invalid state object size"))?;
        ensure!(
            size <= OBJECT_CAP && out.len() == n + size + 2 && out.last() == Some(&b'\n'),
            "invalid state object size/framing"
        );
        self.bytes = self
            .bytes
            .checked_add(size)
            .ok_or_else(|| anyhow!("state graph limit exceeded"))?;
        ensure!(self.bytes <= GRAPH_CAP, "state graph limit exceeded");
        let bytes = out[n + 1..n + 1 + size].to_vec();
        ensure!(
            self.hash(kind, &bytes, false)? == id,
            "state object hash mismatch"
        );
        Ok(bytes)
    }

    fn object(&mut self, kind: &'static str, bytes: Vec<u8>) -> Result<String> {
        self.git.check()?;
        self.bytes += bytes.len();
        ensure!(self.bytes <= GRAPH_CAP, "state graph limit exceeded");
        let oid = self.hash(kind, &bytes, false)?;
        self.objects.push(Object {
            kind,
            bytes,
            oid: oid.clone(),
        });
        Ok(oid)
    }

    fn commit(&mut self, id: &str) -> Result<Commit> {
        self.git.check()?;
        let bytes = self.read("commit", id)?;
        let split = bytes
            .windows(2)
            .position(|w| w == b"\n\n")
            .ok_or_else(|| anyhow!("malformed state base commit"))?;
        let header =
            std::str::from_utf8(&bytes[..split]).map_err(|_| anyhow!("invalid commit headers"))?;
        ensure!(
            !header.contains('\0') && !header.contains('\r'),
            "malformed commit headers"
        );
        let mut lines = header.lines();
        let tree = lines
            .next()
            .and_then(|s| s.strip_prefix("tree "))
            .filter(|s| oid(s, self.len))
            .ok_or_else(|| anyhow!("invalid commit tree"))?
            .to_owned();
        let mut parents = Vec::new();
        let mut author = false;
        let mut time = None;
        for line in lines {
            if let Some(parent) = line.strip_prefix("parent ") {
                ensure!(
                    !author && oid(parent, self.len) && parents.len() < 64,
                    "invalid commit parent"
                );
                parents.push(parent.into());
            } else if line.starts_with("author ") {
                ensure!(!author && time.is_none(), "invalid commit author");
                parse_time(line)?;
                author = true;
            } else if line.starts_with("committer ") {
                ensure!(author && time.is_none(), "invalid commit committer");
                time = Some(parse_time(line)?);
            } else {
                ensure!(
                    time.is_some() && !line.starts_with("tree "),
                    "invalid commit header order"
                );
            }
        }
        ensure!(author, "missing commit author");
        let time = time.ok_or_else(|| anyhow!("missing commit committer"))?;
        Ok(Commit {
            tree,
            parents,
            time,
            message: bytes[split + 2..].to_vec(),
            bytes,
        })
    }

    fn tree(&mut self, id: &str, depth: usize) -> Result<Vec<Entry>> {
        self.git.check()?;
        ensure!(depth <= 64, "state tree depth limit exceeded");
        if let Some(entries) = self.trees.get(id) {
            return Ok(entries.clone());
        }
        let bytes = self.read("tree", id)?;
        let entries = parse_tree(&bytes, self.len)?;
        self.entries += entries.len();
        ensure!(self.entries <= ENTRY_CAP, "state tree entry limit exceeded");
        // A mode is not proof of object type. Validate local blobs without
        // materializing unrelated contents. Gitlinks belong to another repo.
        let blobs: Vec<_> = entries
            .iter()
            .filter(|e| matches!(e.mode.as_slice(), b"100644" | b"100755" | b"120000"))
            .collect();
        for chunk in blobs.chunks(8192) {
            let input: String = chunk.iter().map(|e| format!("{}\n", e.oid)).collect();
            let out = self.git.run(
                &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
                input.as_bytes(),
                OBJECT_CAP,
            )?;
            let expected: String = chunk.iter().map(|e| format!("{} blob\n", e.oid)).collect();
            ensure!(
                out == expected.as_bytes(),
                "missing or malformed base tree blob"
            );
        }
        for entry in &entries {
            self.git.check()?;
            if entry.mode == b"40000" {
                self.tree(&entry.oid, depth + 1)?;
            }
        }
        self.trees.insert(id.into(), entries.clone());
        Ok(entries)
    }

    fn replace(
        &mut self,
        tree: Option<&str>,
        path: &[&str],
        blob: &str,
        expected: Option<&str>,
        depth: usize,
    ) -> Result<String> {
        self.git.check()?;
        let mut entries = match tree {
            Some(id) => self.tree(id, depth)?,
            None => Vec::new(),
        };
        let pos = entries.iter().position(|e| e.name == path[0].as_bytes());
        let old = pos.map(|p| entries.remove(p));
        let (mode, id) = if path.len() == 1 {
            match (&old, expected) {
                (None, None) => {}
                (Some(e), Some(id))
                    if matches!(e.mode.as_slice(), b"100644" | b"100755") && e.oid == id => {}
                _ => return Err(anyhow!("ledger base path/blob condition failed")),
            }
            (b"100644".to_vec(), blob.to_owned())
        } else {
            ensure!(
                old.as_ref().is_none_or(|e| e.mode == b"40000"),
                "ledger parent must be a regular Git tree"
            );
            let id = self.replace(
                old.as_ref().map(|e| e.oid.as_str()),
                &path[1..],
                blob,
                expected,
                depth + 1,
            )?;
            (b"40000".to_vec(), id)
        };
        entries.push(Entry {
            mode,
            name: path[0].as_bytes().to_vec(),
            oid: id,
        });
        entries.sort_by_key(Entry::key);
        self.object("tree", encode_tree(&entries)?)
    }

    fn lookup(&mut self, tree: &str, path: &[&str]) -> Result<String> {
        let mut id = tree.to_owned();
        for (i, name) in path.iter().enumerate() {
            let entries = self.tree(&id, i)?;
            let e = entries
                .iter()
                .find(|e| e.name == name.as_bytes())
                .ok_or_else(|| anyhow!("owned ledger path missing"))?;
            ensure!(
                if i + 1 == path.len() {
                    e.mode == b"100644"
                } else {
                    e.mode == b"40000"
                },
                "invalid owned ledger mode"
            );
            id = e.oid.clone();
        }
        Ok(id)
    }

    fn ownership(
        &mut self,
        base: &Commit,
        ledger_path: &str,
        binding: &Binding,
        expected_blob: Option<&str>,
    ) -> Result<Option<AckOrigin>> {
        self.git.check()?;
        let (record, message, v2, v3) =
            if let Some(json) = base.message.strip_prefix(MARKER.as_bytes()) {
                let record: Record = serde_json::from_slice(json)
                    .map_err(|_| anyhow!("malformed state ownership receipt"))?;
                record.validate(self.len)?;
                let message = record.message()?;
                (record, message, None, None)
            } else if let Some(json) = base.message.strip_prefix(MARKER_V2.as_bytes()) {
                let record: OwnedRecordV2 = serde_json::from_slice(json)
                    .map_err(|_| anyhow!("malformed state ownership receipt"))?;
                record.validate(self.len)?;
                (record.common(), record.message()?, Some(record), None)
            } else if let Some(json) = base.message.strip_prefix(MARKER_V3.as_bytes()) {
                let record: OwnedRecordV3 = serde_json::from_slice(json)
                    .map_err(|_| anyhow!("malformed V3 state ownership receipt"))?;
                record.validate(self.len)?;
                (record.common(), record.message()?, None, Some(record))
            } else {
                return Err(anyhow!("foreign state base commit: owned receipt required"));
            };
        ensure!(
            message == base.message
                && record.path == ledger_path
                && &record.binding == binding
                && Some(record.ledger_blob_id.as_str()) == expected_blob
                && base.parents == [record.base_commit.clone()],
            "state ownership receipt mismatch"
        );
        let path: Vec<_> = ledger_path.split('/').collect();
        ensure!(
            self.lookup(&base.tree, &path)? == record.ledger_blob_id,
            "owned ledger tree/blob mismatch"
        );
        let ledger = self.read("blob", &record.ledger_blob_id)?;
        if let Some(record) = v2 {
            record.validate_ledger(&ledger)?;
        }
        let parent = self.commit(&record.base_commit)?;
        // Inspect just the immediate predecessor's typed metadata. Receipts in
        // this exclusively administered repository are not cryptographic proof;
        // recursively replaying its entire history is deliberately unnecessary.
        let parent_floor = if let Some(json) = parent.message.strip_prefix(MARKER_V3.as_bytes()) {
            let prior: OwnedRecordV3 =
                serde_json::from_slice(json).map_err(|_| anyhow!("malformed parent V3 receipt"))?;
            prior.validate(self.len)?;
            ensure!(
                prior.message()? == parent.message
                    && prior.path == record.path
                    && prior.binding == record.binding
                    && Some(&prior.ledger_blob_id) == record.expected_blob_id.as_ref()
                    && parent.parents == [prior.base_commit.clone()]
                    && self.lookup(&parent.tree, &path)? == prior.ledger_blob_id,
                "parent V3 receipt mismatch"
            );
            Some(prior.provenance_floor)
        } else {
            None
        };
        let floor = if let Some(v3) = &v3 {
            let old_blob = record
                .expected_blob_id
                .as_ref()
                .ok_or_else(|| anyhow!("V3 ledger base missing"))?;
            ensure!(
                self.lookup(&parent.tree, &path)? == *old_blob,
                "V3 parent ledger mismatch"
            );
            if parent_floor.is_none() {
                // V3 can only start with ACK on an already owned V1/V2 base.
                ensure!(
                    matches!(v3.proof, RecordProof::Acknowledgement(_)),
                    "V3 continuation requires V3 parent"
                );
                // This call is bounded: non-V3 parents cannot recurse through V3.
                self.ownership(&parent, &record.path, &record.binding, Some(old_blob))?;
            }
            let origin = match &v3.proof {
                RecordProof::Acknowledgement(a) => Some(a.origin),
                _ => None,
            };
            ensure!(
                v3.provenance_floor == inherited_floor(parent_floor, origin),
                "V3 provenance floor mismatch"
            );
            let old = self.read("blob", old_blob)?;
            v3.validate_transition(&old, &ledger)?;
            Some(v3.provenance_floor)
        } else {
            ensure!(
                parent_floor.is_none(),
                "state receipt reader floor downgrade refused"
            );
            None
        };
        let time = next_time(parent.time)?;
        ensure!(
            base.bytes == commit_bytes(&base.tree, &record.base_commit, time, &base.message),
            "noncanonical owned state commit"
        );
        // Prove that even an owned-looking commit changed only its ledger path.
        let n = self.objects.len();
        let expected_tree = self.replace(
            Some(&parent.tree),
            &path,
            &record.ledger_blob_id,
            record.expected_blob_id.as_deref(),
            0,
        )?;
        self.objects.truncate(n);
        ensure!(
            expected_tree == base.tree,
            "owned state commit has unrelated tree changes"
        );
        Ok(floor)
    }
}

fn parse_time(line: &str) -> Result<i64> {
    let (identity, tail) = line
        .rsplit_once("> ")
        .ok_or_else(|| anyhow!("invalid commit identity/time"))?;
    ensure!(identity.contains(" <"), "invalid commit identity");
    let (seconds, zone) = tail
        .split_once(' ')
        .ok_or_else(|| anyhow!("invalid commit time"))?;
    ensure!(
        !seconds.is_empty()
            && seconds.bytes().all(|b| b.is_ascii_digit())
            && zone.len() == 5
            && matches!(zone.as_bytes()[0], b'+' | b'-')
            && zone[1..].bytes().all(|b| b.is_ascii_digit())
            && zone[1..3].parse::<u8>()? <= 23
            && zone[3..].parse::<u8>()? <= 59,
        "invalid commit time"
    );
    let time: i64 = seconds
        .parse()
        .map_err(|_| anyhow!("invalid commit timestamp"))?;
    ensure!(
        (0..=253_402_300_799).contains(&time),
        "unsupported commit timestamp"
    );
    Ok(time)
}

fn next_time(time: i64) -> Result<i64> {
    ensure!(
        (0..253_402_300_799).contains(&time),
        "state commit timestamp overflow"
    );
    Ok(time + 1)
}

fn commit_bytes(tree: &str, parent: &str, time: i64, message: &[u8]) -> Vec<u8> {
    let mut bytes = format!("tree {tree}\nparent {parent}\nauthor {IDENTITY} {time} +0000\ncommitter {IDENTITY} {time} +0000\n\n").into_bytes();
    bytes.extend_from_slice(message);
    bytes
}

fn parse_tree(mut bytes: &[u8], len: usize) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    let mut previous = None;
    while !bytes.is_empty() {
        let space = bytes
            .iter()
            .position(|b| *b == b' ')
            .ok_or_else(|| anyhow!("malformed raw Git tree"))?;
        let mode = &bytes[..space];
        ensure!(
            matches!(
                mode,
                b"40000" | b"100644" | b"100755" | b"120000" | b"160000"
            ),
            "invalid raw Git tree mode"
        );
        bytes = &bytes[space + 1..];
        let nul = bytes
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| anyhow!("malformed raw Git tree name"))?;
        let name = &bytes[..nul];
        ensure!(
            !name.is_empty()
                && name.len() <= 4096
                && !name.contains(&b'/')
                && name != b"."
                && name != b".."
                && !name.eq_ignore_ascii_case(b".git")
                && names.insert(name.to_vec()),
            "invalid or duplicate raw Git tree name"
        );
        bytes = &bytes[nul + 1..];
        ensure!(bytes.len() >= len / 2, "truncated raw Git tree object ID");
        let id: String = bytes[..len / 2]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        ensure!(oid(&id, len), "zero raw Git tree object ID");
        bytes = &bytes[len / 2..];
        let e = Entry {
            mode: mode.to_vec(),
            name: name.to_vec(),
            oid: id,
        };
        let key = e.key();
        ensure!(
            previous.as_ref().is_none_or(|p| p < &key),
            "noncanonical raw Git tree order"
        );
        previous = Some(key);
        entries.push(e);
        ensure!(
            entries.len() <= ENTRY_CAP,
            "state tree entry limit exceeded"
        );
    }
    Ok(entries)
}

fn encode_tree(entries: &[Entry]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for e in entries {
        bytes.extend_from_slice(&e.mode);
        bytes.push(b' ');
        bytes.extend_from_slice(&e.name);
        bytes.push(0);
        for i in (0..e.oid.len()).step_by(2) {
            bytes.push(
                u8::from_str_radix(&e.oid[i..i + 2], 16)
                    .map_err(|_| anyhow!("invalid tree object ID"))?,
            );
        }
    }
    Ok(bytes)
}

fn prepare(
    repo: &Path,
    state_ref: &str,
    change: &PreparedLedgerChange,
    create_ref: bool,
    deadline: Instant,
    operation: &SourceSession,
) -> Result<(Preparation, ApplyReceipt)> {
    operation.check()?;
    ensure!(
        state_ref == STATE_REF,
        "only refs/heads/hive-calendar-state is supported"
    );
    ensure!(
        repo.is_absolute() && metadata(repo)?.is_some_and(|m| m.is_dir()),
        "state repository must be an absolute directory, not a symlink"
    );
    ensure!(valid_path(&change.path), "invalid ledger JSON path");
    ensure!(
        create_ref == change.expected_blob_id.is_none(),
        "create-ref must exactly match ledger initialization"
    );
    let (intent, audit) = match (&change.proof, change.operation) {
        (LedgerChangeProof::Source, StateOperation::Enroll | StateOperation::Reack) => (None, None),
        (
            LedgerChangeProof::Intent(proof),
            StateOperation::Prepare | StateOperation::MarkUncertain,
        ) => {
            ensure!(
                !create_ref
                    && change.expected_blob_id.is_some()
                    && !change.parser_change_accepted
                    && operation_uuid(&proof.operation_id)
                    && digest(&proof.intent_digest),
                "invalid state operation provider intent"
            );
            (Some(proof), None)
        }
        (LedgerChangeProof::Acknowledgement(proof), StateOperation::Acknowledge) => {
            proof.validate()?;
            ensure!(
                !create_ref && change.expected_blob_id.is_some() && !change.parser_change_accepted,
                "ACK requires an existing owned ledger"
            );
            (None, Some(proof.audit()))
        }
        _ => return Err(anyhow!("state operation proof mismatch")),
    };
    let git = GitSession::with_operation(repo, deadline, operation)?;
    let len = repository(&git)?;
    ensure!(
        oid(&change.base_commit, len),
        "base commit must be a full storage-format object ID"
    );
    let mut p = Preparation {
        git,
        len,
        trees: BTreeMap::new(),
        bytes: 0,
        entries: 0,
        objects: Vec::new(),
    };
    let base = p.commit(&change.base_commit)?;
    p.tree(&base.tree, 0)?;
    let parent_floor = if change.expected_blob_id.is_some() {
        p.ownership(
            &base,
            &change.path,
            &change.binding,
            change.expected_blob_id.as_deref(),
        )?
    } else {
        ensure!(
            !base
                .message
                .windows(b"hive-calendar-state".len())
                .any(|w| w.eq_ignore_ascii_case(b"hive-calendar-state")),
            "owned or malformed receipt cannot seed initialization"
        );
        None
    };
    let blob = p.object("blob", change.candidate_ledger.clone())?;
    let record = Record {
        schema_version: "1".into(),
        state_ref: state_ref.into(),
        path: change.path.clone(),
        binding: change.binding.clone(),
        ledger_blob_id: blob.clone(),
        candidate_sha256: change.candidate_sha256.clone(),
        source_commit: change.source_commit.clone(),
        operation: match change.operation {
            StateOperation::Enroll => "enroll",
            StateOperation::Reack => "reack",
            StateOperation::Prepare => "prepare",
            StateOperation::MarkUncertain => "mark_uncertain",
            StateOperation::Acknowledge => "acknowledge",
        }
        .into(),
        logical_id: change.logical_id.clone(),
        parser_change_accepted: change.parser_change_accepted,
        base_commit: change.base_commit.clone(),
        expected_blob_id: change.expected_blob_id.clone(),
    };
    let v3 = parent_floor.is_some() || audit.is_some();
    let floor = v3.then(|| inherited_floor(parent_floor, audit.map(|a| a.origin)));
    let message = if let Some(floor) = &floor {
        let proof = match (&change.proof, intent, audit) {
            (LedgerChangeProof::Source, _, _) => RecordProof::Source,
            (_, Some(p), _) => RecordProof::Intent(IntentRecord {
                operation_id: p.operation_id.clone(),
                intent_digest: p.intent_digest.clone(),
            }),
            (_, _, Some(a)) => RecordProof::Acknowledgement(Box::new(a.clone())),
            _ => unreachable!(),
        };
        let record = OwnedRecordV3::new(record, proof, *floor);
        record.validate(len)?;
        let old_blob = change
            .expected_blob_id
            .as_deref()
            .ok_or_else(|| anyhow!("V3 ledger base missing"))?;
        let old = p.read("blob", old_blob)?;
        record.validate_transition(&old, &change.candidate_ledger)?;
        record.message()?
    } else if let Some(proof) = intent {
        let record = OwnedRecordV2::new(record, &proof.operation_id, &proof.intent_digest);
        record.validate(len)?;
        record.validate_ledger(&change.candidate_ledger)?;
        record.message()?
    } else {
        record.validate(len)?;
        record.message()?
    };
    let path: Vec<_> = change.path.split('/').collect();
    let tree = p.replace(
        Some(&base.tree),
        &path,
        &blob,
        change.expected_blob_id.as_deref(),
        0,
    )?;
    let commit = p.object(
        "commit",
        commit_bytes(&tree, &change.base_commit, next_time(base.time)?, &message),
    )?;
    let tip = current(&p.git, len)?;
    let already = tip.as_deref() == Some(commit.as_str());
    ensure!(
        already
            || if create_ref {
                tip.is_none()
            } else {
                tip.as_deref() == Some(change.base_commit.as_str())
            },
        "state ref conflict: exact base head required; never rewind an advanced ref"
    );
    p.git.check()?;
    let receipt = ApplyReceipt {
        schema_version: if v3 {
            "3"
        } else if intent.is_some() {
            "2"
        } else {
            "1"
        },
        status: if already {
            "already_applied"
        } else {
            "verified"
        },
        state_ref: state_ref.into(),
        ledger_path: change.path.clone(),
        binding: change.binding.clone(),
        candidate_sha256: change.candidate_sha256.clone(),
        source_commit: change.source_commit.clone(),
        base_commit: change.base_commit.clone(),
        commit,
        ledger_blob_id: blob,
        operation: change.operation,
        logical_id: change.logical_id.clone(),
        parser_change_accepted: change.parser_change_accepted,
        local_ref_updated: false,
        publication_ready: false,
        remote_verified: false,
        remote_ref_verified: false,
        operation_id: intent
            .map(|p| p.operation_id.clone())
            .or_else(|| audit.map(|a| a.operation_id.clone())),
        intent_digest: intent
            .map(|p| p.intent_digest.clone())
            .or_else(|| audit.map(|a| a.intent_digest.clone())),
        ack_cause: audit.map(|a| a.cause),
        evidence_digest: audit.map(|a| a.evidence_digest.clone()),
        provenance_floor: floor,
    };
    Ok((p, receipt))
}

pub(crate) fn verify(
    repo: &Path,
    state_ref: &str,
    change: &PreparedLedgerChange,
    create_ref: bool,
    deadline: Instant,
    operation: &SourceSession,
) -> Result<ApplyReceipt> {
    let (_, receipt) = prepare(repo, state_ref, change, create_ref, deadline, operation)?;
    operation.check()?;
    Ok(receipt)
}

pub(crate) fn apply(
    repo: &Path,
    state_ref: &str,
    change: &PreparedLedgerChange,
    create_ref: bool,
    deadline: Instant,
    operation: &SourceSession,
) -> Result<ApplyReceipt> {
    let (p, mut receipt) = prepare(repo, state_ref, change, create_ref, deadline, operation)?;
    p.git.check()?;
    if receipt.status == "already_applied" {
        return Ok(receipt);
    }
    for object in &p.objects {
        ensure!(
            p.hash(object.kind, &object.bytes, true)? == object.oid,
            "written state object ID mismatch"
        );
    }
    ensure!(repository(&p.git)? == p.len, "state object format changed");
    let tip = current(&p.git, p.len)?;
    p.git.check()?;
    if tip.as_deref() == Some(receipt.commit.as_str()) {
        receipt.status = "already_applied";
        return Ok(receipt);
    }
    ensure!(
        if create_ref {
            tip.is_none()
        } else {
            tip.as_deref() == Some(change.base_commit.as_str())
        },
        "state ref conflict before CAS"
    );
    #[cfg(all(test, unix))]
    tests::observe_write(tests::WritePhase::BeforeCas)?;
    p.git.check()?;
    let old = if create_ref {
        "0".repeat(p.len)
    } else {
        change.base_commit.clone()
    };
    // From this point any transport error/cancellation may be a lost response.
    // Never roll back: an exact retry reconstructs the same commit and reports
    // already_applied, or refuses if another writer has subsequently advanced it.
    let outcome = p.git.run(
        &["update-ref", "--no-deref", STATE_REF, &receipt.commit, &old],
        b"",
        1024,
    );
    let outcome = outcome.and_then(|out| {
        #[cfg(all(test, unix))]
        tests::observe_write(tests::WritePhase::AfterCas)?;
        p.git.check()?;
        Ok(out)
    });
    outcome
        .map_err(|_| anyhow!("state ref CAS outcome unknown (possibly conflict or cancellation); retry the exact same candidate and options to determine already_applied; never roll back"))?;
    receipt.status = "applied";
    receipt.local_ref_updated = true;
    Ok(receipt)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use sha2::Digest;
    use std::{
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
        process::Command,
        time::Duration,
    };

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(120)
    }

    // Existing unit callers still own a root for the entire writer call.
    fn verify(
        repo: &Path,
        state_ref: &str,
        change: &PreparedLedgerChange,
        create_ref: bool,
        deadline: Instant,
    ) -> Result<ApplyReceipt> {
        let operation = SourceSession::operation(deadline)?;
        super::verify(repo, state_ref, change, create_ref, deadline, &operation)
    }

    fn apply(
        repo: &Path,
        state_ref: &str,
        change: &PreparedLedgerChange,
        create_ref: bool,
        deadline: Instant,
    ) -> Result<ApplyReceipt> {
        let operation = SourceSession::operation(deadline)?;
        super::apply(repo, state_ref, change, create_ref, deadline, &operation)
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum WritePhase {
        BeforeCas,
        AfterCas,
    }

    type Observer = Box<dyn FnMut(WritePhase) -> Result<()>>;
    thread_local! {
        static WRITE_OBSERVER: std::cell::RefCell<Option<Observer>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn observe_write(phase: WritePhase) -> Result<()> {
        WRITE_OBSERVER.with_borrow_mut(|observer| match observer.as_mut() {
            Some(f) => f(phase),
            None => Ok(()),
        })
    }

    fn observing<T>(
        observer: impl FnMut(WritePhase) -> Result<()> + 'static,
        run: impl FnOnce() -> T,
    ) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                WRITE_OBSERVER.with_borrow_mut(|o| *o = None);
            }
        }
        WRITE_OBSERVER.with_borrow_mut(|o| {
            assert!(o.is_none());
            *o = Some(Box::new(observer));
        });
        let _reset = Reset;
        run()
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        repo: PathBuf,
        base: String,
    }

    impl Fixture {
        fn new(format: &str) -> Self {
            let temp = tempfile::tempdir_in("/tmp").unwrap();
            let repo = temp.path().canonicalize().unwrap().join("state.git");
            let out = Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", temp.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "init",
                    "--bare",
                    "--quiet",
                    &format!("--object-format={format}"),
                ])
                .arg(&repo)
                .output()
                .unwrap();
            assert!(out.status.success());
            let mut f = Self {
                _temp: temp,
                repo,
                base: String::new(),
            };
            let json = f.write("blob", b"{}\n");
            let executable = f.write("blob", b"#!/bin/sh\nexit 0\n");
            let link = f.write("blob", b"empty.json");
            let subtree = f.write(
                "tree",
                &encode_tree(&[Entry {
                    mode: b"100644".to_vec(),
                    name: b"empty.json".to_vec(),
                    oid: json.clone(),
                }])
                .unwrap(),
            );
            let mut entries = vec![
                Entry {
                    mode: b"40000".to_vec(),
                    name: b"dir".to_vec(),
                    oid: subtree,
                },
                Entry {
                    mode: b"100644".to_vec(),
                    name: b"empty.json".to_vec(),
                    oid: json,
                },
                Entry {
                    mode: b"100755".to_vec(),
                    name: b"executable".to_vec(),
                    oid: executable,
                },
                Entry {
                    mode: b"120000".to_vec(),
                    name: b"link".to_vec(),
                    oid: link,
                },
            ];
            entries.sort_by_key(Entry::key);
            let tree = f.write("tree", &encode_tree(&entries).unwrap());
            f.base = f.seed(&tree, "seed fixture\n", 1_700_000_000);
            f.run(&["update-ref", "refs/heads/unrelated", &f.base], b"");
            f
        }

        fn run(&self, args: &[&str], input: &[u8]) -> Vec<u8> {
            GitSession::before(&self.repo, deadline())
                .unwrap()
                .run(args, input, GRAPH_CAP)
                .unwrap()
        }

        fn write(&self, kind: &str, bytes: &[u8]) -> String {
            String::from_utf8(self.run(
                &["hash-object", "--no-filters", "-w", "-t", kind, "--stdin"],
                bytes,
            ))
            .unwrap()
            .trim()
            .into()
        }

        fn seed(&self, tree: &str, message: &str, time: i64) -> String {
            self.write("commit", format!("tree {tree}\nauthor Fixture <fixture@example.invalid> {time} +0000\ncommitter Fixture <fixture@example.invalid> {time} +0000\n\n{message}").as_bytes())
        }

        fn change(&self) -> PreparedLedgerChange {
            PreparedLedgerChange {
                candidate_sha256: "a".repeat(64),
                operation: StateOperation::Enroll,
                logical_id: "0123456789abcdef0123456789abcdef".into(),
                source_commit: self.base.clone(),
                binding: Binding {
                    provider: "google_calendar".into(),
                    account_id: format!("google-sub-v1:{}", "1".repeat(64)),
                    calendar_id: "fixture-calendar".into(),
                    source_repo_id: "fixture-source".into(),
                },
                parser_change_accepted: false,
                base_commit: self.base.clone(),
                path: "state/nested/ledger.json".into(),
                expected_blob_id: None,
                candidate_ledger: b"{\"fixture\":1}\n".to_vec(),
                proof: LedgerChangeProof::Source,
            }
        }

        fn update(&self, receipt: &ApplyReceipt) -> PreparedLedgerChange {
            PreparedLedgerChange {
                base_commit: receipt.commit.clone(),
                expected_blob_id: Some(receipt.ledger_blob_id.clone()),
                candidate_sha256: "b".repeat(64),
                operation: StateOperation::Reack,
                candidate_ledger: b"{\"fixture\":2}\n".to_vec(),
                ..self.change()
            }
        }

        fn tip(&self) -> Option<String> {
            current(
                &GitSession::before(&self.repo, deadline()).unwrap(),
                self.base.len(),
            )
            .unwrap()
        }

        fn pending_change(&self, receipt: &ApplyReceipt) -> PreparedLedgerChange {
            let mut change = self.update(receipt);
            change.operation = StateOperation::Prepare;
            change.logical_id = "11111111-1111-4111-8111-111111111111".into();
            let source = crate::SourceAck {
                commit: change.source_commit.clone(),
                parser_sha256: "b".repeat(64),
                path: "tasks.md".into(),
                source_id: "tasks".into(),
                line: 1,
                mw_id: "tasks:1:0".into(),
                git_blob_id: "c".repeat(self.base.len()),
                source_revision: format!("sha256:{}", "d".repeat(64)),
                stable_id: None,
                expected: crate::ExpectedTask {
                    text: "Ship".into(),
                    literal_text: "- [ ] Ship due:2026-11-01".into(),
                    done: false,
                    due: "2026-11-01".into(),
                    raw: String::new(),
                    start: String::new(),
                    estimate: String::new(),
                },
            };
            let pending = crate::PendingOperation {
                operation_id: "22222222-2222-4222-8222-222222222222".into(),
                kind: crate::OperationKind::Create,
                phase: crate::OperationPhase::Prepared,
                intended_source: source.clone(),
                target_event_id: crate::operations::reserved_event_id(
                    &change.binding,
                    &change.logical_id,
                )
                .unwrap(),
                base_etag: None,
                desired_payload_hash: Some(crate::payload_hash(&crate::ProspectiveEvent {
                    summary: "Ship".into(),
                    start: crate::CalendarDate {
                        date: "2026-11-01".into(),
                    },
                    end: crate::CalendarDate {
                        date: "2026-11-02".into(),
                    },
                    availability: "free",
                    transparency: "transparent",
                })),
            };
            change.proof = LedgerChangeProof::Intent(crate::ProviderOperationProof {
                operation_id: pending.operation_id.clone(),
                intent_digest: crate::operations::intent_digest(
                    &change.binding,
                    &change.logical_id,
                    &pending,
                )
                .unwrap(),
            });
            change.candidate_ledger = serde_json::to_vec(&crate::Ledger {
                schema_version: "1".into(),
                binding: change.binding.clone(),
                source_watermark: source.commit.clone(),
                entries: vec![crate::LedgerEntry {
                    logical_id: change.logical_id.clone(),
                    lifecycle: crate::Lifecycle::Enrolled,
                    source_ack: source,
                    provider_ack: None,
                    pending: Some(pending),
                    tombstone: None,
                }],
            })
            .unwrap();
            change
        }

        fn ack_change(&self, receipt: &ApplyReceipt) -> PreparedLedgerChange {
            let bytes = self.run(&["cat-file", "blob", &receipt.ledger_blob_id], b"");
            let mut ledger = crate::ledger::decode(&bytes, &receipt.binding).unwrap();
            let entry = ledger
                .entries
                .iter_mut()
                .find(|e| e.logical_id == receipt.logical_id)
                .unwrap();
            let pending = entry.pending.take().unwrap();
            let provider_ack = crate::ProviderAck {
                event_id: pending.target_event_id.clone(),
                etag: "\"fixture-result\"".into(),
                last_payload_hash: pending.desired_payload_hash.clone().unwrap(),
            };
            let audit = AckAudit {
                schema_version: "1".into(),
                binding: receipt.binding.clone(),
                base_commit: receipt.commit.clone(),
                path: receipt.ledger_path.clone(),
                old_blob_id: receipt.ledger_blob_id.clone(),
                logical_id: receipt.logical_id.clone(),
                operation_id: pending.operation_id.clone(),
                intent_digest: crate::operations::intent_digest(
                    &receipt.binding,
                    &receipt.logical_id,
                    &pending,
                )
                .unwrap(),
                old_pending: pending.clone(),
                provider_ack: provider_ack.clone(),
                cause: if pending.phase == crate::OperationPhase::Prepared {
                    AckCause::MutationResponse
                } else {
                    AckCause::ReconciledObservation
                },
                origin: AckOrigin::SyntheticFixture,
                evidence_digest: String::new(),
            };
            let proof = ack::fixture_proof(audit).unwrap();
            entry.lifecycle = crate::Lifecycle::Active;
            entry.provider_ack = Some(provider_ack);
            PreparedLedgerChange {
                candidate_sha256: proof
                    .audit()
                    .evidence_digest
                    .strip_prefix("sha256:")
                    .unwrap()
                    .into(),
                operation: StateOperation::Acknowledge,
                logical_id: receipt.logical_id.clone(),
                source_commit: pending.intended_source.commit,
                binding: receipt.binding.clone(),
                parser_change_accepted: false,
                base_commit: receipt.commit.clone(),
                path: receipt.ledger_path.clone(),
                expected_blob_id: Some(receipt.ledger_blob_id.clone()),
                candidate_ledger: serde_json::to_vec(&ledger).unwrap(),
                proof: LedgerChangeProof::Acknowledgement(proof),
            }
        }

        fn continuation(&self, receipt: &ApplyReceipt) -> PreparedLedgerChange {
            let mut change = self.update(receipt);
            change.logical_id = receipt.logical_id.clone();
            change.source_commit = "e".repeat(self.base.len());
            let mut ledger = crate::ledger::decode(
                &self.run(&["cat-file", "blob", &receipt.ledger_blob_id], b""),
                &receipt.binding,
            )
            .unwrap();
            ledger.source_watermark = change.source_commit.clone();
            ledger
                .entries
                .iter_mut()
                .find(|e| e.logical_id == receipt.logical_id)
                .unwrap()
                .source_ack
                .commit = change.source_commit.clone();
            change.candidate_ledger = serde_json::to_vec(&ledger).unwrap();
            change
        }
    }

    #[test]
    fn v3_terminal_ack_preserves_evidence_and_retries_original_basis() {
        for format in ["sha1", "sha256"] {
            for update in [false, true] {
                for uncertain in [false, true] {
                    let f = Fixture::new(format);
                    let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
                    let mut prepare = f.pending_change(&first);
                    let mut ledger =
                        crate::ledger::decode(&prepare.candidate_ledger, &prepare.binding).unwrap();
                    if update {
                        let entry = &mut ledger.entries[0];
                        let pending = entry.pending.as_mut().unwrap();
                        pending.kind = crate::OperationKind::Update;
                        pending.base_etag = Some("\"fixture-old\"".into());
                        entry.lifecycle = crate::Lifecycle::Active;
                        entry.provider_ack = Some(crate::ProviderAck {
                            event_id: pending.target_event_id.clone(),
                            etag: "\"fixture-old\"".into(),
                            last_payload_hash: format!("sha256:{}", "e".repeat(64)),
                        });
                        prepare.proof = LedgerChangeProof::Intent(crate::ProviderOperationProof {
                            operation_id: pending.operation_id.clone(),
                            intent_digest: crate::operations::intent_digest(
                                &prepare.binding,
                                &prepare.logical_id,
                                pending,
                            )
                            .unwrap(),
                        });
                    }
                    let mut extra = ledger.entries[0].clone();
                    extra.logical_id = "33333333-3333-4333-8333-333333333333".into();
                    extra.lifecycle = crate::Lifecycle::Enrolled;
                    extra.pending = None;
                    extra.provider_ack = None;
                    extra.source_ack.line = 2;
                    extra.source_ack.mw_id = "tasks:2:0".into();
                    ledger.entries.push(extra);
                    prepare.candidate_ledger = serde_json::to_vec(&ledger).unwrap();
                    let mut prepared =
                        apply(&f.repo, STATE_REF, &prepare, false, deadline()).unwrap();
                    if uncertain {
                        let mut change = f.pending_change(&prepared);
                        change.operation = StateOperation::MarkUncertain;
                        ledger.entries[0].pending.as_mut().unwrap().phase =
                            crate::OperationPhase::Uncertain;
                        let pending = ledger.entries[0].pending.as_ref().unwrap();
                        change.proof = LedgerChangeProof::Intent(crate::ProviderOperationProof {
                            operation_id: pending.operation_id.clone(),
                            intent_digest: crate::operations::intent_digest(
                                &change.binding,
                                &change.logical_id,
                                pending,
                            )
                            .unwrap(),
                        });
                        change.candidate_ledger = serde_json::to_vec(&ledger).unwrap();
                        prepared = apply(&f.repo, STATE_REF, &change, false, deadline()).unwrap();
                    }
                    let change = f.ack_change(&prepared);
                    let before = snapshot(&f.repo);
                    let checked = verify(&f.repo, STATE_REF, &change, false, deadline()).unwrap();
                    assert_eq!(snapshot(&f.repo), before);
                    let error = observing(
                        |phase| {
                            if phase == WritePhase::AfterCas {
                                return Err(anyhow!("injected lost ACK response"));
                            }
                            Ok(())
                        },
                        || apply(&f.repo, STATE_REF, &change, false, deadline()),
                    )
                    .unwrap_err();
                    assert!(error.to_string().contains("CAS outcome unknown"));
                    assert_eq!(f.tip().as_deref(), Some(checked.commit.as_str()));
                    let before = snapshot(&f.repo);
                    let applied = apply(&f.repo, STATE_REF, &change, false, deadline()).unwrap();
                    assert_eq!(applied.status, "already_applied");
                    assert_eq!(applied.commit, checked.commit);
                    assert_eq!(applied.schema_version, "3");
                    assert_eq!(applied.provenance_floor, Some(AckOrigin::SyntheticFixture));
                    assert!(
                        !applied.remote_verified
                            && !applied.remote_ref_verified
                            && !applied.publication_ready
                    );
                    assert_eq!(snapshot(&f.repo), before);
                    let after =
                        crate::ledger::decode(&change.candidate_ledger, &change.binding).unwrap();
                    assert!(after.entries[0].pending.is_none());
                    assert_eq!(after.entries[1], ledger.entries[1]);
                    assert_eq!(after.entries[0].source_ack, ledger.entries[0].source_ack);
                    assert_eq!(after.source_watermark, ledger.source_watermark);
                    let json = serde_json::to_value(&applied).unwrap();
                    assert_eq!(json["provenanceFloor"], "synthetic_fixture");
                    assert!(json["ackCause"].is_string() && json["evidenceDigest"].is_string());
                    assert!(json["operationId"].is_string() && json["intentDigest"].is_string());
                    for path in ["dir", "empty.json", "executable", "link"] {
                        assert_eq!(
                            f.run(&["ls-tree", &f.base, "--", path], b""),
                            f.run(&["ls-tree", &applied.commit, "--", path], b"")
                        );
                    }
                    // Reopening ACK ownership must consult its parent's pending ledger.
                    let next = f.continuation(&applied);
                    let advanced = apply(&f.repo, STATE_REF, &next, false, deadline()).unwrap();
                    assert_eq!(advanced.schema_version, "3");
                    assert_eq!(advanced.provenance_floor, Some(AckOrigin::SyntheticFixture));
                    let before = snapshot(&f.repo);
                    assert!(apply(&f.repo, STATE_REF, &change, false, deadline()).is_err());
                    assert_eq!(snapshot(&f.repo), before);
                }
            }
        }
    }

    #[test]
    fn ack_transition_and_proof_mismatches_refuse_before_object_writes() {
        let f = Fixture::new("sha1");
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        let prepared = apply(
            &f.repo,
            STATE_REF,
            &f.pending_change(&first),
            false,
            deadline(),
        )
        .unwrap();
        for corruption in [
            "source_proof",
            "intent_proof",
            "wrong_operation",
            "source_commit",
            "parser",
            "initialize",
            "logical_id",
            "path",
            "old_blob",
            "phase",
            "audit_source",
            "result_etag",
            "result_hash",
            "source_ack",
            "watermark",
            "pending",
            "lifecycle",
            "tombstone",
            "candidate_digest",
        ] {
            let mut change = f.ack_change(&prepared);
            match corruption {
                "source_proof" => change.proof = LedgerChangeProof::Source,
                "intent_proof" => {
                    change.proof = LedgerChangeProof::Intent(crate::ProviderOperationProof {
                        operation_id: prepared.operation_id.clone().unwrap(),
                        intent_digest: prepared.intent_digest.clone().unwrap(),
                    })
                }
                "wrong_operation" => change.operation = StateOperation::Reack,
                "source_commit" => change.source_commit = "f".repeat(40),
                "parser" => change.parser_change_accepted = true,
                "initialize" => change.expected_blob_id = None,
                "logical_id" => change.logical_id = "33333333-3333-4333-8333-333333333333".into(),
                "path" => change.path = "other.json".into(),
                "old_blob" => change.expected_blob_id = Some("f".repeat(40)),
                "candidate_digest" => change.candidate_sha256 = "f".repeat(64),
                "phase" | "audit_source" => {
                    let LedgerChangeProof::Acknowledgement(proof) = &change.proof else {
                        unreachable!()
                    };
                    let mut audit = proof.audit().clone();
                    if corruption == "phase" {
                        audit.old_pending.phase = crate::OperationPhase::Uncertain;
                    } else {
                        audit.old_pending.intended_source.commit = "f".repeat(40);
                        audit.intent_digest = crate::operations::intent_digest(
                            &audit.binding,
                            &audit.logical_id,
                            &audit.old_pending,
                        )
                        .unwrap();
                        change.source_commit = audit.old_pending.intended_source.commit.clone();
                    }
                    let proof = ack::fixture_proof(audit).unwrap();
                    change.candidate_sha256 = proof
                        .audit()
                        .evidence_digest
                        .strip_prefix("sha256:")
                        .unwrap()
                        .into();
                    change.proof = LedgerChangeProof::Acknowledgement(proof);
                }
                _ => {
                    let mut ledger =
                        crate::ledger::decode(&change.candidate_ledger, &change.binding).unwrap();
                    let entry = &mut ledger.entries[0];
                    match corruption {
                        "result_etag" => {
                            entry.provider_ack.as_mut().unwrap().etag = "\"unexpected\"".into()
                        }
                        "result_hash" => {
                            entry.provider_ack.as_mut().unwrap().last_payload_hash =
                                format!("sha256:{}", "f".repeat(64))
                        }
                        "source_ack" => entry.source_ack.commit = "f".repeat(40),
                        "watermark" => ledger.source_watermark = "f".repeat(40),
                        "pending" => {
                            let LedgerChangeProof::Acknowledgement(proof) = &change.proof else {
                                unreachable!()
                            };
                            entry.pending = Some(proof.audit().old_pending.clone());
                        }
                        "lifecycle" => entry.lifecycle = crate::Lifecycle::Enrolled,
                        "tombstone" => {
                            entry.tombstone = Some(crate::Tombstone {
                                reason: "fixture".into(),
                                source_commit: change.source_commit.clone(),
                                operation_id: None,
                                remote_disposition: crate::RemoteDisposition::Unknown,
                            })
                        }
                        _ => unreachable!(),
                    }
                    change.candidate_ledger = serde_json::to_vec(&ledger).unwrap();
                }
            }
            let before = snapshot(&f.repo);
            assert!(
                apply(
                    &f.repo,
                    STATE_REF,
                    &change,
                    corruption == "initialize",
                    deadline()
                )
                .is_err(),
                "{corruption}"
            );
            assert_eq!(snapshot(&f.repo), before, "{corruption}");
        }
    }

    #[test]
    fn v3_source_and_intent_continuations_cannot_lower_the_reader_or_origin_floor() {
        let f = Fixture::new("sha256");
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        let pending = apply(
            &f.repo,
            STATE_REF,
            &f.pending_change(&first),
            false,
            deadline(),
        )
        .unwrap();
        let ack = apply(
            &f.repo,
            STATE_REF,
            &f.ack_change(&pending),
            false,
            deadline(),
        )
        .unwrap();
        let mut reack = f.continuation(&ack);
        let mut ledger = crate::ledger::decode(&reack.candidate_ledger, &reack.binding).unwrap();
        ledger.entries[0].source_ack.expected.text = "Ship again".into();
        ledger.entries[0].source_ack.expected.literal_text =
            "- [ ] Ship again due:2026-11-01".into();
        reack.candidate_ledger = serde_json::to_vec(&ledger).unwrap();
        let source = apply(&f.repo, STATE_REF, &reack, false, deadline()).unwrap();
        let mut enroll = f.continuation(&source);
        enroll.operation = StateOperation::Enroll;
        enroll.logical_id = "33333333-3333-4333-8333-333333333333".into();
        let mut extra = ledger.entries[0].clone();
        extra.logical_id = enroll.logical_id.clone();
        extra.source_ack.line = 2;
        extra.source_ack.mw_id = "tasks:2:0".into();
        extra.lifecycle = crate::Lifecycle::Enrolled;
        extra.provider_ack = None;
        ledger.entries.push(extra);
        enroll.candidate_ledger = serde_json::to_vec(&ledger).unwrap();
        let enrolled = apply(&f.repo, STATE_REF, &enroll, false, deadline()).unwrap();
        let mut prepare = f.pending_change(&enrolled);
        prepare.source_commit = source.source_commit.clone();
        let provider = ledger.entries[0].provider_ack.clone().unwrap();
        let event = crate::ProspectiveEvent {
            summary: "Ship again".into(),
            start: crate::CalendarDate {
                date: "2026-11-01".into(),
            },
            end: crate::CalendarDate {
                date: "2026-11-02".into(),
            },
            availability: "free",
            transparency: "transparent",
        };
        let pending = crate::PendingOperation {
            operation_id: "44444444-4444-4444-8444-444444444444".into(),
            kind: crate::OperationKind::Update,
            phase: crate::OperationPhase::Prepared,
            intended_source: ledger.entries[0].source_ack.clone(),
            target_event_id: provider.event_id,
            base_etag: Some(provider.etag),
            desired_payload_hash: Some(crate::payload_hash(&event)),
        };
        prepare.proof = LedgerChangeProof::Intent(crate::ProviderOperationProof {
            operation_id: pending.operation_id.clone(),
            intent_digest: crate::operations::intent_digest(
                &prepare.binding,
                &prepare.logical_id,
                &pending,
            )
            .unwrap(),
        });
        ledger.entries[0].pending = Some(pending);
        prepare.candidate_ledger = serde_json::to_vec(&ledger).unwrap();
        let prepared = apply(&f.repo, STATE_REF, &prepare, false, deadline()).unwrap();
        let mut uncertain = prepare;
        uncertain.base_commit = prepared.commit.clone();
        uncertain.expected_blob_id = Some(prepared.ledger_blob_id.clone());
        uncertain.operation = StateOperation::MarkUncertain;
        ledger.entries[0].pending.as_mut().unwrap().phase = crate::OperationPhase::Uncertain;
        uncertain.candidate_ledger = serde_json::to_vec(&ledger).unwrap();
        let uncertain = apply(&f.repo, STATE_REF, &uncertain, false, deadline()).unwrap();
        for receipt in [&source, &enrolled, &prepared, &uncertain] {
            assert_eq!(receipt.schema_version, "3");
            assert_eq!(receipt.provenance_floor, Some(AckOrigin::SyntheticFixture));
            assert!(receipt.ack_cause.is_none() && receipt.evidence_digest.is_none());
            let bytes = f.run(&["cat-file", "commit", &receipt.commit], b"");
            let split = bytes.windows(2).position(|w| w == b"\n\n").unwrap();
            let message = &bytes[split + 2..];
            assert!(message.strip_prefix(MARKER.as_bytes()).is_none());
            assert!(message.strip_prefix(MARKER_V2.as_bytes()).is_none());
            let record: OwnedRecordV3 =
                serde_json::from_slice(message.strip_prefix(MARKER_V3.as_bytes()).unwrap())
                    .unwrap();
            assert_eq!(record.provenance_floor, AckOrigin::SyntheticFixture);
        }
        // Forged source continuation cannot launder a synthetic ACK branch.
        let original = f.run(&["cat-file", "commit", &source.commit], b"");
        let split = original.windows(2).position(|w| w == b"\n\n").unwrap();
        let mut record: OwnedRecordV3 = serde_json::from_slice(
            original[split + 2..]
                .strip_prefix(MARKER_V3.as_bytes())
                .unwrap(),
        )
        .unwrap();
        record.provenance_floor = AckOrigin::GoogleAuthenticated;
        let mut bytes = original[..split + 2].to_vec();
        bytes.extend(record.message().unwrap());
        let forged = f.write("commit", &bytes);
        f.run(&["update-ref", STATE_REF, &forged], b"");
        let mut next = f.continuation(&source);
        next.base_commit = forged;
        let before = snapshot(&f.repo);
        assert!(apply(&f.repo, STATE_REF, &next, false, deadline()).is_err());
        assert_eq!(snapshot(&f.repo), before);

        // A V1-looking downgrade of a V3 continuation also refuses.
        let mut common = record.common();
        common.operation = "reack".into();
        let mut bytes = original[..split + 2].to_vec();
        bytes.extend(common.message().unwrap());
        let forged = f.write("commit", &bytes);
        f.run(&["update-ref", STATE_REF, &forged], b"");
        next.base_commit = forged;
        let before = snapshot(&f.repo);
        assert!(apply(&f.repo, STATE_REF, &next, false, deadline()).is_err());
        assert_eq!(snapshot(&f.repo), before);
    }

    #[test]
    fn malformed_or_forged_v3_audits_are_not_owned() {
        let f = Fixture::new("sha1");
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        let pending = apply(
            &f.repo,
            STATE_REF,
            &f.pending_change(&first),
            false,
            deadline(),
        )
        .unwrap();
        let applied = apply(
            &f.repo,
            STATE_REF,
            &f.ack_change(&pending),
            false,
            deadline(),
        )
        .unwrap();
        let original = f.run(&["cat-file", "commit", &applied.commit], b"");
        let split = original.windows(2).position(|w| w == b"\n\n").unwrap();
        let json = original[split + 2..]
            .strip_prefix(MARKER_V3.as_bytes())
            .unwrap();
        for corruption in [
            "floor",
            "evidence",
            "phase",
            "event",
            "etag",
            "hash",
            "operation_id",
            "source_commit",
            "audit_base",
            "audit_path",
            "audit_blob",
            "unknown_origin",
            "unknown_field",
            "duplicate",
            "version",
            "proof_operation",
        ] {
            let mut record: OwnedRecordV3 = serde_json::from_slice(json).unwrap();
            let RecordProof::Acknowledgement(audit) = &mut record.proof else {
                unreachable!()
            };
            match corruption {
                "floor" => record.provenance_floor = AckOrigin::GoogleAuthenticated,
                "evidence" => audit.evidence_digest = format!("sha256:{}", "f".repeat(64)),
                "phase" => audit.old_pending.phase = crate::OperationPhase::Uncertain,
                "event" => audit.provider_ack.event_id = "abcde12345".into(),
                "etag" => audit.provider_ack.etag = "W/\"weak\"".into(),
                "hash" => {
                    audit.provider_ack.last_payload_hash = format!("sha256:{}", "f".repeat(64))
                }
                "operation_id" => {
                    audit.operation_id = "33333333-3333-4333-8333-333333333333".into()
                }
                "source_commit" => record.source_commit = "f".repeat(40),
                "audit_base" => audit.base_commit = "f".repeat(40),
                "audit_path" => audit.path = "other.json".into(),
                "audit_blob" => audit.old_blob_id = "f".repeat(40),
                "version" => record.schema_version = "4".into(),
                "proof_operation" => record.operation = "prepare".into(),
                "unknown_origin" | "unknown_field" | "duplicate" => {}
                _ => unreachable!(),
            }
            if corruption != "evidence" {
                audit.evidence_digest = ack::evidence_digest(audit).unwrap();
                record.candidate_sha256 = audit
                    .evidence_digest
                    .strip_prefix("sha256:")
                    .unwrap()
                    .into();
            }
            let mut message = record.message().unwrap();
            if corruption == "unknown_origin" {
                message = String::from_utf8(message)
                    .unwrap()
                    .replace("synthetic_fixture", "unknown_origin")
                    .into_bytes();
            } else if matches!(corruption, "unknown_field" | "duplicate") {
                let extra = if corruption == "duplicate" {
                    ",\"schemaVersion\":\"3\""
                } else {
                    ",\"unrecognized\":true"
                };
                message.splice(message.len() - 2..message.len() - 2, extra.bytes());
            }
            let mut bytes = original[..split + 2].to_vec();
            bytes.extend(message);
            let forged = f.write("commit", &bytes);
            f.run(&["update-ref", STATE_REF, &forged], b"");
            let mut next = f.continuation(&applied);
            next.base_commit = forged;
            let before = snapshot(&f.repo);
            assert!(
                apply(&f.repo, STATE_REF, &next, false, deadline()).is_err(),
                "{corruption}"
            );
            assert_eq!(snapshot(&f.repo), before, "{corruption}");
        }
    }

    #[test]
    fn provenance_floor_is_monotone_and_fixture_authority_stays_synthetic() {
        for parent in [
            None,
            Some(AckOrigin::GoogleAuthenticated),
            Some(AckOrigin::SyntheticFixture),
        ] {
            for origin in [
                None,
                Some(AckOrigin::GoogleAuthenticated),
                Some(AckOrigin::SyntheticFixture),
            ] {
                let expected = if parent == Some(AckOrigin::SyntheticFixture)
                    || origin == Some(AckOrigin::SyntheticFixture)
                {
                    AckOrigin::SyntheticFixture
                } else {
                    AckOrigin::GoogleAuthenticated
                };
                assert_eq!(inherited_floor(parent, origin), expected);
            }
        }
        let f = Fixture::new("sha1");
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        let prepared = apply(
            &f.repo,
            STATE_REF,
            &f.pending_change(&first),
            false,
            deadline(),
        )
        .unwrap();
        let change = f.ack_change(&prepared);
        let LedgerChangeProof::Acknowledgement(proof) = change.proof else {
            unreachable!()
        };
        let boxed = RecordProof::Acknowledgement(Box::new(proof.audit().clone()));
        let expected = [
            b"{\"kind\":\"Acknowledgement\",\"value\":".as_slice(),
            serde_json::to_vec(proof.audit()).unwrap().as_slice(),
            b"}",
        ]
        .concat();
        assert_eq!(serde_json::to_vec(&boxed).unwrap(), expected);
        let decoded: RecordProof = serde_json::from_slice(&expected).unwrap();
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), expected);
        let mut metadata = proof.audit().clone();
        // Raw metadata only, never Google sealed authority or an HTTP fixture.
        metadata.origin = AckOrigin::GoogleAuthenticated;
        metadata.evidence_digest = ack::evidence_digest(&metadata).unwrap();
        let fixture = ack::fixture_proof(metadata).unwrap();
        assert_eq!(fixture.audit().origin, AckOrigin::SyntheticFixture);
        fixture.validate().unwrap();
    }

    fn snapshot(path: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, map: &mut BTreeMap<PathBuf, (u32, Vec<u8>)>) {
            let m = fs::symlink_metadata(path).unwrap();
            let data = if m.file_type().is_symlink() {
                fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else if m.is_file() {
                fs::read(path).unwrap()
            } else {
                Vec::new()
            };
            map.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (m.permissions().mode(), data),
            );
            if m.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), map);
                }
            }
        }
        let mut map = BTreeMap::new();
        visit(path, path, &mut map);
        map
    }

    #[test]
    #[ignore = "internal subprocess fixture, invoked by the phase-boundary signal test"]
    fn root_cancellation_phase_boundary_child() {
        let control = PathBuf::from(std::env::var_os("HIVE_STATE_CANCEL_CONTROL").unwrap());
        let f = Fixture::new("sha1");
        let end = deadline();
        let operation = SourceSession::operation(end).unwrap();
        // Each helper has returned and unregistered its own actions. The root
        // must still catch a signal in the CPU/phase gap before a writer exists.
        crate::source::require_json_absent_before(&f.repo, &f.base, "missing.json", end).unwrap();
        crate::source::read_json_blob_before(&f.repo, &f.base, "empty.json", end).unwrap();
        crate::source::require_ancestor_before(&f.repo, &f.base, &f.base, end).unwrap();
        let before = snapshot(&f.repo);
        fs::write(control.join("ready"), b"helpers returned").unwrap();
        while !control.join("resume").exists() {
            assert!(Instant::now() < end, "parent never released phase pause");
            std::thread::sleep(Duration::from_millis(2));
        }
        // Delivery is asynchronous with the parent's release-file write.
        while operation.check().is_ok() {
            assert!(
                Instant::now() < end,
                "root missed the phase-boundary signal"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let change = f.change();
        let checked =
            super::verify(&f.repo, STATE_REF, &change, true, end, &operation).unwrap_err();
        let applied = super::apply(&f.repo, STATE_REF, &change, true, end, &operation).unwrap_err();
        assert_eq!(checked.to_string(), "source operation cancelled");
        assert_eq!(applied.to_string(), "source operation cancelled");
        assert_eq!(
            snapshot(&f.repo),
            before,
            "cancelled phase wrote objects or refs"
        );
        drop(operation);
        fs::write(control.join("finished"), b"cancelled without writes").unwrap();
    }

    #[test]
    fn sigint_and_sigterm_between_helpers_and_writer_are_not_lost() {
        use std::process::{Child, Stdio};
        struct ChildGuard(Option<Child>);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                if let Some(child) = &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let control = tempfile::tempdir_in("/tmp").unwrap();
            let mut child = ChildGuard(Some(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "state_git::tests::root_cancellation_phase_boundary_child",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env_clear()
                    .env("HIVE_STATE_CANCEL_CONTROL", control.path())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            ));
            let end = Instant::now() + Duration::from_secs(20);
            while !control.path().join("ready").exists() {
                assert!(
                    Instant::now() < end,
                    "phase-boundary child did not reach pause"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            // Only our own unreaped child is signaled, never the test runner.
            assert_eq!(
                unsafe { libc::kill(child.0.as_ref().unwrap().id() as libc::pid_t, signal) },
                0
            );
            fs::write(control.path().join("resume"), b"continue").unwrap();
            let status = loop {
                if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
                    child.0.take();
                    break status;
                }
                assert!(
                    Instant::now() < end,
                    "phase-boundary child failed to unwind"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(status.success(), "phase-boundary cancellation child failed");
            assert_eq!(
                fs::read(control.path().join("finished")).unwrap(),
                b"cancelled without writes"
            );
        }
    }

    #[test]
    fn root_cancellation_at_pre_cas_stops_the_actual_writer() {
        for initialize in [true, false] {
            let f = Fixture::new("sha1");
            let change = if initialize {
                f.change()
            } else {
                let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
                f.update(&first)
            };
            let expected = verify(&f.repo, STATE_REF, &change, initialize, deadline()).unwrap();
            let old_tip = f.tip();
            let end = deadline();
            let operation = SourceSession::operation(end).unwrap();
            let cancellation = operation.child(end).unwrap();
            let reached = std::rc::Rc::new(std::cell::Cell::new(false));
            let observed = reached.clone();
            let error = observing(
                move |phase| {
                    assert!(
                        phase == WritePhase::BeforeCas,
                        "CAS ran after root cancellation"
                    );
                    observed.set(true);
                    cancellation.cancel();
                    Ok(())
                },
                || super::apply(&f.repo, STATE_REF, &change, initialize, end, &operation),
            )
            .unwrap_err();
            assert!(reached.get());
            assert_eq!(error.to_string(), "source operation cancelled");
            assert!(operation.check().is_err());
            assert_eq!(f.tip(), old_tip, "cancelled writer performed CAS");
            // Object writes really completed before the pre-CAS observation.
            assert_eq!(
                f.run(&["cat-file", "-t", &expected.commit], b""),
                b"commit\n"
            );
        }
    }

    #[test]
    fn cancellation_after_a_real_successful_cas_reports_unknown_and_exact_retry() {
        let f = Fixture::new("sha1");
        let change = f.change();
        let expected = verify(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
        let end = deadline();
        let operation = SourceSession::operation(end).unwrap();
        let cancellation = operation.child(end).unwrap();
        let won = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = won.clone();
        let error = observing(
            move |phase| {
                if phase == WritePhase::AfterCas {
                    // The real hardened Git transport has already returned success:
                    // the actual transaction won. Inject only its lost response.
                    observed.set(true);
                    cancellation.cancel();
                }
                Ok(())
            },
            || super::apply(&f.repo, STATE_REF, &change, true, end, &operation),
        )
        .unwrap_err();
        assert!(won.get());
        assert!(operation.check().is_err());
        assert!(error.to_string().contains("CAS outcome unknown"));
        assert!(error.to_string().contains("retry the exact same candidate"));
        assert_eq!(f.tip().as_deref(), Some(expected.commit.as_str()));
        let before = snapshot(&f.repo);
        let retry = apply(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
        assert_eq!(retry.status, "already_applied");
        assert!(!retry.local_ref_updated);
        assert_eq!(snapshot(&f.repo), before);
    }

    #[test]
    fn v2_pending_receipts_reopen_retry_and_never_rewind() {
        for format in ["sha1", "sha256"] {
            let f = Fixture::new(format);
            let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
            let mut prepare = f.pending_change(&first);
            // Preparing from a newer snapshot does not rewrite the frozen ack.
            prepare.source_commit = "d".repeat(f.base.len());
            let before = snapshot(&f.repo);
            let checked = verify(&f.repo, STATE_REF, &prepare, false, deadline()).unwrap();
            assert_eq!(snapshot(&f.repo), before);
            let prepared = apply(&f.repo, STATE_REF, &prepare, false, deadline()).unwrap();
            assert_eq!(prepared.commit, checked.commit);
            assert_eq!(prepared.schema_version, "2");
            assert!(prepared.operation_id.is_some() && prepared.intent_digest.is_some());
            let bytes = f.run(&["cat-file", "commit", &prepared.commit], b"");
            assert!(
                bytes
                    .windows(MARKER_V2.len())
                    .any(|w| w == MARKER_V2.as_bytes())
            );
            let candidate = crate::mark_uncertain(&crate::UncertainOptions {
                binding: prepare.binding.clone(),
                ledger: crate::LedgerLocation {
                    repository: f.repo.clone(),
                    commit: prepared.commit.clone(),
                    path: prepare.path.clone(),
                },
                entry: crate::EntrySelection::LogicalId(prepare.logical_id.clone()),
                operation_id: prepared.operation_id.clone().unwrap(),
            })
            .unwrap();
            let bytes = serde_json::to_vec(&candidate).unwrap();
            let candidate_path = f.repo.parent().unwrap().join("candidate.json");
            fs::write(&candidate_path, &bytes).unwrap();
            let options = crate::ApplyOptions {
                source: crate::PreviewOptions {
                    repository: "/unavailable/source".into(),
                    commit: candidate.source_commit.clone(),
                    mw_binary: "/unavailable/mw".into(),
                },
                binding: prepare.binding.clone(),
                ledger_repository: f.repo.clone(),
                ledger_path: prepare.path.clone(),
                state_ref: STATE_REF.into(),
                candidate_path,
                expected_candidate_sha256: format!("{:x}", sha2::Sha256::digest(&bytes)),
                create_ref: false,
                accept_parser_change: false,
            };
            let end = deadline();
            let operation = SourceSession::operation(end).unwrap();
            let uncertain = crate::approval::validate(&options, end, &operation).unwrap();
            assert_eq!(uncertain.operation, StateOperation::MarkUncertain);
            let applied = apply(&f.repo, STATE_REF, &uncertain, false, deadline()).unwrap();
            let before = snapshot(&f.repo);
            let retry = crate::apply_candidate(&options).unwrap();
            assert_eq!(retry.status, "already_applied");
            assert!(
                !retry.local_ref_updated
                    && !retry.publication_ready
                    && !retry.remote_verified
                    && !retry.remote_ref_verified
            );
            assert!(apply(&f.repo, STATE_REF, &prepare, false, deadline()).is_err());
            assert_eq!(snapshot(&f.repo), before);
            // Ownership of a V2 tip can also be read before a legacy operation.
            verify(&f.repo, STATE_REF, &f.update(&applied), false, deadline()).unwrap();
        }
    }

    #[test]
    fn forged_v2_ownership_receipts_refuse_before_writes() {
        let f = Fixture::new("sha1");
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        let change = f.pending_change(&first);
        let prepared = apply(&f.repo, STATE_REF, &change, false, deadline()).unwrap();
        let original = f.run(&["cat-file", "commit", &prepared.commit], b"");
        let split = original.windows(2).position(|w| w == b"\n\n").unwrap();
        let json = original[split + 2..]
            .strip_prefix(MARKER_V2.as_bytes())
            .unwrap();
        let original_record: serde_json::Value = serde_json::from_slice(json).unwrap();
        for corruption in [
            "version",
            "marker",
            "operation",
            "digest_format",
            "digest_value",
            "operation_id",
            "selected_entry",
            "phase",
            "parser",
            "expected_blob",
            "unknown",
            "duplicate",
        ] {
            let mut record: OwnedRecordV2 =
                serde_json::from_value(original_record.clone()).unwrap();
            match corruption {
                "version" => record.schema_version = "3".into(),
                "operation" => record.operation = "acknowledge".into(),
                "digest_format" => record.intent_digest = "a".repeat(64),
                "digest_value" => record.intent_digest = format!("sha256:{}", "a".repeat(64)),
                "operation_id" => {
                    record.operation_id = "33333333-3333-4333-8333-333333333333".into()
                }
                "selected_entry" => {
                    record.logical_id = "33333333-3333-4333-8333-333333333333".into()
                }
                "phase" => record.operation = "mark_uncertain".into(),
                "parser" => record.parser_change_accepted = true,
                "expected_blob" => record.expected_blob_id = None,
                "marker" | "unknown" | "duplicate" => {}
                _ => unreachable!(),
            }
            let mut message = record.message().unwrap();
            match corruption {
                "marker" => message[MARKER_V2.len() - 3] = b'3',
                "unknown" | "duplicate" => {
                    let key = if corruption == "unknown" {
                        "privateUnknown"
                    } else {
                        "operationId"
                    };
                    let extra = format!(",\"{key}\":\"private-value\"");
                    message.splice(message.len() - 2..message.len() - 2, extra.bytes());
                }
                _ => {}
            }
            let mut bytes = original[..split + 2].to_vec();
            bytes.extend(message);
            let forged = f.write("commit", &bytes);
            f.run(&["update-ref", STATE_REF, &forged], b"");
            let mut next = f.update(&prepared);
            next.base_commit = forged;
            let before = snapshot(&f.repo);
            let error = apply(&f.repo, STATE_REF, &next, false, deadline()).unwrap_err();
            assert!(!format!("{error:#}").contains("private"), "{corruption}");
            assert_eq!(snapshot(&f.repo), before, "{corruption}");
        }
    }

    #[test]
    fn invalid_operation_proofs_and_pending_bindings_refuse_before_writes() {
        let f = Fixture::new("sha1");
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        for corruption in [
            "missing",
            "uuid",
            "digest",
            "other_digest",
            "operation_id",
            "phase",
            "entry",
            "parser",
            "initialize",
            "legacy",
            "event",
            "payload",
            "delete",
        ] {
            let mut change = f.pending_change(&first);
            let LedgerChangeProof::Intent(proof) = &mut change.proof else {
                unreachable!()
            };
            match corruption {
                "missing" => change.proof = LedgerChangeProof::Source,
                "uuid" => proof.operation_id = "invalid".into(),
                "digest" => proof.intent_digest = "a".repeat(64),
                "other_digest" => proof.intent_digest = format!("sha256:{}", "a".repeat(64)),
                "operation_id" => {
                    proof.operation_id = "33333333-3333-4333-8333-333333333333".into()
                }
                "phase" => change.operation = StateOperation::MarkUncertain,
                "entry" => change.logical_id = "33333333-3333-4333-8333-333333333333".into(),
                "parser" => change.parser_change_accepted = true,
                "initialize" => change.expected_blob_id = None,
                "legacy" => change.operation = StateOperation::Reack,
                "event" | "payload" | "delete" => {
                    let mut ledger =
                        crate::ledger::decode(&change.candidate_ledger, &change.binding).unwrap();
                    let pending = ledger.entries[0].pending.as_mut().unwrap();
                    match corruption {
                        "event" => pending.target_event_id = "abcde12345".into(),
                        "payload" => {
                            pending.desired_payload_hash =
                                Some(format!("sha256:{}", "e".repeat(64)))
                        }
                        "delete" => {
                            pending.kind = crate::OperationKind::Delete;
                            pending.desired_payload_hash = None;
                            pending.base_etag = Some("etag".into());
                        }
                        _ => unreachable!(),
                    }
                    proof.intent_digest = crate::operations::intent_digest(
                        &change.binding,
                        &change.logical_id,
                        pending,
                    )
                    .unwrap();
                    if corruption == "delete" {
                        let entry = &mut ledger.entries[0];
                        entry.lifecycle = crate::Lifecycle::Active;
                        entry.provider_ack = Some(crate::ProviderAck {
                            event_id: entry.pending.as_ref().unwrap().target_event_id.clone(),
                            etag: "etag".into(),
                            last_payload_hash: format!("sha256:{}", "e".repeat(64)),
                        });
                    }
                    change.candidate_ledger = serde_json::to_vec(&ledger).unwrap();
                }
                _ => unreachable!(),
            }
            let before = snapshot(&f.repo);
            assert!(
                apply(
                    &f.repo,
                    STATE_REF,
                    &change,
                    corruption == "initialize",
                    deadline()
                )
                .is_err(),
                "{corruption}"
            );
            assert_eq!(snapshot(&f.repo), before, "{corruption}");
        }
    }

    #[test]
    fn initialize_reack_exact_retry_and_verify_are_deterministic_and_read_only() {
        for format in ["sha1", "sha256"] {
            let f = Fixture::new(format);
            let change = f.change();
            let before = snapshot(&f.repo);
            let verified = verify(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
            assert_eq!(verified.status, "verified");
            assert!(!verified.local_ref_updated);
            assert_eq!(snapshot(&f.repo), before);
            let again = verify(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
            assert_eq!(again.commit, verified.commit);
            let applied = apply(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
            assert_eq!(applied.commit, verified.commit);
            assert_eq!(applied.status, "applied");
            assert!(applied.local_ref_updated);
            assert_eq!(f.tip().as_deref(), Some(applied.commit.as_str()));
            assert_eq!(
                f.run(&["rev-parse", "refs/heads/unrelated"], b""),
                format!("{}\n", f.base).as_bytes()
            );
            assert_eq!(
                f.run(
                    &[
                        "ls-tree",
                        &f.base,
                        "--",
                        "dir",
                        "empty.json",
                        "executable",
                        "link"
                    ],
                    b""
                ),
                f.run(
                    &[
                        "ls-tree",
                        &applied.commit,
                        "--",
                        "dir",
                        "empty.json",
                        "executable",
                        "link"
                    ],
                    b""
                )
            );
            assert_eq!(
                f.run(
                    &[
                        "cat-file",
                        "blob",
                        &format!("{}:{}", applied.commit, change.path)
                    ],
                    b""
                ),
                change.candidate_ledger
            );
            let after = snapshot(&f.repo);
            for retry in [
                verify(&f.repo, STATE_REF, &change, true, deadline()).unwrap(),
                apply(&f.repo, STATE_REF, &change, true, deadline()).unwrap(),
            ] {
                assert_eq!(retry.status, "already_applied");
                assert!(!retry.local_ref_updated);
                assert!(
                    !retry.publication_ready
                        && !retry.remote_verified
                        && !retry.remote_ref_verified
                );
            }
            assert_eq!(snapshot(&f.repo), after);
            let update = f.update(&applied);
            let checked = verify(&f.repo, STATE_REF, &update, false, deadline()).unwrap();
            assert_eq!(snapshot(&f.repo), after);
            let updated = apply(&f.repo, STATE_REF, &update, false, deadline()).unwrap();
            assert_eq!(updated.commit, checked.commit);
            assert!(apply(&f.repo, STATE_REF, &change, true, deadline()).is_err());
            assert_eq!(
                apply(&f.repo, STATE_REF, &update, false, deadline())
                    .unwrap()
                    .status,
                "already_applied"
            );
            assert!(!f.repo.join("index").exists());
            assert_eq!(
                fs::read(f.repo.join("config")).unwrap(),
                before[Path::new("config")].1
            );
            let receipt = serde_json::to_string(&updated).unwrap();
            assert!(receipt.starts_with("{\"schemaVersion\":\"1\","));
            assert!(!receipt.contains("operationId") && !receipt.contains("intentDigest"));
            assert!(receipt.contains("\"remoteRefVerified\":false"));
            assert!(!receipt.contains("fixture\\\":2"));
        }
    }

    #[test]
    fn stale_head_is_refused_even_with_identical_ledger_and_foreign_heads_are_never_adopted() {
        let f = Fixture::new("sha1");
        let change = f.change();
        f.run(&["update-ref", STATE_REF, &f.base], b"");
        let before = snapshot(&f.repo);
        assert!(apply(&f.repo, STATE_REF, &change, true, deadline()).is_err());
        assert_eq!(snapshot(&f.repo), before);
        f.run(&["update-ref", "-d", STATE_REF], b"");
        let first = apply(&f.repo, STATE_REF, &change, true, deadline()).unwrap();
        let mut a = f.update(&first);
        a.candidate_ledger = change.candidate_ledger.clone();
        let second = apply(&f.repo, STATE_REF, &a, false, deadline()).unwrap();
        assert_eq!(first.ledger_blob_id, second.ledger_blob_id);
        let mut stale = f.update(&first);
        stale.candidate_sha256 = "c".repeat(64);
        let before = snapshot(&f.repo);
        assert!(apply(&f.repo, STATE_REF, &stale, false, deadline()).is_err());
        assert!(apply(&f.repo, STATE_REF, &change, true, deadline()).is_err());
        assert_eq!(snapshot(&f.repo), before);
        // Manually crafted commit with exactly the ledger is still foreign.
        let tree =
            String::from_utf8(f.run(&["rev-parse", &format!("{}^{{tree}}", first.commit)], b""))
                .unwrap();
        let foreign = f.seed(tree.trim(), "manual foreign state\n", 1_700_000_010);
        f.run(&["update-ref", STATE_REF, &foreign], b"");
        stale.base_commit = foreign;
        assert!(verify(&f.repo, STATE_REF, &stale, false, deadline()).is_err());
    }

    #[test]
    fn unsafe_refs_and_repository_layouts_are_refused_without_writes() {
        for poison in [
            "symbolic",
            "dangling",
            "ref-symlink",
            "refs-symlink",
            "objects-symlink",
            "alternates",
            "worktree",
            "unborn",
            "collision",
            "ordinary",
            "reftable",
            "shared",
        ] {
            let f = Fixture::new("sha1");
            let target = f.repo.join(STATE_REF);
            match poison {
                "symbolic" => {
                    f.run(&["symbolic-ref", STATE_REF, "refs/heads/unrelated"], b"");
                }
                "dangling" => {
                    fs::write(&target, "ref: refs/heads/missing\n").unwrap();
                }
                "ref-symlink" => {
                    symlink("/no/such/ref", &target).unwrap();
                }
                "refs-symlink" | "objects-symlink" => {
                    let name = if poison == "refs-symlink" {
                        "refs"
                    } else {
                        "objects"
                    };
                    fs::rename(f.repo.join(name), f.repo.join(format!("{name}-saved"))).unwrap();
                    symlink(format!("{name}-saved"), f.repo.join(name)).unwrap();
                }
                "alternates" => {
                    fs::write(f.repo.join("objects/info/alternates"), "/no/such/objects\n")
                        .unwrap();
                }
                "worktree" | "unborn" => {
                    fs::create_dir_all(f.repo.join("worktrees/registered")).unwrap();
                    fs::write(
                        f.repo.join("worktrees/registered/HEAD"),
                        if poison == "unborn" {
                            "ref: refs/heads/not-yet-born\n".into()
                        } else {
                            format!("{}\n", f.base)
                        },
                    )
                    .unwrap();
                }
                "collision" => {
                    f.run(&["update-ref", &format!("{STATE_REF}/child"), &f.base], b"");
                }
                "ordinary" => {
                    f.run(&["config", "core.bare", "false"], b"");
                }
                "reftable" => {
                    f.run(&["refs", "migrate", "--ref-format=reftable"], b"");
                }
                "shared" => {
                    fs::set_permissions(f.repo.join("objects"), fs::Permissions::from_mode(0o777))
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let before = snapshot(&f.repo);
            assert!(
                verify(&f.repo, STATE_REF, &f.change(), true, deadline()).is_err(),
                "{poison}"
            );
            assert!(
                apply(&f.repo, STATE_REF, &f.change(), true, deadline()).is_err(),
                "{poison}"
            );
            assert_eq!(snapshot(&f.repo), before, "{poison}");
        }
    }

    #[test]
    fn real_linked_worktrees_including_unborn_and_checked_out_state_are_refused() {
        for unborn in [false, true] {
            let f = Fixture::new("sha1");
            let worktree = f.repo.parent().unwrap().join("worktree");
            if unborn {
                f.run(
                    &[
                        "worktree",
                        "add",
                        "--orphan",
                        "-b",
                        "unborn",
                        worktree.to_str().unwrap(),
                    ],
                    b"",
                );
            } else {
                f.run(&["update-ref", STATE_REF, &f.base], b"");
                f.run(
                    &[
                        "worktree",
                        "add",
                        worktree.to_str().unwrap(),
                        "hive-calendar-state",
                    ],
                    b"",
                );
            }
            let before = snapshot(f.repo.parent().unwrap());
            assert!(verify(&f.repo, STATE_REF, &f.change(), true, deadline()).is_err());
            assert!(apply(&f.repo, STATE_REF, &f.change(), true, deadline()).is_err());
            assert!(apply(&worktree, STATE_REF, &f.change(), true, deadline()).is_err());
            assert_eq!(snapshot(f.repo.parent().unwrap()), before);
        }
    }

    #[test]
    fn existing_nested_parent_preserves_siblings_and_malformed_local_objects_fail_closed() {
        let f = Fixture::new("sha1");
        let mut c = f.change();
        c.path = "dir/new/ledger.json".into();
        let base_sibling = f.run(&["ls-tree", &f.base, "--", "dir/empty.json"], b"");
        let applied = apply(&f.repo, STATE_REF, &c, true, deadline()).unwrap();
        assert_eq!(
            f.run(&["ls-tree", &applied.commit, "--", "dir/empty.json"], b""),
            base_sibling
        );
        f.run(&["update-ref", "-d", STATE_REF], b"");
        for kind in ["missing-blob", "wrong-type", "truncated-tree", "bad-commit"] {
            let tree = if kind == "truncated-tree" {
                String::from_utf8(f.run(
                    &["hash-object", "--literally", "-w", "-t", "tree", "--stdin"],
                    b"100644 bad\0abc",
                ))
                .unwrap()
                .trim()
                .to_owned()
            } else {
                let entry = Entry {
                    mode: b"100644".to_vec(),
                    name: b"bad".to_vec(),
                    oid: if kind == "missing-blob" {
                        "f".repeat(40)
                    } else {
                        f.base.clone()
                    },
                };
                f.write("tree", &encode_tree(&[entry]).unwrap())
            };
            c.base_commit = if kind == "bad-commit" {
                String::from_utf8(f.run(
                    &[
                        "hash-object",
                        "--literally",
                        "-w",
                        "-t",
                        "commit",
                        "--stdin",
                    ],
                    b"not a commit\n",
                ))
                .unwrap()
                .trim()
                .to_owned()
            } else {
                f.seed(&tree, "malformed fixture\n", 1_700_000_000)
            };
            let before = snapshot(&f.repo);
            assert!(
                apply(&f.repo, STATE_REF, &c, true, deadline()).is_err(),
                "{kind}"
            );
            assert_eq!(snapshot(&f.repo), before);
        }
    }

    #[test]
    fn packed_refs_work_and_hooks_never_run() {
        let f = Fixture::new("sha1");
        let sentinel = f.repo.join("hook-ran");
        for name in [
            "reference-transaction",
            "pre-commit",
            "post-commit",
            "post-update",
        ] {
            let path = f.repo.join("hooks").join(name);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\n/usr/bin/touch '{}'\nexit 1\n",
                    sentinel.display()
                ),
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        f.run(&["pack-refs", "--all"], b"");
        assert!(!f.repo.join(STATE_REF).exists());
        apply(&f.repo, STATE_REF, &f.update(&first), false, deadline()).unwrap();
        assert!(!sentinel.exists());
    }

    #[test]
    fn competing_writers_use_whole_head_cas_and_lost_response_retries_are_exact() {
        let f = Fixture::new("sha1");
        let first = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|i| {
                    let mut change = f.update(&first);
                    change.candidate_sha256 = if i == 0 { "c" } else { "d" }.repeat(64);
                    let f = &f;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        apply(&f.repo, STATE_REF, &change, false, deadline())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
        let winner = outcomes.into_iter().find_map(Result::ok).unwrap();
        let mut exact = f.update(&first);
        exact.candidate_sha256 = winner.candidate_sha256.clone();
        let before = snapshot(&f.repo);
        // Ignore the original receipt, as if the caller lost the CAS response.
        let retry = apply(&f.repo, STATE_REF, &exact, false, deadline()).unwrap();
        assert_eq!(retry.status, "already_applied");
        assert!(!retry.local_ref_updated);
        assert_eq!(snapshot(&f.repo), before);
    }

    #[test]
    fn invalid_parents_blobs_paths_receipts_and_timestamps_fail_before_writes() {
        let f = Fixture::new("sha1");
        for path in [
            "empty.json/ledger.json",
            "link/ledger.json",
            "executable/ledger.json",
            "dir",
            "../ledger.json",
            ".git/ledger.json",
            "a//ledger.json",
            "empty.json",
        ] {
            let mut c = f.change();
            c.path = path.into();
            let before = snapshot(&f.repo);
            assert!(
                apply(&f.repo, STATE_REF, &c, true, deadline()).is_err(),
                "{path}"
            );
            assert_eq!(snapshot(&f.repo), before);
        }
        for (create, expected) in [(false, None), (true, Some("a".repeat(40)))] {
            let mut c = f.change();
            c.expected_blob_id = expected;
            assert!(verify(&f.repo, STATE_REF, &c, create, deadline()).is_err());
        }
        assert!(verify(&f.repo, "refs/heads/other", &f.change(), true, deadline()).is_err());
        assert!(verify(&f.repo, STATE_REF, &f.change(), true, Instant::now()).is_err());
        let applied = apply(&f.repo, STATE_REF, &f.change(), true, deadline()).unwrap();
        let original = f.run(&["cat-file", "commit", &applied.commit], b"");
        for corruption in ["marker", "receipt", "identity", "unrelated"] {
            let mut bytes = original.clone();
            match corruption {
                "marker" => {
                    let p = bytes
                        .windows(MARKER.len())
                        .position(|w| w == MARKER.as_bytes())
                        .unwrap();
                    bytes[p] = b'H';
                }
                "receipt" => {
                    let p = bytes
                        .windows(64)
                        .position(|w| w == "a".repeat(64).as_bytes())
                        .unwrap();
                    bytes[p] = b'z';
                }
                "identity" => {
                    let p = bytes
                        .windows(IDENTITY.len())
                        .position(|w| w == IDENTITY.as_bytes())
                        .unwrap();
                    bytes[p] = b'h';
                }
                "unrelated" => {
                    let tree = String::from_utf8(
                        f.run(&["rev-parse", &format!("{}^{{tree}}", applied.commit)], b""),
                    )
                    .unwrap();
                    let raw = f.run(&["cat-file", "tree", tree.trim()], b"");
                    let mut entries = parse_tree(&raw, 40).unwrap();
                    entries.retain(|e| e.name != b"executable");
                    let replacement = f.write("tree", &encode_tree(&entries).unwrap());
                    bytes.splice(5..45, replacement.bytes());
                }
                _ => unreachable!(),
            }
            let foreign = f.write("commit", &bytes);
            f.run(&["update-ref", STATE_REF, &foreign], b"");
            let mut c = f.update(&applied);
            c.base_commit = foreign;
            let before = snapshot(&f.repo);
            assert!(
                apply(&f.repo, STATE_REF, &c, false, deadline()).is_err(),
                "{corruption}"
            );
            assert_eq!(snapshot(&f.repo), before);
        }
        assert!(next_time(253_402_300_799).is_err());
        assert!(parse_time("committer X <x@y> -1 +0000").is_err());
        assert!(parse_time("committer X <x@y> 1 +2460").is_err());
    }

    #[test]
    fn raw_tree_encoding_rejects_duplicates_bad_order_modes_and_truncation() {
        let id = "a".repeat(40);
        let e = |name: &[u8], mode: &[u8]| Entry {
            name: name.to_vec(),
            mode: mode.to_vec(),
            oid: id.clone(),
        };
        for entries in [
            vec![e(b"a", b"100644"), e(b"a", b"40000")],
            vec![e(b"z", b"100644"), e(b"a", b"100644")],
            vec![e(b"a/b", b"100644")],
            vec![e(b"..", b"100644")],
            vec![e(b"a", b"040000")],
        ] {
            assert!(parse_tree(&encode_tree(&entries).unwrap(), 40).is_err());
        }
        assert!(parse_tree(b"100644 a\0abc", 40).is_err());
        let entries = vec![
            e(b"foo.bar", b"100644"),
            e(b"foo", b"40000"),
            e(b"foo0", b"100644"),
        ];
        let raw = encode_tree(&entries).unwrap();
        assert_eq!(encode_tree(&parse_tree(&raw, 40).unwrap()).unwrap(), raw);
    }

    #[test]
    fn receipt_wire_encoding_is_frozen() {
        let record = Record {
            schema_version: "1".into(),
            state_ref: STATE_REF.into(),
            path: "ledger.json".into(),
            binding: Binding {
                provider: "google-calendar".into(),
                account_id: "a".into(),
                calendar_id: "c".into(),
                source_repo_id: "s".into(),
            },
            ledger_blob_id: "1".repeat(40),
            candidate_sha256: "2".repeat(64),
            source_commit: "3".repeat(40),
            operation: "enroll".into(),
            logical_id: "id".into(),
            parser_change_accepted: false,
            base_commit: "4".repeat(40),
            expected_blob_id: None,
        };
        let expected = concat!(
            "hive-calendar-state v1\n\n",
            "{\"schemaVersion\":\"1\",\"stateRef\":\"refs/heads/hive-calendar-state\",\"path\":\"ledger.json\",",
            "\"binding\":{\"provider\":\"google-calendar\",\"accountId\":\"a\",\"calendarId\":\"c\",\"sourceRepoId\":\"s\"},",
            "\"ledgerBlobId\":\"1111111111111111111111111111111111111111\",",
            "\"candidateSha256\":\"2222222222222222222222222222222222222222222222222222222222222222\",",
            "\"sourceCommit\":\"3333333333333333333333333333333333333333\",\"operation\":\"enroll\",",
            "\"logicalId\":\"id\",\"parserChangeAccepted\":false,\"baseCommit\":\"4444444444444444444444444444444444444444\",\"expectedBlobId\":null}\n"
        );
        assert_eq!(record.message().unwrap(), expected.as_bytes());
        let bytes = commit_bytes(
            &"5".repeat(40),
            &record.base_commit,
            1_700_000_001,
            expected.as_bytes(),
        );
        let header = concat!(
            "tree 5555555555555555555555555555555555555555\nparent 4444444444444444444444444444444444444444\n",
            "author Hive Calendar <hive-calendar@localhost> 1700000001 +0000\n",
            "committer Hive Calendar <hive-calendar@localhost> 1700000001 +0000\n\n"
        );
        assert_eq!(bytes, [header.as_bytes(), expected.as_bytes()].concat());

        let mut common = record.clone();
        common.operation = "prepare".into();
        common.expected_blob_id = Some("6".repeat(40));
        let v2 = OwnedRecordV2::new(
            common,
            "22222222-2222-4222-8222-222222222222",
            &format!("sha256:{}", "7".repeat(64)),
        );
        let expected_v2 = concat!(
            "hive-calendar-state v2\n\n",
            "{\"schemaVersion\":\"2\",\"stateRef\":\"refs/heads/hive-calendar-state\",\"path\":\"ledger.json\",",
            "\"binding\":{\"provider\":\"google-calendar\",\"accountId\":\"a\",\"calendarId\":\"c\",\"sourceRepoId\":\"s\"},",
            "\"ledgerBlobId\":\"1111111111111111111111111111111111111111\",",
            "\"candidateSha256\":\"2222222222222222222222222222222222222222222222222222222222222222\",",
            "\"sourceCommit\":\"3333333333333333333333333333333333333333\",\"operation\":\"prepare\",",
            "\"logicalId\":\"id\",\"parserChangeAccepted\":false,\"baseCommit\":\"4444444444444444444444444444444444444444\",",
            "\"expectedBlobId\":\"6666666666666666666666666666666666666666\",",
            "\"operationId\":\"22222222-2222-4222-8222-222222222222\",",
            "\"intentDigest\":\"sha256:7777777777777777777777777777777777777777777777777777777777777777\"}\n"
        );
        assert_eq!(v2.message().unwrap(), expected_v2.as_bytes());

        let mut common = record;
        common.expected_blob_id = Some("6".repeat(40));
        let v3 = OwnedRecordV3::new(common, RecordProof::Source, AckOrigin::SyntheticFixture);
        let expected_v3 = concat!(
            "hive-calendar-state v3\n\n",
            "{\"schemaVersion\":\"3\",\"stateRef\":\"refs/heads/hive-calendar-state\",\"path\":\"ledger.json\",",
            "\"binding\":{\"provider\":\"google-calendar\",\"accountId\":\"a\",\"calendarId\":\"c\",\"sourceRepoId\":\"s\"},",
            "\"ledgerBlobId\":\"1111111111111111111111111111111111111111\",",
            "\"candidateSha256\":\"2222222222222222222222222222222222222222222222222222222222222222\",",
            "\"sourceCommit\":\"3333333333333333333333333333333333333333\",\"operation\":\"enroll\",",
            "\"logicalId\":\"id\",\"parserChangeAccepted\":false,\"baseCommit\":\"4444444444444444444444444444444444444444\",",
            "\"expectedBlobId\":\"6666666666666666666666666666666666666666\",",
            "\"proof\":{\"kind\":\"Source\"},\"provenanceFloor\":\"synthetic_fixture\"}\n"
        );
        assert_eq!(v3.message().unwrap(), expected_v3.as_bytes());
        let parsed: OwnedRecordV3 =
            serde_json::from_slice(&v3.message().unwrap()[MARKER_V3.len()..]).unwrap();
        assert_eq!(parsed.message().unwrap(), expected_v3.as_bytes());
    }
}
