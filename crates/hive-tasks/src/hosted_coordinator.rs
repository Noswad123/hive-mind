//! Test-only integrated Tasks protocol, not a hosted writer API. Transport-origin
//! floors are outside serialized state. No native store or live constructor.
//! Main/state observations and Google PATCH are NOT an atomic distributed fence.
use super::remote::{Exchange, Receipt, Remote, View};
use super::*;
use crate::publisher_http::{Http, Resource};
use serde::Serialize;

fn digest(v: &impl Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(v)?)))
}
fn nonce() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow!("randomness_failed"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy)]
enum Phase {
    Prepared,
    Attempting,
    Uncertain,
    RecoveredSatisfied,
    Conflict,
    Applied,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    operation_nonce: String,
    commit: String,
    source: Value,
    due: String,
    binding: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    intent: Intent,
    id: String,
    phase: Phase,
    owner: Option<String>,
    basis: Option<Resource>,
    patch: Option<Resource>,
    confirmation: Option<Resource>,
    recovery: Option<Resource>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u32,
    origin: String,
    namespace: String,
    operation: Option<Operation>,
}
// Non-Clone, non-Serde; never reconstructed from Attempting or a readback.
struct Permit {
    receipt: Receipt,
    old: String,
    invocation: String,
    operation: String,
}
struct Coordinator {
    remote: Remote,
    github: github::GitHub,
    http: Http,
    attempt_barrier: Option<std::sync::Arc<std::sync::Barrier>>,
}
impl Coordinator {
    fn namespace(b: &Binding) -> Result<String> {
        digest(
            &json!({"repositoryId":b.expected_repository_id, "repository":b.expected_repository,
            "handoff":b.handoff_sha256, "parser":b.trusted_parser_sha256,
            "account":b.expected_account_namespace,"connection":b.connection,"source":b.source_repo_id}),
        )
    }
    fn initialize(path: &Path, b: &Binding, github: github::GitHub, http: Http) -> Result<Self> {
        // No promotion by editing raw JSON or selecting a Google transport.
        ensure!(!http.google, "test_origin_only");
        let state = State {
            schema: 1,
            origin: "Loopback".into(),
            namespace: Self::namespace(b)?,
            operation: None,
        };
        let remote = Remote::initialize(path, &state.namespace, &serde_json::to_value(&state)?)?;
        Ok(Self {
            remote,
            github,
            http,
            attempt_barrier: None,
        })
    }
    fn load(&self, b: &Binding) -> Result<(View, State)> {
        ensure!(!self.http.google, "test_origin_only");
        let view = self.remote.observe()?;
        let state = Self::validate(b, view.history.iter())?;
        Ok((view, state))
    }
    fn validate<'a>(b: &Binding, history: impl IntoIterator<Item = &'a Value>) -> Result<State> {
        let mut previous: Option<State> = None;
        for raw in history {
            ensure!(
                serde_json::to_vec(raw)?.len() <= storage::LIMIT,
                "hosted_state_limit"
            );
            let state: State = serde_json::from_value(raw.clone())?;
            ensure!(
                state.schema == 1
                    && state.origin == "Loopback"
                    && state.namespace == Self::namespace(b)?,
                "hosted_state_binding_mismatch"
            );
            if let Some(op) = &state.operation {
                ensure!(
                    op.id == digest(&op.intent)?
                        && hex(&op.intent.operation_nonce, 64)
                        && op.intent.binding == state.namespace
                        && oid(&op.intent.commit),
                    "invalid_hosted_intent"
                );
                crate::publisher::validate_hosted_source(
                    &op.intent.source,
                    &op.intent.commit,
                    &op.intent.due,
                )?;
                if op.phase != Phase::Prepared {
                    ensure!(
                        op.owner.as_deref().is_some_and(|s| hex(s, 64)),
                        "missing_attempt_owner"
                    );
                    let basis = op.basis.as_ref().ok_or_else(|| anyhow!("missing_basis"))?;
                    let task = basis.validate(&b.reviewed_task_handoff.provider_id)?;
                    policy::accepted_fields(&task, &b.reviewed_task_handoff.baseline)?;
                    ensure!(task.due.is_none(), "dated_attempt_basis");
                    for r in [&op.patch, &op.confirmation, &op.recovery]
                        .into_iter()
                        .flatten()
                    {
                        r.validate(&b.reviewed_task_handoff.provider_id)?;
                    }
                    if let Some(patch) = &op.patch {
                        patch.satisfied(
                            basis,
                            &b.reviewed_task_handoff.provider_id,
                            &op.intent.due,
                        )?;
                        ensure!(patch.etag != basis.etag, "unchanged_patch_etag");
                    }
                    if op.phase == Phase::Applied {
                        let patch = op.patch.as_ref().ok_or_else(|| anyhow!("missing_patch"))?;
                        let confirmed = op
                            .confirmation
                            .as_ref()
                            .ok_or_else(|| anyhow!("missing_confirmation"))?;
                        confirmed.satisfied(
                            basis,
                            &b.reviewed_task_handoff.provider_id,
                            &op.intent.due,
                        )?;
                        ensure!(confirmed.etag == patch.etag, "confirmation_drift");
                    }
                    if op.phase == Phase::RecoveredSatisfied {
                        op.recovery
                            .as_ref()
                            .ok_or_else(|| anyhow!("missing_recovery"))?
                            .satisfied(
                                basis,
                                &b.reviewed_task_handoff.provider_id,
                                &op.intent.due,
                            )?;
                    }
                } else {
                    ensure!(
                        op.owner.is_none()
                            && op.basis.is_none()
                            && op.patch.is_none()
                            && op.confirmation.is_none()
                            && op.recovery.is_none(),
                        "prepared_with_attempt"
                    );
                    ensure!(
                        serde_json::to_vec(&state)?.len()
                            <= storage::LIMIT - 5 * crate::publisher_http::RESOURCE_LIMIT,
                        "insufficient_hosted_headroom"
                    );
                }
            }
            match (&previous, &state.operation) {
                (None, None) => {}
                (Some(prev), Some(next)) => {
                    if let Some(old) = &prev.operation {
                        ensure!(
                            next.id == old.id && digest(&next.intent)? == digest(&old.intent)?,
                            "intent_replaced"
                        );
                        ensure!(
                            matches!(
                                (old.phase, next.phase),
                                (Phase::Prepared, Phase::Attempting)
                                    | (
                                        Phase::Attempting,
                                        Phase::Attempting
                                            | Phase::Applied
                                            | Phase::Uncertain
                                            | Phase::RecoveredSatisfied
                                            | Phase::Conflict
                                    )
                                    | (
                                        Phase::Uncertain,
                                        Phase::RecoveredSatisfied | Phase::Conflict
                                    )
                            ),
                            "invalid_hosted_transition"
                        );
                        if old.phase != Phase::Prepared {
                            ensure!(
                                next.owner == old.owner
                                    && digest(&next.basis)? == digest(&old.basis)?,
                                "attempt_replaced"
                            );
                            if old.patch.is_some() {
                                ensure!(
                                    digest(&next.patch)? == digest(&old.patch)?,
                                    "patch_evidence_replaced"
                                );
                            }
                        }
                    } else {
                        ensure!(next.phase == Phase::Prepared, "missing_prepared");
                    }
                }
                _ => anyhow::bail!("hosted_history_reset"),
            }
            previous = Some(state);
        }
        previous.ok_or_else(|| anyhow!("empty_history"))
    }
    fn persist(
        &self,
        b: &Binding,
        view: &View,
        state: &State,
        invocation: &str,
        lost: bool,
    ) -> Result<Receipt> {
        let proposed = serde_json::to_value(state)?;
        Self::validate(b, view.history.iter().chain(std::iter::once(&proposed)))?;
        let operation = state
            .operation
            .as_ref()
            .ok_or_else(|| anyhow!("no_operation"))?;
        match self
            .remote
            .exchange(view, &proposed, &operation.id, invocation, lost)?
        {
            Exchange::Fresh(r) => Ok(r),
            Exchange::Rejected => anyhow::bail!("stale_remote_state"),
            Exchange::Unknown => anyhow::bail!("unknown_remote_state_ack"),
        }
    }
    fn main(&self, b: &Binding, commit: &str, token: &str, deadline: Instant) -> Result<()> {
        let current = self.github.verify(b, commit, token, deadline)?;
        ensure!(current.matches(b, commit), "main_mismatch");
        Ok(())
    }
    fn prepare(
        &self,
        inputs: &Inputs<'_>,
        b: &Binding,
        a: &AdmittedPush,
        lost: bool,
    ) -> Result<()> {
        let snapshot = source(inputs, b, a)?;
        let (view, mut state) = self.load(b)?;
        ensure!(state.operation.is_none(), "target_blocked_or_closed");
        let h = &b.reviewed_task_handoff;
        let candidate = policy::evaluate(
            &snapshot,
            &b.source_repo_id,
            h,
            &observation(h, h.baseline.clone()),
        )?;
        ensure!(
            candidate.decision() == FirstDue::SetFirstDue,
            "first_due_only"
        );
        let intent = Intent {
            operation_nonce: nonce()?,
            commit: a.commit.clone(),
            source: candidate.source().clone(),
            due: candidate.desired_due().into(),
            binding: state.namespace.clone(),
        };
        state.operation = Some(Operation {
            id: digest(&intent)?,
            intent,
            phase: Phase::Prepared,
            owner: None,
            basis: None,
            patch: None,
            confirmation: None,
            recovery: None,
        });
        unchanged(inputs, b, a)?;
        self.persist(b, &view, &state, &nonce()?, lost)?;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn attempt(
        &self,
        inputs: &Inputs<'_>,
        b: &Binding,
        a: &AdmittedPush,
        store: &ImmutableTasksWriteStore,
        oauth: &dyn OAuthTransport,
        github_token: &str,
        lost: bool,
        deadline: Instant,
    ) -> Result<(Permit, hive_google_auth::BoundAccessToken)> {
        let snapshot = source(inputs, b, a)?;
        let (view, mut state) = self.load(b)?;
        let op = state
            .operation
            .as_mut()
            .ok_or_else(|| anyhow!("no_operation"))?;
        ensure!(
            op.phase == Phase::Prepared && op.intent.commit == a.commit,
            "not_prepared"
        );
        let bound = Authenticator::new(store, oauth)
            .access_token(&b.connection)
            .map_err(auth_error)?;
        ensure!(
            bound.identity.namespace()? == b.expected_account_namespace,
            "account_mismatch"
        );
        unchanged(inputs, b, a)?;
        let h = &b.reviewed_task_handoff;
        let basis = self
            .http
            .request(
                bound.token.expose(),
                &h.list_id,
                &h.provider_id,
                None,
                remaining(deadline)?,
            )?
            .resource(&h.provider_id)?;
        let candidate = policy::evaluate(
            &snapshot,
            &b.source_repo_id,
            h,
            &observation(h, basis.validate(&h.provider_id)?),
        )?;
        ensure!(
            candidate.decision() == FirstDue::SetFirstDue
                && candidate.source() == &op.intent.source
                && candidate.desired_due() == op.intent.due,
            "frozen_intent_drift"
        );
        self.main(b, &a.commit, github_token, deadline)?;
        unchanged(inputs, b, a)?;
        let invocation = nonce()?;
        op.owner = Some(invocation.clone());
        op.basis = Some(basis);
        op.phase = Phase::Attempting;
        let operation = op.id.clone();
        if let Some(barrier) = &self.attempt_barrier {
            barrier.wait();
        }
        let receipt = self.persist(b, &view, &state, &invocation, lost)?;
        Ok((
            Permit {
                receipt,
                old: view.oid,
                invocation,
                operation,
            },
            bound,
        ))
    }
    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &self,
        permit: Permit,
        inputs: &Inputs<'_>,
        b: &Binding,
        a: &AdmittedPush,
        bearer: &str,
        github_token: &str,
        deadline: Instant,
        stop: Stop,
    ) -> Result<Phase> {
        // Main and state can still race AFTER this observation. This is not a
        // server-side exactly-once promise; the creator's permit is never renewed.
        let snapshot = source(inputs, b, a)?;
        self.main(b, &a.commit, github_token, deadline)?;
        let (view, mut state) = self.load(b)?;
        let op = state
            .operation
            .as_mut()
            .ok_or_else(|| anyhow!("no_operation"))?;
        ensure!(
            permit.receipt.binds(
                &self.remote,
                &permit.old,
                &view.oid,
                &permit.operation,
                &permit.invocation
            ) && op.id == permit.operation
                && op.owner.as_deref() == Some(&permit.invocation)
                && op.phase == Phase::Attempting,
            "creator_permit_not_current"
        );
        unchanged(inputs, b, a)?;
        let basis = op.basis.as_ref().ok_or_else(|| anyhow!("missing_basis"))?;
        let h = &b.reviewed_task_handoff;
        let candidate = policy::evaluate(
            &snapshot,
            &b.source_repo_id,
            h,
            &observation(h, basis.validate(&h.provider_id)?),
        )?;
        ensure!(
            op.intent.commit == a.commit
                && candidate.source() == &op.intent.source
                && candidate.desired_due() == op.intent.due
                && candidate.decision() == FirstDue::SetFirstDue,
            "frozen_intent_drift"
        );
        let response = self.http.request(
            bearer,
            &h.list_id,
            &h.provider_id,
            Some((&basis.etag, &op.intent.due)),
            remaining(deadline)?,
        );
        if stop == Stop::AfterPatch {
            anyhow::bail!("interrupted_after_patch");
        }
        let patch = response
            .and_then(|r| r.resource(&h.provider_id))
            .and_then(|r| r.mutation(basis, &h.provider_id, &op.intent.due));
        match patch {
            Ok(r) => op.patch = Some(r),
            Err(_) => {
                op.phase = Phase::Uncertain;
                self.persist(
                    b,
                    &view,
                    &state,
                    &permit.invocation,
                    stop == Stop::LoseOutcomeAck,
                )?;
                return Ok(Phase::Uncertain);
            }
        }
        self.persist(
            b,
            &view,
            &state,
            &permit.invocation,
            stop == Stop::LoseEvidenceAck,
        )?;
        let (view, mut state) = self.load(b)?;
        let op = state.operation.as_mut().unwrap();
        let confirmed = self
            .http
            .request(
                bearer,
                &h.list_id,
                &h.provider_id,
                None,
                remaining(deadline)?,
            )
            .and_then(|r| r.resource(&h.provider_id))
            .and_then(|r| {
                r.satisfied(op.basis.as_ref().unwrap(), &h.provider_id, &op.intent.due)?;
                ensure!(
                    r.etag == op.patch.as_ref().unwrap().etag,
                    "confirmation_drift"
                );
                Ok(r)
            });
        let phase = match confirmed {
            Ok(r) => {
                op.confirmation = Some(r);
                Phase::Applied
            }
            Err(_) => Phase::Uncertain,
        };
        op.phase = phase;
        self.persist(
            b,
            &view,
            &state,
            &permit.invocation,
            stop == Stop::LoseOutcomeAck,
        )?;
        Ok(phase)
    }
    // No parser, source path, claims, main or creator permit is required. GET only.
    fn recover(
        &self,
        b: &Binding,
        store: &ImmutableTasksWriteStore,
        oauth: &dyn OAuthTransport,
        deadline: Instant,
    ) -> Result<Phase> {
        let (view, mut state) = self.load(b)?;
        let op = state
            .operation
            .as_mut()
            .ok_or_else(|| anyhow!("no_operation"))?;
        if !matches!(op.phase, Phase::Attempting | Phase::Uncertain) {
            return Ok(op.phase);
        }
        let bound = Authenticator::new(store, oauth)
            .access_token(&b.connection)
            .map_err(auth_error)?;
        ensure!(
            bound.identity.namespace()? == b.expected_account_namespace,
            "account_mismatch"
        );
        let h = &b.reviewed_task_handoff;
        let observed = self
            .http
            .request(
                bound.token.expose(),
                &h.list_id,
                &h.provider_id,
                None,
                remaining(deadline)?,
            )
            .and_then(|r| r.resource(&h.provider_id));
        let Ok(observed) = observed else {
            return Ok(Phase::Uncertain);
        };
        let basis = op.basis.as_ref().unwrap();
        let next = if observed
            .satisfied(basis, &h.provider_id, &op.intent.due)
            .is_ok()
        {
            Phase::RecoveredSatisfied
        } else if observed.raw.get("due").is_none_or(Value::is_null)
            && observed.nondue() == basis.nondue()
        {
            Phase::Uncertain
        } else {
            Phase::Conflict
        };
        // An unchanged observation grants no authority and must not manufacture
        // revocation of a paused creator. Repeated recovery consumes no history.
        if next == Phase::Uncertain {
            return Ok(next);
        }
        op.recovery = Some(observed);
        op.phase = next;
        self.persist(b, &view, &state, &nonce()?, false)?;
        Ok(next)
    }
}
#[derive(PartialEq, Eq)]
enum Stop {
    None,
    AfterPatch,
    LoseEvidenceAck,
    LoseOutcomeAck,
}

#[path = "hosted_coordinator_tests.rs"]
mod tests;
