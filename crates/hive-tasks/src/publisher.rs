//! Local-admin, single-target publisher. The lock is not a distributed fence.
use crate::{
    policy::{self, FirstDue, TaskObservation},
    publisher_http::{Http, Resource, ResponseEvidence},
    storage::{self, Lock},
};
use anyhow::{Context, Result, ensure};
use hive_calendar::task_snapshot::read_committed_tasks;
use hive_capture::handoff::TaskHandoff;
use hive_google_auth::{
    AuthProfile, CredentialStore, OAuthTransport, google::GoogleOAuth, provider::Authenticator,
    storage::MacKeychainStore, validate_connection,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const STATE_REF: &str = "refs/heads/hive-tasks-publisher-state";
const BINDING: &str = "hive-publisher-binding.json";
const LOCK: &str = "hive-publisher-lock";
const TEST_FLOOR: &str = "hive-publisher-test-origin";
// Three Resource slots can coexist (PATCH, confirmation, recovery). Each contains
// <=64 KiB of compact raw JSON plus a separately serialized <=1024-byte ASCII
// ETag (<=2048 escaped bytes) and <32 wrapper bytes. Their total plus 4 KiB for
// attempt digests/wrappers and <=8 phases is less than this 256 KiB reserve.
const EVIDENCE_RESERVE: usize = 4 * crate::publisher_http::RESOURCE_LIMIT;
const _: () =
    assert!(3 * (crate::publisher_http::RESOURCE_LIMIT + 2 * 1024 + 32) + 4096 < EVIDENCE_RESERVE);

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn digest(value: &impl Serialize) -> Result<String> {
    Ok(hash(&serde_json::to_vec(value)?))
}
fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn random() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).context("publisher randomness unavailable")?;
    Ok(hash(&bytes))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    schema: u32,
    installation_id: String,
    state_repo: PathBuf,
    source_repo: PathBuf,
    mw_bin: PathBuf,
    parser_sha256: String,
    connection: String,
    origin: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema: u32,
    kind: String,
    installation_id: String,
    origin: String,
    source_ref: String,
    handoff: TaskHandoff,
    handoff_sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Source {
    path: String,
    line: usize,
    source_id: String,
    source_revision: String,
    #[serde(rename = "gitBlobID")]
    git_blob_id: String,
    literal_text: String,
    literal_metadata: String,
    metadata: Metadata,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    due: String,
    raw: String,
    start: String,
    estimate: String,
}
impl Source {
    fn validate(&self, commit: &str, due: &str) -> Result<()> {
        ensure!(
            hex(&self.git_blob_id, commit.len())
                && self.line > 0
                && self
                    .source_revision
                    .strip_prefix("sha256:")
                    .is_some_and(|s| hex(s, 64)),
            "invalid frozen source evidence"
        );
        ensure!(
            !Path::new(&self.path).is_absolute()
                && !Path::new(&self.path)
                    .components()
                    .any(|p| !matches!(p, std::path::Component::Normal(_))),
            "invalid semantic source path"
        );
        let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")?;
        let day = time::Date::parse(&self.metadata.due, &format)?;
        ensure!(
            day.year() >= 1
                && day.to_string() == self.metadata.due
                && due == format!("{}T00:00:00.000Z", day),
            "invalid frozen due"
        );
        Ok(())
    }
}
#[cfg(test)]
pub(crate) fn validate_hosted_source(source: &Value, commit: &str, due: &str) -> Result<()> {
    serde_json::from_value::<Source>(source.clone())?.validate(commit, due)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Intent {
    nonce: String,
    source_repo_id: String,
    commit: String,
    parser_sha256: String,
    source: Source,
    handoff: TaskHandoff,
    handoff_sha256: String,
    baseline: Resource,
    baseline_sha256: String,
    patch: DuePatch,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DuePatch {
    due: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
enum Phase {
    Prepared,
    Attempting,
    Applied,
    RejectedConflict,
    Uncertain,
    NotDispatched,
    RecoveredSatisfied,
    ConflictNeedsReview,
    AlreadySatisfied,
}
impl Phase {
    fn permits_next(self) -> bool {
        matches!(self, Self::NotDispatched | Self::RejectedConflict)
    }
    fn follows(self, old: Self) -> bool {
        matches!(
            (old, self),
            (Self::Prepared, Self::Attempting | Self::NotDispatched)
                | (
                    Self::Attempting,
                    Self::Applied
                        | Self::RejectedConflict
                        | Self::Uncertain
                        | Self::RecoveredSatisfied
                        | Self::ConflictNeedsReview
                )
                | (
                    Self::Uncertain,
                    Self::RecoveredSatisfied | Self::ConflictNeedsReview
                )
        )
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Operation {
    operation_id: String,
    intent_sha256: String,
    intent: Intent,
    phases: Vec<Phase>,
    patch_evidence: Option<Resource>,
    confirmation: Option<Resource>,
    recovery_evidence: Option<Resource>,
    attempt_evidence: Option<AttemptEvidence>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
enum AttemptEvidence {
    Response(ResponseEvidence),
    TransportOrEnvelopeUnknown,
}
impl Operation {
    fn phase(&self) -> Phase {
        *self.phases.last().expect("validated phases")
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct State {
    manifest: Manifest,
    operations: Vec<Operation>,
}

impl State {
    fn validate(&self, binding: &Binding) -> Result<()> {
        let m = &self.manifest;
        m.handoff.validate()?;
        for id in [&m.handoff.list_id, &m.handoff.provider_id] {
            ensure!(
                !id.is_empty()
                    && id.len() <= 256
                    && id.trim() == id
                    && id != "@default"
                    && !id.contains('.')
                    && !id.chars().any(char::is_control),
                "exact task/list ID required; no default list or aliases"
            );
        }
        ensure!(
            m.schema == 1
                && m.kind == "google-tasks-first-due-publisher"
                && m.origin == binding.origin
                && m.installation_id == binding.installation_id
                && hex(&m.installation_id, 64)
                && m.source_ref == "refs/heads/main"
                && m.handoff_sha256 == digest(&m.handoff)?,
            "invalid publisher manifest/binding"
        );
        ensure!(
            m.handoff.baseline.due.is_none(),
            "undated reviewed handoff required"
        );
        ensure!(self.operations.len() <= 128, "operation retention limit");
        let mut ids = std::collections::BTreeSet::new();
        for (index, op) in self.operations.iter().enumerate() {
            let i = &op.intent;
            ensure!(
                op.intent_sha256 == digest(i)?
                    && op.operation_id == op.intent_sha256
                    && ids.insert(&op.operation_id),
                "invalid intent digest/operation ID"
            );
            ensure!(
                digest(&i.handoff)? == m.handoff_sha256
                    && i.handoff_sha256 == m.handoff_sha256
                    && i.source_repo_id == m.handoff.source_repo_id
                    && hex(&i.nonce, 64),
                "intent namespace mismatch"
            );
            ensure!(
                matches!(i.commit.len(), 40 | 64)
                    && hex(&i.commit, i.commit.len())
                    && hex(&i.parser_sha256, 64),
                "invalid frozen source evidence"
            );
            i.source.validate(&i.commit, &i.patch.due)?;
            let task = i.baseline.validate(&m.handoff.provider_id)?;
            policy::accepted_fields(&task, &m.handoff.baseline)?;
            ensure!(
                i.baseline_sha256 == digest(&i.baseline.raw)?,
                "invalid resource digest"
            );
            ensure!(
                matches!(
                    op.phases.first(),
                    Some(Phase::Prepared | Phase::AlreadySatisfied)
                ) && op.phases.len() <= 8
                    && op.phases.windows(2).all(|p| p[1].follows(p[0])),
                "invalid publisher phase history"
            );
            if op.phases[0] == Phase::AlreadySatisfied {
                ensure!(
                    task.due
                        .as_deref()
                        .is_some_and(|d| policy::due_matches(d, &i.patch.due)),
                    "invalid satisfied baseline"
                );
            } else {
                ensure!(task.due.is_none(), "prepared baseline must be undated");
            }
            if let Some(response) = &op.patch_evidence {
                ensure!(
                    op.phases.contains(&Phase::Attempting),
                    "response without attempt"
                );
                response.satisfied(&i.baseline, &m.handoff.provider_id, &i.patch.due)?;
                ensure!(response.etag != i.baseline.etag, "unchanged mutation ETag");
                ensure!(
                    matches!(&op.attempt_evidence, Some(AttemptEvidence::Response(e)) if e.status == 200),
                    "validated mutation lacks response evidence"
                );
            }
            if let Some(attempt) = &op.attempt_evidence {
                ensure!(
                    op.phases.contains(&Phase::Attempting),
                    "attempt evidence without Attempting"
                );
                if let AttemptEvidence::Response(e) = attempt {
                    ensure!(
                        (100..=599).contains(&e.status)
                            && hex(&e.body_sha256, 64)
                            && e.body_bytes <= crate::publisher_http::RESOURCE_LIMIT,
                        "invalid bounded response evidence"
                    );
                }
            }
            if op.phase() == Phase::RejectedConflict {
                ensure!(
                    matches!(&op.attempt_evidence, Some(AttemptEvidence::Response(e)) if e.status == 412),
                    "rejected conflict lacks 412 evidence"
                );
            }
            if let Some(confirmed) = &op.confirmation {
                confirmed.satisfied(&i.baseline, &m.handoff.provider_id, &i.patch.due)?;
                ensure!(
                    op.patch_evidence
                        .as_ref()
                        .is_some_and(|p| p.etag == confirmed.etag),
                    "confirmation ETag mismatch"
                );
            }
            if op.phase() == Phase::Applied {
                ensure!(
                    op.patch_evidence.is_some() && op.confirmation.is_some(),
                    "Applied lacks durable evidence"
                );
            }
            if let Some(observed) = &op.recovery_evidence {
                observed.validate(&m.handoff.provider_id)?;
                ensure!(
                    op.phases.contains(&Phase::Attempting),
                    "recovery evidence without attempt"
                );
            }
            if op.phase() == Phase::RecoveredSatisfied {
                op.recovery_evidence
                    .as_ref()
                    .context("recovered satisfaction lacks durable GET evidence")?
                    .satisfied(&i.baseline, &m.handoff.provider_id, &i.patch.due)?;
            }
            ensure!(
                index + 1 == self.operations.len() || op.phase().permits_next(),
                "operation follows blocked/closed target"
            );
        }
        let bytes = serde_json::to_vec(self)?.len();
        ensure!(bytes <= storage::LIMIT, "state size limit");
        // Enforce admission at prepare persistence and every Prepared apply load.
        // Never retroactively impose headroom on Attempting/Uncertain or terminal
        // states: these must remain readable and eligible for GET-only recovery.
        if self
            .operations
            .last()
            .is_some_and(|op| op.phase() == Phase::Prepared)
        {
            ensure!(
                bytes <= storage::LIMIT - EVIDENCE_RESERVE,
                "insufficient state headroom for dispatch and recovery evidence"
            );
        }
        Ok(())
    }
}

fn parser_hash(path: &Path) -> Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    storage::path(path, true)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(file.metadata()?.is_file(), "regular parser binary required");
    let mut bytes = Vec::new();
    file.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 64 * 1024 * 1024, "parser binary limit");
    Ok(hash(&bytes))
}
fn main_tip(repo: &Path) -> Result<String> {
    storage::path(repo, true)?;
    storage::path(&repo.join(".git"), true)?;
    ensure!(
        repo.join(".git").is_dir()
            && storage::git(repo, &["rev-parse", "--absolute-git-dir"], b"")?
                == repo
                    .join(".git")
                    .to_str()
                    .context("UTF-8 source path required")?,
        "dedicated canonical source Git directory required"
    );
    ensure!(
        storage::git(repo, &["symbolic-ref", "HEAD"], b"")? == "refs/heads/main",
        "source HEAD must be main"
    );
    ensure!(
        !repo.join(".git/objects/info/alternates").exists() && !repo.join(".git/shallow").exists(),
        "isolated complete source required"
    );
    storage::git(
        repo,
        &["rev-parse", "--verify", "refs/heads/main^{commit}"],
        b"",
    )
}

/// Purely local setup. No auth, notes writes, queue operations, or provider calls.
pub fn initialize(
    state_repo: &Path,
    source_repo: &Path,
    mw_bin: &Path,
    connection: &str,
    handoff: &TaskHandoff,
) -> Result<Value> {
    initialize_origin(
        state_repo,
        source_repo,
        mw_bin,
        connection,
        handoff,
        "Google",
    )
}
fn initialize_origin(
    state_repo: &Path,
    source_repo: &Path,
    mw_bin: &Path,
    connection: &str,
    handoff: &TaskHandoff,
    origin: &str,
) -> Result<Value> {
    storage::path(state_repo, false)?;
    ensure!(
        !state_repo.exists(),
        "publisher init refuses existing paths"
    );
    validate_connection(connection)?;
    handoff.validate()?;
    ensure!(
        handoff.baseline.due.is_none(),
        "undated handoff baseline required"
    );
    main_tip(source_repo)?;
    let binding = Binding {
        schema: 1,
        installation_id: random()?,
        state_repo: state_repo.into(),
        source_repo: source_repo.into(),
        mw_bin: mw_bin.into(),
        parser_sha256: parser_hash(mw_bin)?,
        connection: connection.into(),
        origin: origin.into(),
    };
    let state = State {
        manifest: Manifest {
            schema: 1,
            kind: "google-tasks-first-due-publisher".into(),
            installation_id: binding.installation_id.clone(),
            origin: origin.into(),
            source_ref: "refs/heads/main".into(),
            handoff: handoff.clone(),
            handoff_sha256: digest(handoff)?,
        },
        operations: Vec::new(),
    };
    state.validate(&binding)?;
    fs::create_dir(state_repo)?;
    storage::git(
        state_repo,
        &["init", "--bare", "--template=", "--object-format=sha1", "."],
        b"",
    )?;
    storage::create(&state_repo.join(LOCK), b"")?;
    let _lock = Lock::acquire(&state_repo.join(LOCK))?;
    storage::create(&state_repo.join(BINDING), &serde_json::to_vec(&binding)?)?;
    if origin != "Google" {
        storage::create(
            &state_repo.join(TEST_FLOOR),
            b"test-only; never Google authority\n",
        )?;
    }
    let oid = storage::save_ref(state_repo, STATE_REF, None, &state)?;
    fs::File::open(state_repo)?.sync_all()?;
    Ok(report(&state, &oid, false))
}

/// Exclusive local-admin session. Native credentials are not constructed by open/inspect.
pub struct Publisher {
    repo: PathBuf,
    binding: Binding,
    _lock: Lock,
}
impl Publisher {
    pub fn open(repo: &Path) -> Result<Self> {
        Self::open_origin(repo, "Google")
    }
    fn open_origin(repo: &Path, origin: &str) -> Result<Self> {
        storage::path(repo, true)?;
        let lock = Lock::acquire(&repo.join(LOCK))?;
        let binding: Binding = storage::read(&repo.join(BINDING))?;
        ensure!(
            origin != "Google" || !repo.join(TEST_FLOOR).exists(),
            "test-origin installation cannot be promoted"
        );
        ensure!(
            binding.schema == 1 && binding.state_repo == repo && binding.origin == origin,
            "publisher local installation/origin mismatch; no adopt/rebind"
        );
        validate_connection(&binding.connection)?;
        ensure!(
            binding.source_repo.is_absolute()
                && binding.mw_bin.is_absolute()
                && hex(&binding.parser_sha256, 64),
            "invalid local source/parser binding"
        );
        let session = Self {
            repo: repo.into(),
            binding,
            _lock: lock,
        };
        let (_, state) = session.load()?;
        // Preserve the installation's inception origin, even if a later tip and
        // local binding are edited together. This is not protection against a
        // malicious administrator rewriting the entire installation/history.
        let root = storage::git(repo, &["rev-list", "--max-parents=0", STATE_REF], b"")?;
        ensure!(
            hex(&root, 40),
            "single complete publisher inception required"
        );
        let genesis: State = serde_json::from_str(&storage::git(
            repo,
            &["cat-file", "blob", &format!("{root}:state.json")],
            b"",
        )?)?;
        genesis.validate(&session.binding)?;
        ensure!(
            genesis.operations.is_empty()
                && genesis.manifest.handoff_sha256 == state.manifest.handoff_sha256,
            "publisher inception/target mismatch; no rebind"
        );
        Ok(session)
    }
    fn load(&self) -> Result<(String, State)> {
        let (oid, state): (_, State) = storage::load_ref(&self.repo, STATE_REF)?;
        state.validate(&self.binding)?;
        Ok((oid, state))
    }
    fn persist(&self, oid: &mut String, state: &State) -> Result<()> {
        state.validate(&self.binding)?;
        *oid = storage::save_ref(&self.repo, STATE_REF, Some(oid), state)?;
        Ok(())
    }
    pub fn inspect(&self) -> Result<Value> {
        let (oid, state) = self.load()?;
        Ok(report(&state, &oid, false))
    }
    fn source(
        &self,
        commit: &str,
        handoff: &TaskHandoff,
        baseline: &Resource,
    ) -> Result<(Source, String, FirstDue)> {
        ensure!(
            matches!(commit.len(), 40 | 64)
                && hex(commit, commit.len())
                && main_tip(&self.binding.source_repo)? == commit,
            "selected full commit must equal local main tip"
        );
        ensure!(
            parser_hash(&self.binding.mw_bin)? == self.binding.parser_sha256,
            "bound parser changed"
        );
        let snapshot =
            read_committed_tasks(&self.binding.source_repo, commit, &self.binding.mw_bin)?;
        let candidate = policy::evaluate(
            &snapshot,
            &handoff.source_repo_id,
            handoff,
            &TaskObservation {
                schema_version: "1".into(),
                kind: "google-task-observation".into(),
                source_repo_id: handoff.source_repo_id.clone(),
                account_key: handoff.account_key.clone(),
                list_id: handoff.list_id.clone(),
                task: baseline.validate(&handoff.provider_id)?,
            },
        )?;
        ensure!(
            snapshot.parser_sha256 == self.binding.parser_sha256
                && parser_hash(&self.binding.mw_bin)? == self.binding.parser_sha256
                && main_tip(&self.binding.source_repo)? == commit,
            "source/parser changed during read"
        );
        Ok((
            serde_json::from_value(candidate.source().clone())?,
            candidate.desired_due().into(),
            candidate.decision(),
        ))
    }
    fn preflight_prepare(&self, commit: &str) -> Result<(String, State)> {
        let (oid, state) = self.load()?;
        ensure!(
            state.operations.len() < 128
                && state
                    .operations
                    .last()
                    .is_none_or(|o| o.phase().permits_next()),
            "target blocked or permanently closed"
        );
        // Run all local source policy before constructing any native dependency.
        let baseline = Resource {
            raw: serde_json::to_value(&state.manifest.handoff.baseline)?,
            etag: "\"local-preflight\"".into(),
        };
        let mut baseline = baseline;
        baseline.raw["etag"] = json!(baseline.etag);
        self.source(commit, &state.manifest.handoff, &baseline)?;
        Ok((oid, state))
    }
    pub fn prepare(&mut self, commit: &str) -> Result<Value> {
        let budget = Budget::new();
        self.preflight_prepare(commit)?;
        let store = MacKeychainStore::for_profile(AuthProfile::TasksWrite)?;
        let oauth = GoogleOAuth::new()?;
        self.prepare_with(
            commit,
            &store,
            &|remaining| oauth.with_request_timeout(remaining),
            &Http::google()?,
            &budget,
            Stop::None,
        )
    }
    fn prepare_with<F, T>(
        &self,
        commit: &str,
        store: &dyn CredentialStore,
        oauth: &F,
        http: &Http,
        budget: &Budget,
        stop: Stop,
    ) -> Result<Value>
    where
        F: Fn(Duration) -> hive_google_auth::Result<T> + Send + Sync,
        T: OAuthTransport,
    {
        let (mut oid, mut state) = self.preflight_prepare(commit)?;
        self.check_origin(http)?;
        let h = &state.manifest.handoff;
        let bound = authenticate(&self.binding, h, store, oauth, budget)?;
        let baseline = http
            .request(
                bound.token.expose(),
                &h.list_id,
                &h.provider_id,
                None,
                budget.remaining()?,
            )?
            .resource(&h.provider_id)?;
        let (source, due, decision) = self.source(commit, h, &baseline)?;
        ensure!(
            decision != FirstDue::Conflict,
            "remote due conflicts; no rescheduling"
        );
        let intent = Intent {
            nonce: random()?,
            source_repo_id: h.source_repo_id.clone(),
            commit: commit.into(),
            parser_sha256: self.binding.parser_sha256.clone(),
            source,
            handoff: h.clone(),
            handoff_sha256: state.manifest.handoff_sha256.clone(),
            baseline_sha256: digest(&baseline.raw)?,
            baseline,
            patch: DuePatch { due },
        };
        let id = digest(&intent)?;
        state.operations.push(Operation {
            operation_id: id.clone(),
            intent_sha256: id,
            intent,
            phases: vec![if decision == FirstDue::AlreadySatisfied {
                Phase::AlreadySatisfied
            } else {
                Phase::Prepared
            }],
            patch_evidence: None,
            confirmation: None,
            recovery_evidence: None,
            attempt_evidence: None,
        });
        stop.at(Stop::BeforePrepared)?;
        ensure!(
            main_tip(&self.binding.source_repo)? == commit,
            "main advanced before prepare CAS"
        );
        budget.remaining()?;
        self.persist(&mut oid, &state)?;
        stop.at(Stop::AfterPrepared)?;
        Ok(report(&state, &oid, http.google))
    }
    fn check_origin(&self, http: &Http) -> Result<()> {
        ensure!(
            http.google == (self.binding.origin == "Google"),
            "HTTP/state origin mismatch"
        );
        Ok(())
    }
    fn preflight_apply(&self, approval: &Approval<'_>) -> Result<(String, State)> {
        ensure!(
            approval.acknowledge_live_write,
            "--acknowledge-live-write is required"
        );
        ensure!(
            hex(approval.operation_id, 64)
                && hex(approval.expected_state_oid, 40)
                && hex(approval.expected_intent_sha256, 64),
            "exact operation, state OID and intent SHA256 required"
        );
        let (oid, state) = self.load()?;
        let op = state.operations.last().context("no prepared operation")?;
        ensure!(
            oid == approval.expected_state_oid
                && op.operation_id == approval.operation_id
                && op.intent_sha256 == approval.expected_intent_sha256
                && op.phase() == Phase::Prepared,
            "stale/unapproved/non-Prepared operation; no replay"
        );
        let (source, due, decision) =
            self.source(&op.intent.commit, &op.intent.handoff, &op.intent.baseline)?;
        ensure!(
            digest(&source)? == digest(&op.intent.source)?
                && due == op.intent.patch.due
                && decision == FirstDue::SetFirstDue
                && op.intent.parser_sha256 == self.binding.parser_sha256,
            "frozen source evidence changed"
        );
        Ok((oid, state))
    }
    pub fn apply(&mut self, approval: Approval<'_>) -> Result<Value> {
        let budget = Budget::new();
        self.preflight_apply(&approval)?;
        let store = MacKeychainStore::for_profile(AuthProfile::TasksWrite)?;
        let oauth = GoogleOAuth::new()?;
        self.apply_with(
            &approval,
            &store,
            &|remaining| oauth.with_request_timeout(remaining),
            &Http::google()?,
            &budget,
            Stop::None,
        )
    }
    fn apply_with<F, T>(
        &self,
        approval: &Approval<'_>,
        store: &dyn CredentialStore,
        oauth: &F,
        http: &Http,
        budget: &Budget,
        stop: Stop,
    ) -> Result<Value>
    where
        F: Fn(Duration) -> hive_google_auth::Result<T> + Send + Sync,
        T: OAuthTransport,
    {
        let (mut oid, mut state) = self.preflight_apply(approval)?;
        self.check_origin(http)?;
        let i = state.operations.last().unwrap().intent.clone();
        let h = &i.handoff;
        let bound = authenticate(&self.binding, h, store, oauth, budget)?;
        let current = http
            .request(
                bound.token.expose(),
                &h.list_id,
                &h.provider_id,
                None,
                budget.remaining()?,
            )?
            .resource(&h.provider_id)?;
        ensure!(
            digest(&current.raw)? == i.baseline_sha256 && current.etag == i.baseline.etag,
            "reviewed full resource/ETag changed; prepare again only after recovery"
        );
        self.preflight_apply(approval)?;
        #[cfg(test)]
        if stop == Stop::AttemptingCas {
            storage::save_ref(&self.repo, STATE_REF, Some(&oid), &state)?;
        }
        state
            .operations
            .last_mut()
            .unwrap()
            .phases
            .push(Phase::Attempting);
        self.persist(&mut oid, &state)?;
        let permit = DispatchPermit {
            origin: self.binding.origin.clone(),
            state_repo: self.repo.clone(),
            state_oid: oid.clone(),
            operation_id: approval.operation_id.into(),
            intent: i,
        };
        stop.at(Stop::AfterAttempting)?;
        stop.at(Stop::BeforePatch)?;
        #[cfg(test)]
        if stop == Stop::ExpiredAfterAttempting {
            return self
                .dispatch(
                    permit,
                    bound.token.expose(),
                    http,
                    &Budget {
                        deadline: Instant::now(),
                    },
                )
                .map(|_| Value::Null);
        }
        // Budget admission is rechecked AFTER the Attempting CAS. No retry path exists.
        let response = self.dispatch(permit, bound.token.expose(), http, budget)?;
        stop.at(Stop::AfterRemoteMutation)?;
        state.operations.last_mut().unwrap().attempt_evidence = Some(match &response {
            Ok(response) => AttemptEvidence::Response(response.evidence()),
            Err(_) => AttemptEvidence::TransportOrEnvelopeUnknown,
        });
        let outcome = match response {
            Ok(response) if response.conflict() => Phase::RejectedConflict,
            Ok(response) => {
                let op = state.operations.last_mut().unwrap();
                let i = &op.intent;
                match response
                    .resource(&i.handoff.provider_id)
                    .and_then(|r| r.mutation(&i.baseline, &i.handoff.provider_id, &i.patch.due))
                {
                    Err(_) => Phase::Uncertain,
                    Ok(resource) => {
                        op.patch_evidence = Some(resource);
                        self.persist(&mut oid, &state)?;
                        stop.at(Stop::AfterPatchEvidence)?;
                        stop.at(Stop::BeforeConfirmation)?;
                        let op = state.operations.last_mut().unwrap();
                        let i = &op.intent;
                        let confirmed = budget
                            .remaining()
                            .and_then(|remaining| {
                                http.request(
                                    bound.token.expose(),
                                    &i.handoff.list_id,
                                    &i.handoff.provider_id,
                                    None,
                                    remaining,
                                )
                            })
                            .and_then(|r| r.resource(&i.handoff.provider_id))
                            .and_then(|r| {
                                r.satisfied(&i.baseline, &i.handoff.provider_id, &i.patch.due)?;
                                ensure!(
                                    r.etag == op.patch_evidence.as_ref().unwrap().etag,
                                    "confirmation ETag changed"
                                );
                                Ok(r)
                            });
                        match confirmed {
                            Ok(r) => {
                                op.confirmation = Some(r);
                                Phase::Applied
                            }
                            Err(_) => Phase::Uncertain,
                        }
                    }
                }
            }
            Err(_) => Phase::Uncertain,
        };
        #[cfg(test)]
        if stop == Stop::OutcomeCas {
            storage::save_ref(&self.repo, STATE_REF, Some(&oid), &state)?;
        }
        state.operations.last_mut().unwrap().phases.push(outcome);
        self.persist(&mut oid, &state)?;
        Ok(report(&state, &oid, http.google))
    }
    fn dispatch(
        &self,
        permit: DispatchPermit,
        bearer: &str,
        http: &Http,
        budget: &Budget,
    ) -> Result<Result<crate::publisher_http::Response>> {
        self.check_origin(http)?;
        ensure!(
            permit.origin == self.binding.origin && permit.state_repo == self.repo,
            "dispatch permit installation/origin mismatch"
        );
        let (oid, state) = self.load()?;
        ensure!(
            oid == permit.state_oid
                && state
                    .operations
                    .last()
                    .is_some_and(|o| o.operation_id == permit.operation_id
                        && o.phase() == Phase::Attempting
                        && o.intent_sha256 == digest(&permit.intent).unwrap_or_default()),
            "dispatch permit no longer current"
        );
        let i = permit.intent;
        let remaining = budget.remaining()?;
        Ok(http.request(
            bearer,
            &i.handoff.list_id,
            &i.handoff.provider_id,
            Some((&i.baseline.etag, &i.patch.due)),
            remaining,
        ))
    }
    pub fn recover(&mut self, operation_id: &str) -> Result<Value> {
        let budget = Budget::new();
        let (oid, state) = self.load()?;
        let op = state.operations.last().context("no operation")?;
        ensure!(
            op.operation_id == operation_id,
            "unknown/current operation required"
        );
        if !matches!(op.phase(), Phase::Attempting | Phase::Uncertain) {
            return self.recover_local(oid, state);
        }
        let store = MacKeychainStore::for_profile(AuthProfile::TasksWrite)?;
        let oauth = GoogleOAuth::new()?;
        self.recover_with(
            operation_id,
            &store,
            &|remaining| oauth.with_request_timeout(remaining),
            &Http::google()?,
            &budget,
        )
    }
    fn recover_local(&self, mut oid: String, mut state: State) -> Result<Value> {
        if state
            .operations
            .last()
            .is_some_and(|o| o.phase() == Phase::Prepared)
        {
            state
                .operations
                .last_mut()
                .unwrap()
                .phases
                .push(Phase::NotDispatched);
            self.persist(&mut oid, &state)?;
        }
        Ok(report(&state, &oid, false))
    }
    fn recover_with<F, T>(
        &self,
        operation_id: &str,
        store: &dyn CredentialStore,
        oauth: &F,
        http: &Http,
        budget: &Budget,
    ) -> Result<Value>
    where
        F: Fn(Duration) -> hive_google_auth::Result<T> + Send + Sync,
        T: OAuthTransport,
    {
        let (mut oid, mut state) = self.load()?;
        let op = state.operations.last_mut().context("no operation")?;
        ensure!(
            op.operation_id == operation_id,
            "unknown/current operation required"
        );
        if !matches!(op.phase(), Phase::Attempting | Phase::Uncertain) {
            return self.recover_local(oid, state);
        }
        self.check_origin(http)?;
        let i = &op.intent;
        let h = &i.handoff;
        let bound = authenticate(&self.binding, h, store, oauth, budget)?;
        let response = http.request(
            bound.token.expose(),
            &h.list_id,
            &h.provider_id,
            None,
            budget.remaining()?,
        );
        let mut evidence = None;
        let next = match response {
            Ok(r) if r.status != 200 => Phase::Uncertain,
            Ok(r) => match r.resource(&h.provider_id) {
                Ok(r) => {
                    let phase = if r
                        .satisfied(&i.baseline, &h.provider_id, &i.patch.due)
                        .is_ok()
                    {
                        Phase::RecoveredSatisfied
                    } else if r.raw.get("due").is_none_or(Value::is_null)
                        && r.nondue() == i.baseline.nondue()
                    {
                        Phase::Uncertain
                    } else {
                        Phase::ConflictNeedsReview
                    };
                    evidence = Some(r);
                    phase
                }
                // An invalid/untrusted observation is not a factual conflict.
                // Leave it unresolved so a later exact GET can establish facts.
                Err(_) => Phase::Uncertain,
            },
            Err(_) => Phase::Uncertain,
        };
        let observed = evidence.is_some();
        if next != op.phase() {
            op.recovery_evidence = evidence;
            op.phases.push(next);
            self.persist(&mut oid, &state)?;
        }
        Ok(report(&state, &oid, http.google && observed))
    }
}

/// Exact reviewed Prepared commit and semantic intent, plus explicit write consent.
pub struct Approval<'a> {
    pub operation_id: &'a str,
    pub expected_state_oid: &'a str,
    pub expected_intent_sha256: &'a str,
    pub acknowledge_live_write: bool,
}
// Move-only, private, minted only by this process's successful Attempting CAS.
struct DispatchPermit {
    origin: String,
    state_repo: PathBuf,
    state_oid: String,
    operation_id: String,
    intent: Intent,
}

fn report(state: &State, oid: &str, authenticated_now: bool) -> Value {
    let op = state.operations.last();
    let google = state.manifest.origin == "Google";
    let applied = op.is_some_and(|o| o.phase() == Phase::Applied);
    json!({"schemaVersion":"1", "kind":"google-tasks-publisher-report", "origin":state.manifest.origin,
        "stateOid":oid, "operationId":op.map(|o| &o.operation_id), "intentSha256":op.map(|o| &o.intent_sha256), "phase":op.map(Operation::phase),
        "localOnly":true, "hostedReady":false, "publicationReady":false, "pushVerified":false,
        "remoteVerified":google && authenticated_now,
        "hiveWriteProven":google && applied && authenticated_now,
        "durableValidatedPatchEvidence":op.is_some_and(|o| o.patch_evidence.is_some()),
        "attribution":if applied && google {"validated-PATCH+confirmation+durable-ACK"} else {"UNKNOWN"},
        "operations":state.operations})
}

// Admission/network budget, not a hard wall-clock bound on native UI or fsync.
struct Budget {
    deadline: Instant,
}
impl Budget {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_secs(120),
        }
    }
    fn remaining(&self) -> Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .context("publisher admission/network budget expired")
    }
}
struct BoundedOAuth<'a, F> {
    factory: &'a F,
    budget: &'a Budget,
}
impl<F, T> OAuthTransport for BoundedOAuth<'_, F>
where
    F: Fn(Duration) -> hive_google_auth::Result<T> + Send + Sync,
    T: OAuthTransport,
{
    fn exchange(
        &self,
        _: &hive_google_auth::ClientConfig,
        _: &hive_google_auth::AuthorizationGrant,
    ) -> hive_google_auth::Result<hive_google_auth::TokenSet> {
        Err(hive_google_auth::AuthError::Denied)
    }
    fn refresh(
        &self,
        client: &hive_google_auth::ClientConfig,
        token: &hive_google_auth::SecretString,
    ) -> hive_google_auth::Result<hive_google_auth::TokenSet> {
        let remaining = self
            .budget
            .remaining()
            .map_err(|_| hive_google_auth::AuthError::Timeout)?
            .min(Duration::from_secs(30));
        let transport = (self.factory)(remaining)?;
        self.budget
            .remaining()
            .map_err(|_| hive_google_auth::AuthError::Timeout)?;
        transport.refresh(client, token)
    }
    fn identity(
        &self,
        token: &hive_google_auth::SecretString,
    ) -> hive_google_auth::Result<hive_google_auth::GoogleIdentity> {
        let remaining = self
            .budget
            .remaining()
            .map_err(|_| hive_google_auth::AuthError::Timeout)?
            .min(Duration::from_secs(30));
        let transport = (self.factory)(remaining)?;
        self.budget
            .remaining()
            .map_err(|_| hive_google_auth::AuthError::Timeout)?;
        transport.identity(token)
    }
}
fn authenticate<F, T>(
    binding: &Binding,
    handoff: &TaskHandoff,
    store: &dyn CredentialStore,
    factory: &F,
    budget: &Budget,
) -> Result<hive_google_auth::BoundAccessToken>
where
    F: Fn(Duration) -> hive_google_auth::Result<T> + Send + Sync,
    T: OAuthTransport,
{
    ensure!(
        store.profile() == AuthProfile::TasksWrite,
        "TasksWrite credential profile required"
    );
    budget.remaining()?;
    let transport = BoundedOAuth { factory, budget };
    let bound = Authenticator::new(store, &transport).access_token(&binding.connection)?;
    ensure!(
        bound.identity.namespace()? == handoff.account_key,
        "publisher account namespace mismatch"
    );
    budget.remaining()?;
    Ok(bound)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    None,
    BeforePrepared,
    AfterPrepared,
    #[cfg(test)]
    AttemptingCas,
    #[cfg(test)]
    ExpiredAfterAttempting,
    AfterAttempting,
    BeforePatch,
    AfterRemoteMutation,
    AfterPatchEvidence,
    BeforeConfirmation,
    #[cfg(test)]
    OutcomeCas,
}
impl Stop {
    fn at(self, point: Self) -> Result<()> {
        ensure!(self != point, "publisher process interruption");
        Ok(())
    }
}

#[cfg(test)]
#[path = "publisher_tests.rs"]
mod tests;
