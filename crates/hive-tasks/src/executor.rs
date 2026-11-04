use crate::{
    policy::{self, FirstDue, TaskObservation},
    storage::{self, Lock},
};
use anyhow::{Context, Result, ensure};
use hive_calendar::task_snapshot::{CommittedTaskSnapshot, read_committed_tasks};
use hive_capture::{
    GoogleTask,
    handoff::{TaskHandoff, opaque_id, validate_open_task},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: String,
    execution_mode: String,
    state_id: String,
    state_repo: PathBuf,
    world_path: PathBuf,
    world_id: String,
    source_repo_id: String,
    account_key: String,
    list_id: String,
    task_id: String,
}

/// Explicit initialization input; only synthetic tasks should be used.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    schema_version: String,
    world_id: String,
    source_repo_id: String,
    account_key: String,
    list_id: String,
    task: GoogleTask,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct World {
    manifest: Manifest,
    fake_revision: u64,
    durable_patch_count: u64,
    task: Option<GoogleTask>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    fn blocks(self) -> bool {
        matches!(
            self,
            Self::Prepared | Self::Attempting | Self::Uncertain | Self::ConflictNeedsReview
        )
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
                    Self::Uncertain | Self::RecoveredSatisfied | Self::ConflictNeedsReview
                )
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceEvidence {
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Intent {
    operation_id: String,
    world_id: String,
    source_repo_id: String,
    source_repo: PathBuf,
    commit: String,
    parser_sha256: String,
    source: SourceEvidence,
    handoff: TaskHandoff,
    expected_fake_revision: u64,
    patch: DuePatch,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DuePatch {
    due: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Operation {
    intent: Intent,
    phases: Vec<Phase>,
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

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn operation_id(intent: &Intent) -> Result<String> {
    let mut copy = intent.clone();
    copy.operation_id.clear();
    Ok(hash(&serde_json::to_vec(&copy)?))
}
fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl State {
    fn validate(&self, repo: &Path) -> Result<()> {
        let m = &self.manifest;
        ensure!(
            m.schema_version == "1"
                && m.execution_mode == "fake-only"
                && m.state_repo == repo
                && hex(&m.state_id, 64)
                && opaque_id(&m.world_id)
                && opaque_id(&m.source_repo_id)
                && opaque_id(&m.list_id)
                && opaque_id(&m.task_id)
                && m.account_key
                    .strip_prefix("google-sub-v1:")
                    .is_some_and(|s| hex(s, 64))
                && m.world_path != m.state_repo,
            "invalid fake manifest"
        );
        storage::path(&m.world_path, false)?;
        ensure!(self.operations.len() <= 128, "operation retention limit");
        let mut ids = std::collections::BTreeSet::new();
        let mut pending = 0;
        for (index, op) in self.operations.iter().enumerate() {
            let i = &op.intent;
            i.handoff.validate()?;
            ensure!(
                i.world_id == m.world_id
                    && i.source_repo_id == m.source_repo_id
                    && i.handoff.source_repo_id == m.source_repo_id
                    && i.handoff.account_key == m.account_key
                    && i.handoff.list_id == m.list_id
                    && i.handoff.provider_id == m.task_id,
                "intent namespace mismatch"
            );
            ensure!(
                i.operation_id == operation_id(i)? && ids.insert(&i.operation_id),
                "invalid or duplicate operation ID"
            );
            ensure!(
                matches!(i.commit.len(), 40 | 64)
                    && hex(&i.commit, i.commit.len())
                    && hex(&i.source.git_blob_id, i.commit.len())
                    && i.source
                        .source_revision
                        .strip_prefix("sha256:")
                        .is_some_and(|s| hex(s, 64))
                    && i.source.line > 0
                    && i.source_repo.is_absolute()
                    && i.source_repo.as_os_str().len() <= 4096
                    && hex(&i.parser_sha256, 64)
                    && i.expected_fake_revision > 0,
                "invalid frozen provenance"
            );
            ensure!(
                i.patch.due == format!("{}T00:00:00.000Z", i.source.metadata.due),
                "invalid due-only intent"
            );
            time::Date::parse(
                &i.source.metadata.due,
                &time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")?,
            )?;
            ensure!(
                matches!(
                    op.phases.first(),
                    Some(Phase::Prepared | Phase::AlreadySatisfied)
                ) && op.phases.len() <= 128,
                "invalid initial phase"
            );
            ensure!(
                op.phases.windows(2).all(|p| p[1].follows(p[0])),
                "invalid phase transition"
            );
            pending += usize::from(op.phase().blocks());
            if op.phase().blocks()
                || matches!(
                    op.phase(),
                    Phase::Applied | Phase::RecoveredSatisfied | Phase::AlreadySatisfied
                )
            {
                ensure!(
                    index + 1 == self.operations.len(),
                    "operation follows blocked or satisfied target"
                );
            }
        }
        ensure!(pending <= 1, "multiple unresolved operations");
        Ok(())
    }
}

fn world_lock(world: &Path) -> PathBuf {
    world.with_file_name(format!(
        "{}.hive-fake-lock",
        world.file_name().unwrap().to_string_lossy()
    ))
}

pub fn initialize(state_repo: &Path, world: &Path, fixture: &Path) -> Result<Value> {
    storage::path(state_repo, false)?;
    storage::path(world, false)?;
    ensure!(
        !state_repo.exists() && !world.exists() && !world_lock(world).exists(),
        "fake init refuses existing paths"
    );
    let fixture: Fixture = storage::read(fixture)?;
    ensure!(
        fixture.schema_version == "1"
            && opaque_id(&fixture.world_id)
            && opaque_id(&fixture.source_repo_id)
            && opaque_id(&fixture.list_id)
            && opaque_id(&fixture.task.id)
            && fixture
                .account_key
                .strip_prefix("google-sub-v1:")
                .is_some_and(|s| hex(s, 64)),
        "invalid fake fixture namespace"
    );
    validate_open_task(&fixture.task)?;
    ensure!(fixture.task.due.is_none(), "fixture must be undated");
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).context("state ID randomness unavailable")?;
    let manifest = Manifest {
        schema_version: "1".into(),
        execution_mode: "fake-only".into(),
        state_id: hash(&random),
        state_repo: state_repo.into(),
        world_path: world.into(),
        world_id: fixture.world_id,
        source_repo_id: fixture.source_repo_id,
        account_key: fixture.account_key,
        list_id: fixture.list_id,
        task_id: fixture.task.id.clone(),
    };
    fs::create_dir(state_repo)?;
    storage::git(
        state_repo,
        &["init", "--bare", "--template=", "--object-format=sha1", "."],
        b"",
    )?;
    storage::create(&state_repo.join("hive-fake-lock"), b"")?;
    storage::create(&world_lock(world), b"")?;
    let _state_lock = Lock::acquire(&state_repo.join("hive-fake-lock"))?;
    let _world_lock = Lock::acquire(&world_lock(world))?;
    let state = State {
        manifest: manifest.clone(),
        operations: Vec::new(),
    };
    state.validate(state_repo)?;
    let w = World {
        manifest,
        fake_revision: 1,
        durable_patch_count: 0,
        task: Some(fixture.task),
    };
    storage::create(world, &serde_json::to_vec(&w)?)?;
    storage::save(state_repo, None, &state)?;
    Ok(report(&state, None, Some(&w)))
}

fn report(state: &State, op: Option<&Operation>, world: Option<&World>) -> Value {
    json!({"schemaVersion":"1","executionMode":"fake-only","provider":"durable-fake",
        "googleConditionalWriteValidated":false,"productionReady":false,"publicationReady":false,
        "pushVerified":false,"remoteVerified":false,"hiveWriteProven":false,
        "worldId":state.manifest.world_id,"sourceRepoId":state.manifest.source_repo_id,
        "operationId":op.map(|o| &o.intent.operation_id),"phase":op.map(Operation::phase),
        "fakeApplied":op.is_some_and(|o| o.phase() == Phase::Applied),
        "attribution":if op.is_some_and(|o| o.phase() == Phase::RecoveredSatisfied) {"UNKNOWN"} else {"not-asserted"},
        "durablePatchCount":world.map(|w| w.durable_patch_count),"fakeRevision":world.map(|w| w.fake_revision),
        "operations":state.operations})
}

/// Read-only: does not create or acquire a lock file or access the fake world.
pub fn inspect(state_repo: &Path) -> Result<Value> {
    let (_, state): (_, State) = storage::load(state_repo)?;
    state.validate(state_repo)?;
    Ok(report(&state, None, None))
}

pub struct SourceOptions<'a> {
    pub repo: &'a Path,
    pub commit: &'a str,
    pub mw_bin: &'a Path,
    pub source_repo_id: &'a str,
}

// Cannot be deserialized or constructed from a caller-supplied snapshot.
struct CollectedSource {
    snapshot: CommittedTaskSnapshot,
    repo: PathBuf,
}
impl CollectedSource {
    fn collect(options: &SourceOptions<'_>) -> Result<Self> {
        Ok(Self {
            snapshot: read_committed_tasks(options.repo, options.commit, options.mw_bin)?,
            repo: options.repo.canonicalize()?,
        })
    }
}

/// Holds both exclusive locks throughout observation, intent commits and private dispatch.
pub struct FakeSession {
    state_repo: PathBuf,
    world_path: PathBuf,
    _state_lock: Lock,
    _world_lock: Lock,
}
struct DispatchToken {
    intent: Intent,
    attempting_ref: String,
}

impl FakeSession {
    pub fn open(state_repo: &Path, world: &Path) -> Result<Self> {
        storage::path(state_repo, true)?;
        storage::path(world, true)?;
        let state_lock = Lock::acquire(&state_repo.join("hive-fake-lock"))?;
        let world_lock = Lock::acquire(&world_lock(world))?;
        let session = Self {
            state_repo: state_repo.into(),
            world_path: world.into(),
            _state_lock: state_lock,
            _world_lock: world_lock,
        };
        session.load()?;
        Ok(session)
    }

    fn load(&self) -> Result<(String, State, World)> {
        let (oid, state): (_, State) = storage::load(&self.state_repo)?;
        state.validate(&self.state_repo)?;
        ensure!(
            state.manifest.world_path == self.world_path,
            "fake world path mismatch"
        );
        let world: World = storage::read(&self.world_path)?;
        ensure!(
            world.manifest == state.manifest
                && world.fake_revision > 0
                && world.durable_patch_count < world.fake_revision,
            "fake world namespace or revision mismatch"
        );
        Ok((oid, state, world))
    }

    fn persist(&self, oid: &mut String, state: &State) -> Result<()> {
        state.validate(&self.state_repo)?;
        *oid = storage::save(&self.state_repo, Some(oid), state)?;
        Ok(())
    }

    pub fn execute_first_due(
        &mut self,
        source: SourceOptions<'_>,
        handoff: &TaskHandoff,
    ) -> Result<Value> {
        self.execute(source, handoff, Stop::None)
    }

    fn execute(
        &mut self,
        source: SourceOptions<'_>,
        handoff: &TaskHandoff,
        _stop: Stop,
    ) -> Result<Value> {
        #[cfg(test)]
        let stop = _stop;
        let (mut oid, mut state, world) = self.load()?;
        ensure!(
            !state.operations.iter().any(|o| o.phase().blocks()),
            "unresolved fake operation blocks execution"
        );
        ensure!(state.operations.len() < 128, "operation retention limit");
        let m = &state.manifest;
        ensure!(
            source.source_repo_id == m.source_repo_id
                && handoff.source_repo_id == m.source_repo_id
                && handoff.account_key == m.account_key
                && handoff.list_id == m.list_id
                && handoff.provider_id == m.task_id,
            "fake target/source namespace mismatch"
        );
        let collected = CollectedSource::collect(&source)?;
        let observation = TaskObservation {
            schema_version: "1".into(),
            kind: "google-task-observation".into(),
            source_repo_id: m.source_repo_id.clone(),
            account_key: m.account_key.clone(),
            list_id: m.list_id.clone(),
            task: world
                .task
                .clone()
                .context("fake target missing; no recreation")?,
        };
        let candidate = policy::evaluate(
            &collected.snapshot,
            source.source_repo_id,
            handoff,
            &observation,
        )?;
        ensure!(
            candidate.decision() != FirstDue::Conflict,
            "remote due conflicts; no rescheduling"
        );
        // A previous satisfied/applied target is never reopened using a stale or newer source.
        if let Some(previous) = state.operations.iter().find(|o| {
            matches!(
                o.phase(),
                Phase::Applied | Phase::RecoveredSatisfied | Phase::AlreadySatisfied
            )
        }) {
            return Ok(report(&state, Some(previous), Some(&world)));
        }
        let mut intent = Intent {
            operation_id: String::new(),
            world_id: m.world_id.clone(),
            source_repo_id: m.source_repo_id.clone(),
            source_repo: collected.repo,
            commit: collected.snapshot.commit,
            parser_sha256: collected.snapshot.parser_sha256,
            source: serde_json::from_value(candidate.source().clone())?,
            handoff: handoff.clone(),
            expected_fake_revision: world.fake_revision,
            patch: DuePatch {
                due: candidate.desired_due().into(),
            },
        };
        intent.operation_id = operation_id(&intent)?;
        if let Some(previous) = state
            .operations
            .iter()
            .find(|o| o.intent.operation_id == intent.operation_id)
        {
            return Ok(report(&state, Some(previous), Some(&world)));
        }
        #[cfg(test)]
        stop.at(Stop::BeforeIntent)?;
        let phase = if candidate.decision() == FirstDue::AlreadySatisfied {
            Phase::AlreadySatisfied
        } else {
            Phase::Prepared
        };
        state.operations.push(Operation {
            intent: intent.clone(),
            phases: vec![phase],
        });
        self.persist(&mut oid, &state)?;
        if phase == Phase::AlreadySatisfied {
            return Ok(report(&state, state.operations.last(), Some(&world)));
        }
        #[cfg(test)]
        stop.at(Stop::AfterPrepared)?;
        #[cfg(test)]
        if stop == Stop::AttemptingCas {
            storage::save(&self.state_repo, Some(&oid), &state)?;
        }
        state
            .operations
            .last_mut()
            .unwrap()
            .phases
            .push(Phase::Attempting);
        self.persist(&mut oid, &state)?;
        // Only this invocation's successful exact-ref CAS can mint a move-only token.
        let token = DispatchToken {
            intent,
            attempting_ref: oid.clone(),
        };
        #[cfg(test)]
        stop.at(Stop::AfterAttempting)?;
        #[cfg(test)]
        if stop == Stop::RevisionRace {
            let mut changed: World = storage::read(&self.world_path)?;
            changed.fake_revision += 1;
            storage::atomic(&self.world_path, &changed)?;
        }
        let outcome = self.dispatch(token, _stop)?;
        #[cfg(test)]
        stop.at(Stop::BeforeOutcome)?;
        #[cfg(test)]
        if stop == Stop::OutcomeCas {
            storage::save(&self.state_repo, Some(&oid), &state)?;
        }
        state.operations.last_mut().unwrap().phases.push(outcome);
        self.persist(&mut oid, &state)?;
        let (_, _, world) = self.load()?;
        Ok(report(&state, state.operations.last(), Some(&world)))
    }

    fn dispatch(&self, token: DispatchToken, _stop: Stop) -> Result<Phase> {
        let (oid, state, mut world) = self.load()?;
        ensure!(
            oid == token.attempting_ref,
            "dispatch ref no longer current"
        );
        let i = token.intent;
        ensure!(
            state.operations.last().is_some_and(
                |o| o.intent.operation_id == i.operation_id && o.phase() == Phase::Attempting
            ),
            "dispatch intent no longer current"
        );
        if world.fake_revision != i.expected_fake_revision
            || world.task.as_ref().is_none_or(|t| {
                t.due.is_some() || policy::accepted_fields(t, &i.handoff.baseline).is_err()
            })
        {
            return Ok(Phase::RejectedConflict);
        }
        // The sole mutation payload has exactly one field. Fake revisions are NOT Google ETags.
        world.task.as_mut().unwrap().due = Some(i.patch.due);
        world.fake_revision = world
            .fake_revision
            .checked_add(1)
            .context("fake revision exhausted")?;
        world.durable_patch_count = world
            .durable_patch_count
            .checked_add(1)
            .context("patch count exhausted")?;
        storage::atomic(&self.world_path, &world)?;
        #[cfg(test)]
        _stop.at(Stop::AfterMutationLostResponse)?;
        Ok(Phase::Applied)
    }

    pub fn recover(&mut self, operation_id: &str) -> Result<Value> {
        let (mut oid, mut state, world) = self.load()?;
        let index = state
            .operations
            .iter()
            .position(|o| o.intent.operation_id == operation_id)
            .context("unknown fake operation")?;
        let op = &state.operations[index];
        let next = match op.phase() {
            Phase::Prepared => Some(Phase::NotDispatched),
            Phase::Attempting | Phase::Uncertain => {
                let i = &op.intent;
                Some(match &world.task {
                    None => Phase::Uncertain,
                    Some(t) if policy::accepted_fields(t, &i.handoff.baseline).is_err() => {
                        Phase::ConflictNeedsReview
                    }
                    Some(t) => match &t.due {
                        Some(d) if policy::due_matches(d, &i.patch.due) => {
                            Phase::RecoveredSatisfied
                        }
                        None => Phase::Uncertain,
                        Some(_) => Phase::ConflictNeedsReview,
                    },
                })
            }
            _ => None,
        };
        if let Some(next) = next.filter(|next| *next != op.phase()) {
            let phases = &mut state.operations[index].phases;
            // Older state may contain repeated Uncertain observations up to the
            // history bound. Retain transitions, not redundant same-phase entries.
            phases.dedup();
            phases.push(next);
            self.persist(&mut oid, &state)?;
        }
        Ok(report(&state, Some(&state.operations[index]), Some(&world)))
    }
}

// No production failpoint injection surface.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    None,
    #[cfg(test)]
    BeforeIntent,
    #[cfg(test)]
    AfterPrepared,
    #[cfg(test)]
    AfterAttempting,
    #[cfg(test)]
    AfterMutationLostResponse,
    #[cfg(test)]
    BeforeOutcome,
    #[cfg(test)]
    AttemptingCas,
    #[cfg(test)]
    RevisionRace,
    #[cfg(test)]
    OutcomeCas,
}
#[cfg(test)]
impl Stop {
    fn at(self, point: Self) -> Result<()> {
        ensure!(self != point, "simulated process interruption");
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
