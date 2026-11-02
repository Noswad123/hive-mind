//! Read-only, ledger-relative advice. Task contents in this output are private data.
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};
use serde::Serialize;

use crate::{
    Binding, CommittedSnapshot, ExpectedTask, Ledger, LedgerInput, LedgerLocation, Lifecycle,
    PlanOptions, PreviewOptions, ProspectiveEvent, SourceAck, SourceTask, ledger, projection,
    source,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub schema_version: &'static str,
    pub kind: &'static str,
    pub mode: &'static str,
    pub commit: String,
    pub parser_sha256: String,
    pub binding: Binding,
    pub ledger: Option<LedgerBasis>,
    pub publication_ready: bool,
    pub remote_verified: bool,
    pub ledger_head_verified: bool,
    pub requires_ledger_compare_and_swap: bool,
    pub diagnostics: Vec<&'static str>,
    pub counts: PlanCounts,
    pub entries: Vec<EntryPlan>,
    pub unenrolled_observations: Vec<UnenrolledObservation>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerBasis {
    pub commit: String,
    pub path: String,
    #[serde(rename = "gitBlobID")]
    pub git_blob_id: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCounts {
    pub entries: usize,
    pub noop: usize,
    pub update: usize,
    pub review: usize,
    pub blocked: usize,
    pub retired: usize,
    pub unenrolled_observations: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanDecision {
    Noop,
    Update,
    Review,
    Blocked,
    Retired,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPlan {
    pub logical_id: String,
    pub decision: PlanDecision,
    pub publication_ready: bool,
    pub diagnostics: Vec<&'static str>,
    pub source_ack: SourceAck,
    pub current_source: Option<SourceAck>,
    pub event_id: Option<String>,
    pub base_etag: Option<String>,
    pub last_acknowledged_payload_hash: Option<String>,
    pub desired_payload_hash: Option<String>,
    pub prospective_event: Option<ProspectiveEvent>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnenrolledObservation {
    pub source_ack: SourceAck,
    pub publication_ready: bool,
    pub diagnostics: Vec<&'static str>,
    pub due: Option<String>,
    pub desired_payload_hash: Option<String>,
    pub prospective_event: Option<ProspectiveEvent>,
}

pub(crate) fn plan(options: &PlanOptions) -> Result<Plan> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let location = match &options.ledger {
        LedgerInput::BootstrapPreview => None,
        LedgerInput::Committed {
            repository,
            commit,
            path,
        } => Some(LedgerLocation {
            repository: repository.clone(),
            commit: commit.clone(),
            path: path.clone(),
        }),
    };
    let context = load_context(
        &options.source,
        &options.binding,
        location.as_ref(),
        deadline,
    )?;
    let result = build_plan(
        &context.target,
        &options.binding,
        context.ledger.as_ref(),
        context.basis,
    )?;
    ensure!(Instant::now() < deadline, "calendar plan deadline exceeded");
    Ok(result)
}

pub(crate) struct PlanningContext {
    pub target: CommittedSnapshot,
    pub ledger: Option<Ledger>,
    pub basis: Option<LedgerBasis>,
}

pub(crate) fn load_context(
    source: &PreviewOptions,
    binding: &Binding,
    ledger: Option<&LedgerLocation>,
    deadline: Instant,
) -> Result<PlanningContext> {
    ledger::validate_binding(binding)?;
    let target = source::read_snapshot_before(
        &source.repository,
        &source.commit,
        &source.mw_binary,
        deadline,
    )?;
    let (ledger, basis) = match ledger {
        None => (None, None),
        Some(LedgerLocation {
            repository,
            commit,
            path,
        }) => {
            let blob = source::read_json_blob_before(repository, commit, path, deadline)?;
            let ledger = ledger::decode(&blob.content, binding)?;
            validate_history(
                &target,
                &ledger,
                deadline,
                |commit| {
                    source::read_snapshot_before(
                        &source.repository,
                        commit,
                        &source.mw_binary,
                        deadline,
                    )
                },
                |ancestor, descendant| {
                    source::require_ancestor_before(
                        &source.repository,
                        ancestor,
                        descendant,
                        deadline,
                    )
                },
            )?;
            (
                Some(ledger),
                Some(LedgerBasis {
                    commit: commit.clone(),
                    path: path.clone(),
                    git_blob_id: blob.object_id,
                }),
            )
        }
    };
    ensure!(Instant::now() < deadline, "calendar plan deadline exceeded");
    Ok(PlanningContext {
        target,
        ledger,
        basis,
    })
}

fn validate_history(
    target: &CommittedSnapshot,
    ledger: &Ledger,
    deadline: Instant,
    mut read: impl FnMut(&str) -> Result<CommittedSnapshot>,
    mut require_ancestor: impl FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    let mut pairs = BTreeSet::new();
    let mut ancestry = |ancestor: &str, descendant: &str| -> Result<()> {
        ensure!(Instant::now() < deadline, "calendar plan deadline exceeded");
        if pairs.insert((ancestor.to_owned(), descendant.to_owned())) {
            // Even equal IDs go through the hook: strings alone are not commit proof.
            require_ancestor(ancestor, descendant)
                .map_err(|_| anyhow!("source history unavailable or not ancestral"))?;
        }
        Ok(())
    };
    ancestry(&ledger.source_watermark, &target.commit)?;
    let mut proofs: BTreeMap<&str, Vec<&SourceAck>> = BTreeMap::new();
    for entry in &ledger.entries {
        ancestry(&entry.source_ack.commit, &ledger.source_watermark)?;
        proofs
            .entry(&entry.source_ack.commit)
            .or_default()
            .push(&entry.source_ack);
        if let Some(pending) = &entry.pending {
            ancestry(&pending.intended_source.commit, &target.commit)?;
            proofs
                .entry(&pending.intended_source.commit)
                .or_default()
                .push(&pending.intended_source);
        }
        if let Some(tombstone) = &entry.tombstone {
            ancestry(&tombstone.source_commit, &ledger.source_watermark)?;
        }
    }
    for (commit, acks) in proofs {
        ensure!(Instant::now() < deadline, "calendar plan deadline exceeded");
        let acks: Vec<_> = acks
            .into_iter()
            .filter(|ack| ack.parser_sha256 == target.parser_sha256)
            .collect();
        if acks.is_empty() {
            continue;
        }
        // Retain only the target and one historical snapshot, never all history files.
        let historical;
        let snapshot = if commit == target.commit {
            target
        } else {
            historical =
                read(commit).map_err(|_| anyhow!("source history snapshot unavailable"))?;
            &historical
        };
        ensure!(
            snapshot.commit == commit,
            "source history snapshot mismatch"
        );
        ensure!(
            snapshot.parser_sha256 == target.parser_sha256,
            "source parser changed during planning"
        );
        for ack in acks {
            let candidates: Vec<_> = snapshot
                .items
                .iter()
                .filter(|task| locator_matches(task, ack))
                .collect();
            ensure!(candidates.len() == 1, "ledger source evidence mismatch");
            ensure!(
                observation(snapshot, candidates[0])? == *ack,
                "ledger source evidence mismatch"
            );
        }
    }
    Ok(())
}

fn locator_matches(task: &SourceTask, ack: &SourceAck) -> bool {
    task.source_path == ack.path && task.line == ack.line && task.id == ack.mw_id
}

pub(crate) fn observation(snapshot: &CommittedSnapshot, task: &SourceTask) -> Result<SourceAck> {
    let blob = snapshot
        .files
        .get(&task.source_path)
        .ok_or_else(|| anyhow!("source blob missing from committed snapshot"))?;
    let ack = SourceAck {
        commit: snapshot.commit.clone(),
        parser_sha256: snapshot.parser_sha256.clone(),
        path: task.source_path.clone(),
        source_id: task.source_id.clone(),
        line: task.line,
        mw_id: task.id.clone(),
        git_blob_id: blob.object_id.clone(),
        source_revision: task.source_revision.clone(),
        stable_id: task.stable_id.clone(),
        expected: ExpectedTask {
            text: task.text.clone(),
            literal_text: task.literal_text.clone(),
            done: task.done,
            due: task.metadata.due.clone(),
            raw: task.metadata.raw.clone(),
            start: task.metadata.start.clone(),
            estimate: task.metadata.estimate.clone(),
        },
    };
    ledger::validate_source(&ack, ack.commit.len())?;
    Ok(ack)
}

pub(crate) fn build_plan(
    target: &CommittedSnapshot,
    binding: &Binding,
    ledger: Option<&Ledger>,
    basis: Option<LedgerBasis>,
) -> Result<Plan> {
    let preview = projection::project(target)?;
    let tasks: BTreeMap<_, _> = target
        .items
        .iter()
        .map(|task| ((task.source_path.clone(), task.line, task.id.clone()), task))
        .collect();
    ensure!(
        tasks.len() == target.items.len(),
        "ambiguous source task locator"
    );
    let recognize = |ack: &SourceAck| -> Result<Option<SourceAck>> {
        let key = (ack.path.clone(), ack.line, ack.mw_id.clone());
        let Some(task) = tasks.get(&key) else {
            return Ok(None);
        };
        let current = observation(target, task)?;
        let mut comparable = current.clone();
        comparable.commit = ack.commit.clone();
        Ok((comparable == *ack).then_some(current))
    };
    let mut projected: BTreeMap<_, _> = preview
        .items
        .into_iter()
        .map(|item| {
            (
                (item.source.path.clone(), item.source.line, item.id.clone()),
                item,
            )
        })
        .collect();
    let mut claimed = BTreeSet::new();
    let mut entries = Vec::new();
    if let Some(ledger) = ledger {
        for entry in &ledger.entries {
            let mut result = EntryPlan {
                logical_id: entry.logical_id.clone(),
                decision: PlanDecision::Review,
                publication_ready: false,
                diagnostics: vec![],
                source_ack: entry.source_ack.clone(),
                current_source: None,
                event_id: None,
                base_etag: None,
                last_acknowledged_payload_hash: None,
                desired_payload_hash: None,
                prospective_event: None,
            };
            let ack = &entry.source_ack;
            let key = (ack.path.clone(), ack.line, ack.mw_id.clone());
            result.current_source = recognize(ack)?;
            if result.current_source.is_some() {
                claimed.insert(key.clone());
            }
            if let Some(pending) = &entry.pending
                && let Some(current) = recognize(&pending.intended_source)?
            {
                claimed.insert((current.path, current.line, current.mw_id));
            }
            if entry.lifecycle == Lifecycle::Retired {
                result.decision = PlanDecision::Retired;
                result.diagnostics.push("retired_no_action");
            } else if entry.pending.is_some() {
                result.decision = PlanDecision::Blocked;
                result.diagnostics.push("reconciliation_required");
            } else if ack.parser_sha256 != target.parser_sha256 {
                result.diagnostics.push("parser_provenance_changed");
            } else if result.current_source.is_none() {
                result.diagnostics.push(if !tasks.contains_key(&key) {
                    "source_missing_or_moved"
                } else {
                    "source_basis_changed"
                });
            } else if entry.lifecycle == Lifecycle::Enrolled {
                result
                    .diagnostics
                    .push("initial_publication_not_implemented");
            } else {
                let item = projected.remove(&key);
                if let Some(event) = item.and_then(|item| item.prospective_event) {
                    let hash = ledger::payload_hash(&event);
                    let provider = entry
                        .provider_ack
                        .as_ref()
                        .ok_or_else(|| anyhow!("active ledger entry lacks provider evidence"))?;
                    result.event_id = Some(provider.event_id.clone());
                    result.last_acknowledged_payload_hash =
                        Some(provider.last_payload_hash.clone());
                    if hash == provider.last_payload_hash {
                        result.decision = PlanDecision::Noop;
                        result.diagnostics.push("payload_matches_ledger_not_remote");
                    } else {
                        result.decision = PlanDecision::Update;
                        result
                            .diagnostics
                            .push("advisory_update_executor_not_implemented");
                        result.base_etag = Some(provider.etag.clone());
                        result.desired_payload_hash = Some(hash);
                        result.prospective_event = Some(event);
                    }
                } else {
                    result.diagnostics.push("source_not_open_with_valid_due");
                }
            }
            entries.push(result);
        }
    }
    entries.sort_by(|a, b| a.logical_id.cmp(&b.logical_id));
    let mut unenrolled_observations = Vec::new();
    // Projection order and the map key are deterministic; no ID is assigned here.
    for (key, item) in projected {
        if claimed.contains(&key) {
            continue;
        }
        let task = tasks
            .get(&key)
            .ok_or_else(|| anyhow!("source projection locator mismatch"))?;
        let mut diagnostics = vec!["enrollment_required"];
        diagnostics.extend(item.diagnostics.into_iter().filter(|diagnostic| {
            matches!(
                *diagnostic,
                "duplicate_due" | "date_invalid" | "parser_due_mismatch" | "end_date_overflow"
            )
        }));
        let desired_payload_hash = item.prospective_event.as_ref().map(ledger::payload_hash);
        unenrolled_observations.push(UnenrolledObservation {
            source_ack: observation(target, task)?,
            publication_ready: false,
            diagnostics,
            due: item.due,
            desired_payload_hash,
            prospective_event: item.prospective_event,
        });
    }
    let mut counts = PlanCounts {
        entries: entries.len(),
        unenrolled_observations: unenrolled_observations.len(),
        ..Default::default()
    };
    for entry in &entries {
        match entry.decision {
            PlanDecision::Noop => counts.noop += 1,
            PlanDecision::Update => counts.update += 1,
            PlanDecision::Review => counts.review += 1,
            PlanDecision::Blocked => counts.blocked += 1,
            PlanDecision::Retired => counts.retired += 1,
        }
    }
    Ok(Plan {
        schema_version: "1",
        kind: "calendar-publication-plan",
        mode: if ledger.is_some() {
            "ledger"
        } else {
            "bootstrap-preview"
        },
        commit: target.commit.clone(),
        parser_sha256: target.parser_sha256.clone(),
        binding: binding.clone(),
        ledger: basis,
        publication_ready: false,
        remote_verified: false,
        ledger_head_verified: false,
        requires_ledger_compare_and_swap: true,
        diagnostics: vec![
            "source_only_advisory",
            "remote_not_verified",
            "ledger_head_not_verified",
            "executor_not_implemented",
        ],
        counts,
        entries,
        unenrolled_observations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LedgerEntry, OperationKind, OperationPhase, PendingOperation, ProviderAck,
        RemoteDisposition, SourceBlob, TaskMetadata, Tombstone,
    };
    use sha2::{Digest, Sha256};

    fn binding() -> Binding {
        Binding {
            provider: "google_calendar".into(),
            account_id: format!("google-sub-v1:{}", "a".repeat(64)),
            calendar_id: "calendar@example.invalid".into(),
            source_repo_id: "fixture-repo".into(),
        }
    }

    fn snapshot(commit: char) -> CommittedSnapshot {
        let content = b"- [ ] Ship due:2026-11-01\n".to_vec();
        CommittedSnapshot {
            commit: commit.to_string().repeat(40),
            parser_sha256: "a".repeat(64),
            files: BTreeMap::from([(
                "tasks.md".into(),
                SourceBlob {
                    object_id: "b".repeat(40),
                    content: content.clone(),
                },
            )]),
            items: vec![SourceTask {
                id: "tasks:1:0".into(),
                stable_id: None,
                identity_kind: "positional".into(),
                source_revision: format!("sha256:{:x}", Sha256::digest(&content)),
                source_id: "tasks".into(),
                source_path: "tasks.md".into(),
                line: 1,
                text: "Ship due:2026-11-01".into(),
                literal_text: "- [ ] Ship due:2026-11-01".into(),
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

    fn ledger(snapshot: &CommittedSnapshot) -> Ledger {
        let event = projection::project(snapshot)
            .unwrap()
            .items
            .remove(0)
            .prospective_event
            .unwrap();
        Ledger {
            schema_version: "1".into(),
            binding: binding(),
            source_watermark: snapshot.commit.clone(),
            entries: vec![LedgerEntry {
                logical_id: "11111111-1111-4111-8111-111111111111".into(),
                lifecycle: Lifecycle::Active,
                source_ack: observation(snapshot, &snapshot.items[0]).unwrap(),
                provider_ack: Some(ProviderAck {
                    event_id: "event123".into(),
                    etag: "old-etag".into(),
                    last_payload_hash: ledger::payload_hash(&event),
                }),
                pending: None,
                tombstone: None,
            }],
        }
    }

    fn decoded(ledger: &Ledger) -> Ledger {
        ledger::decode(&serde_json::to_vec(ledger).unwrap(), &binding()).unwrap()
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(120)
    }

    fn planned(target: &CommittedSnapshot, ledger: &Ledger) -> Plan {
        let ledger = decoded(ledger);
        validate_history(
            target,
            &ledger,
            deadline(),
            |commit| {
                let mut old = snapshot('1');
                old.commit = commit.into();
                Ok(old)
            },
            |_, _| Ok(()),
        )
        .unwrap();
        build_plan(
            target,
            &binding(),
            Some(&ledger),
            Some(LedgerBasis {
                commit: "f".repeat(40),
                path: "calendar/ledger.json".into(),
                git_blob_id: "e".repeat(40),
            }),
        )
        .unwrap()
    }

    #[test]
    fn markerless_exact_and_descendant_are_ledger_relative_noops() {
        let old = snapshot('1');
        let ledger = ledger(&old);
        for target in [snapshot('1'), snapshot('2')] {
            let plan = planned(&target, &ledger);
            assert_eq!(plan.entries[0].decision, PlanDecision::Noop);
            assert_eq!(
                plan.entries[0].current_source.as_ref().unwrap().commit,
                target.commit
            );
            assert!(plan.unenrolled_observations.is_empty());
            assert!(!plan.publication_ready && !plan.remote_verified && !plan.ledger_head_verified);
            assert!(plan.requires_ledger_compare_and_swap);
            assert_eq!(plan.counts.noop, 1);
            let json = serde_json::to_value(&plan).unwrap();
            assert_eq!(json["ledger"]["commit"], "f".repeat(40));
            assert_eq!(json["ledger"]["path"], "calendar/ledger.json");
            assert_eq!(json["ledger"]["gitBlobID"], "e".repeat(40));
            assert_eq!(json["mode"], "ledger");
            assert_eq!(json["entries"][0]["publicationReady"], false);
            assert_eq!(
                json["entries"][0]["lastAcknowledgedPayloadHash"],
                ledger.entries[0]
                    .provider_ack
                    .as_ref()
                    .unwrap()
                    .last_payload_hash
            );
            assert!(
                plan.entries[0]
                    .diagnostics
                    .contains(&"payload_matches_ledger_not_remote")
            );
        }
    }

    #[test]
    fn explicit_new_ack_allows_update_with_same_event_and_old_etag() {
        let mut target = snapshot('2');
        let mut ledger = ledger(&target);
        target.items[0].text = "Explicitly acknowledged new title".into();
        target.items[0].literal_text =
            "- [ ] Explicitly acknowledged new title due:2026-11-01".into();
        ledger.entries[0].source_ack = observation(&target, &target.items[0]).unwrap();
        let plan = planned(&target, &ledger);
        let entry = &plan.entries[0];
        assert_eq!(entry.decision, PlanDecision::Update);
        assert_eq!(entry.event_id.as_deref(), Some("event123"));
        assert_eq!(entry.base_etag.as_deref(), Some("old-etag"));
        let provider = ledger.entries[0].provider_ack.as_ref().unwrap();
        assert_eq!(
            entry.last_acknowledged_payload_hash.as_deref(),
            Some(provider.last_payload_hash.as_str())
        );
        assert_ne!(
            entry.last_acknowledged_payload_hash,
            entry.desired_payload_hash
        );
        assert_eq!(
            entry.desired_payload_hash.as_deref(),
            Some(ledger::payload_hash(entry.prospective_event.as_ref().unwrap()).as_str())
        );
        assert!(!entry.publication_ready);
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            json["entries"][0]["lastAcknowledgedPayloadHash"],
            provider.last_payload_hash
        );
        assert_eq!(
            json["entries"][0]["desiredPayloadHash"],
            entry.desired_payload_hash.as_deref().unwrap()
        );
        assert_eq!(json["entries"][0]["eventId"], provider.event_id);
        assert_eq!(json["entries"][0]["baseEtag"], provider.etag);
        assert_eq!(json["entries"][0]["publicationReady"], false);
        assert_eq!(json["remoteVerified"], false);
        assert!(plan.unenrolled_observations.is_empty());
    }

    #[test]
    fn source_changes_are_reviews_not_identity_relinks_or_creates() {
        let original = snapshot('1');
        let ledger = ledger(&original);
        for change in 0..8 {
            let mut target = snapshot('2');
            match change {
                0 => target.files.get_mut("tasks.md").unwrap().object_id = "c".repeat(40),
                1 => target.items[0].source_revision = format!("sha256:{}", "c".repeat(64)),
                2 => target.items[0].text = "Edited title".into(),
                3 => target.items[0].done = true,
                4 => {
                    target.items[0].metadata.due.clear();
                    target.items[0].literal_text = "- [ ] Ship".into();
                }
                5 => target.items[0].metadata.estimate = "2h".into(),
                6 => target.items[0].metadata.raw = "other metadata".into(),
                _ => {
                    target.items[0].metadata.start = "2026-10-31".into();
                }
            }
            let plan = planned(&target, &ledger);
            assert_eq!(plan.entries[0].decision, PlanDecision::Review, "{change}");
            assert_eq!(plan.entries[0].diagnostics, ["source_basis_changed"]);
            assert!(plan.entries[0].current_source.is_none());
            assert!(plan.entries[0].event_id.is_none());
            assert!(plan.entries[0].prospective_event.is_none());
            assert_eq!(plan.counts.update, 0);
        }
        for change in 0..3 {
            let mut target = snapshot('2');
            match change {
                0 => target.items.clear(),
                1 => {
                    target.items[0].source_path = "moved.md".into();
                    let blob = target.files.remove("tasks.md").unwrap();
                    target.files.insert("moved.md".into(), blob);
                }
                _ => {
                    target.items[0].line = 2;
                    target.items[0].id = "tasks:2:0".into();
                }
            }
            let plan = planned(&target, &ledger);
            assert_eq!(plan.entries[0].diagnostics, ["source_missing_or_moved"]);
            assert_eq!(plan.entries[0].decision, PlanDecision::Review);
        }
    }

    #[test]
    fn full_snapshot_recognizes_acknowledged_completed_undated_and_invalid_due() {
        for state in 0..3 {
            let mut target = snapshot('2');
            let mut ledger = ledger(&target);
            match state {
                0 => target.items[0].done = true,
                1 => {
                    target.items[0].metadata.due.clear();
                    target.items[0].literal_text = "- [ ] Ship".into();
                }
                _ => {
                    target.items[0].metadata.due.clear();
                    target.items[0].literal_text = "- [ ] Ship due:bad".into();
                }
            }
            ledger.entries[0].source_ack = observation(&target, &target.items[0]).unwrap();
            let plan = planned(&target, &ledger);
            assert_eq!(plan.entries[0].decision, PlanDecision::Review);
            assert_eq!(
                plan.entries[0].diagnostics,
                ["source_not_open_with_valid_due"]
            );
            assert!(plan.entries[0].current_source.is_some());
            assert!(plan.unenrolled_observations.is_empty());
        }
    }

    #[test]
    fn every_pending_kind_and_phase_blocks_and_retired_never_resurrects() {
        let target = snapshot('1');
        for kind in [
            OperationKind::Create,
            OperationKind::Update,
            OperationKind::Delete,
        ] {
            for phase in [OperationPhase::Prepared, OperationPhase::Uncertain] {
                let mut ledger = ledger(&target);
                let entry = &mut ledger.entries[0];
                if kind == OperationKind::Create {
                    entry.lifecycle = Lifecycle::Enrolled;
                    entry.provider_ack = None;
                }
                entry.pending = Some(PendingOperation {
                    operation_id: "22222222-2222-4222-8222-222222222222".into(),
                    kind,
                    phase,
                    intended_source: entry.source_ack.clone(),
                    target_event_id: "event123".into(),
                    base_etag: (kind != OperationKind::Create).then(|| "old-etag".into()),
                    desired_payload_hash: (kind != OperationKind::Delete)
                        .then(|| format!("sha256:{}", "a".repeat(64))),
                });
                let plan = planned(&target, &ledger);
                assert_eq!(plan.entries[0].decision, PlanDecision::Blocked);
                assert_eq!(plan.entries[0].diagnostics, ["reconciliation_required"]);
                assert!(plan.entries[0].prospective_event.is_none());
            }
        }
        let mut ledger = ledger(&target);
        ledger.entries[0].lifecycle = Lifecycle::Retired;
        ledger.entries[0].tombstone = Some(Tombstone {
            reason: "explicit retirement".into(),
            source_commit: target.commit.clone(),
            operation_id: None,
            remote_disposition: RemoteDisposition::Retained,
        });
        let plan = planned(&target, &ledger);
        assert_eq!(plan.entries[0].decision, PlanDecision::Retired);
        assert!(plan.entries[0].event_id.is_none());
        assert!(plan.unenrolled_observations.is_empty());
        ledger.entries[0].lifecycle = Lifecycle::Enrolled;
        ledger.entries[0].tombstone = None;
        ledger.entries[0].provider_ack = None;
        let plan = planned(&target, &ledger);
        assert_eq!(plan.entries[0].decision, PlanDecision::Review);
        assert_eq!(
            plan.entries[0].diagnostics,
            ["initial_publication_not_implemented"]
        );
    }

    #[test]
    fn historical_proofs_are_validated_once_per_commit_and_ancestry_pair() {
        let old = snapshot('1');
        let target = snapshot('2');
        let mut ledger = ledger(&old);
        ledger.entries[0].pending = Some(PendingOperation {
            operation_id: "22222222-2222-4222-8222-222222222222".into(),
            kind: OperationKind::Update,
            phase: OperationPhase::Prepared,
            intended_source: ledger.entries[0].source_ack.clone(),
            target_event_id: "event123".into(),
            base_etag: Some("old-etag".into()),
            desired_payload_hash: Some(format!("sha256:{}", "a".repeat(64))),
        });
        let mut reads = Vec::new();
        let mut pairs = BTreeSet::new();
        validate_history(
            &target,
            &ledger,
            deadline(),
            |commit| {
                reads.push(commit.to_owned());
                Ok(snapshot('1'))
            },
            |ancestor, descendant| {
                assert!(pairs.insert((ancestor.to_owned(), descendant.to_owned())));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(reads.as_slice(), std::slice::from_ref(&old.commit));
        assert_eq!(pairs.len(), 2);
        validate_history(
            &old,
            &ledger,
            deadline(),
            |_| panic!("target must be reused"),
            |_, _| Ok(()),
        )
        .unwrap();
        for field in 0..6 {
            let mut forged = ledger.clone();
            let ack = &mut forged.entries[0].source_ack;
            match field {
                0 => ack.expected.text = "forged private text".into(),
                1 => ack.git_blob_id = "e".repeat(40),
                2 => ack.line = 9,
                3 => ack.source_revision = format!("sha256:{}", "e".repeat(64)),
                4 => ack.expected.raw = "secret".into(),
                _ => ack.stable_id = Some("f".repeat(32)),
            }
            let error = validate_history(
                &target,
                &forged,
                deadline(),
                |_| Ok(snapshot('1')),
                |_, _| Ok(()),
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "ledger source evidence mismatch");
        }
        ledger.entries[0]
            .pending
            .as_mut()
            .unwrap()
            .intended_source
            .expected
            .done = true;
        assert!(
            validate_history(
                &target,
                &ledger,
                deadline(),
                |_| Ok(snapshot('1')),
                |_, _| Ok(())
            )
            .is_err()
        );
    }

    #[test]
    fn exact_pending_intended_source_is_not_offered_for_duplicate_enrollment() {
        let mut ledger = ledger(&snapshot('1'));
        let mut target = snapshot('2');
        target.items[0].text = "Acknowledged pending title".into();
        target.items[0].literal_text = "- [ ] Acknowledged pending title due:2026-11-01".into();
        target.files.get_mut("tasks.md").unwrap().object_id = "c".repeat(40);
        ledger.entries[0].pending = Some(PendingOperation {
            operation_id: "22222222-2222-4222-8222-222222222222".into(),
            kind: OperationKind::Update,
            phase: OperationPhase::Uncertain,
            intended_source: observation(&target, &target.items[0]).unwrap(),
            target_event_id: "event123".into(),
            base_etag: Some("old-etag".into()),
            desired_payload_hash: Some(format!("sha256:{}", "a".repeat(64))),
        });
        let plan = planned(&target, &ledger);
        assert_eq!(plan.entries[0].decision, PlanDecision::Blocked);
        assert!(plan.entries[0].current_source.is_none());
        assert!(plan.unenrolled_observations.is_empty());
    }

    #[test]
    fn rollback_divergence_missing_history_and_uncovered_retirement_fail_closed() {
        let target = snapshot('2');
        let old = snapshot('1');
        for failure in 0..4 {
            let mut ledger = ledger(&old);
            if failure == 1 {
                ledger.entries[0].source_ack.commit = "3".repeat(40);
            }
            if failure == 2 {
                ledger.entries[0].tombstone = Some(Tombstone {
                    reason: "retired".into(),
                    source_commit: "3".repeat(40),
                    operation_id: None,
                    remote_disposition: RemoteDisposition::Retained,
                });
            }
            if failure == 3 {
                ledger.entries[0].pending = Some(PendingOperation {
                    operation_id: String::new(),
                    kind: OperationKind::Update,
                    phase: OperationPhase::Uncertain,
                    intended_source: observation(&snapshot('3'), &snapshot('3').items[0]).unwrap(),
                    target_event_id: String::new(),
                    base_etag: None,
                    desired_payload_hash: None,
                });
            }
            let error = validate_history(
                &target,
                &ledger,
                deadline(),
                |_| Ok(snapshot('1')),
                |ancestor, descendant| {
                    if (failure == 0 && ancestor == old.commit && descendant == target.commit)
                        || ancestor == "3".repeat(40)
                    {
                        Err(anyhow!("private Git error with credentials"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "source history unavailable or not ancestral"
            );
        }
        let ledger = ledger(&old);
        assert_eq!(
            validate_history(
                &target,
                &ledger,
                deadline(),
                |_| Err(anyhow!("/private/temporary/path")),
                |_, _| Ok(())
            )
            .unwrap_err()
            .to_string(),
            "source history snapshot unavailable"
        );
        assert!(
            validate_history(
                &target,
                &ledger,
                Instant::now(),
                |_| panic!("expired"),
                |_, _| panic!("expired")
            )
            .is_err()
        );
    }

    #[test]
    fn parser_changes_review_old_semantics_and_concurrent_changes_refuse() {
        let target = snapshot('2');
        let mut ledger = ledger(&snapshot('1'));
        ledger.entries[0].source_ack.parser_sha256 = "f".repeat(64);
        validate_history(
            &target,
            &ledger,
            deadline(),
            |_| panic!("cannot prove old semantics"),
            |_, _| Ok(()),
        )
        .unwrap();
        let plan = build_plan(&target, &binding(), Some(&ledger), None).unwrap();
        assert_eq!(plan.entries[0].decision, PlanDecision::Review);
        assert_eq!(plan.entries[0].diagnostics, ["parser_provenance_changed"]);
        assert!(plan.entries[0].current_source.is_none());
        ledger.entries[0].source_ack.parser_sha256 = target.parser_sha256.clone();
        let error = validate_history(
            &target,
            &ledger,
            deadline(),
            |_| {
                let mut historical = snapshot('1');
                historical.parser_sha256 = "f".repeat(64);
                Ok(historical)
            },
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "source parser changed during planning");
    }

    #[test]
    fn bootstrap_crlf_observation_preserves_literal_bytes_and_round_trips_ledger() {
        let mut target = snapshot('1');
        target.items[0].literal_text.push('\r');
        let content = format!("{}\n", target.items[0].literal_text).into_bytes();
        target.items[0].source_revision = format!("sha256:{:x}", Sha256::digest(&content));
        target.files.get_mut("tasks.md").unwrap().content = content;
        let plan = build_plan(&target, &binding(), None, None).unwrap();
        let ack = &plan.unenrolled_observations[0].source_ack;
        assert_eq!(
            ack.expected.literal_text.as_bytes(),
            b"- [ ] Ship due:2026-11-01\r"
        );
        assert_eq!(ack.expected.literal_text, target.items[0].literal_text);
        let mut enrolled = ledger(&target);
        enrolled.entries[0].lifecycle = Lifecycle::Enrolled;
        enrolled.entries[0].provider_ack = None;
        enrolled.entries[0].source_ack = ack.clone();
        assert_eq!(decoded(&enrolled).entries[0].source_ack, *ack);
        assert!(!plan.publication_ready);
    }

    #[test]
    fn bootstrap_refuses_oversized_observations_without_truncating_or_normalizing() {
        for field in ["line", "text", "literal", "raw", "due", "start", "estimate"] {
            let mut target = snapshot('1');
            let task = &mut target.items[0];
            match field {
                "line" => {
                    task.line = 1_000_001;
                    task.id = "tasks:1000001:0".into();
                }
                "text" => task.text = "private-content".repeat(5000),
                "literal" => task.literal_text.push_str(&"private-content".repeat(5000)),
                "raw" => task.metadata.raw = "private-content".repeat(5000),
                "due" => task.metadata.due = "private-content".repeat(100),
                "start" => task.metadata.start = "private-content".repeat(100),
                _ => task.metadata.estimate = "private-content".repeat(100),
            }
            let before = target.items.clone();
            let error = build_plan(&target, &binding(), None, None)
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                if field == "line" {
                    "invalid ledger source line"
                } else {
                    "ledger expected task limit exceeded"
                },
                "{field}"
            );
            assert_eq!(target.items, before, "{field}");
        }
    }

    #[test]
    fn bootstrap_and_unenrolled_are_explicit_observations_without_assigned_ids() {
        let target = snapshot('1');
        let bootstrap = build_plan(&target, &binding(), None, None).unwrap();
        assert_eq!(bootstrap.mode, "bootstrap-preview");
        assert!(bootstrap.entries.is_empty() && bootstrap.ledger.is_none());
        let observation = &bootstrap.unenrolled_observations[0];
        assert_eq!(observation.source_ack.stable_id, None);
        assert_eq!(observation.diagnostics, ["enrollment_required"]);
        assert!(observation.prospective_event.is_some());
        assert!(!observation.publication_ready);
        let mut ledger = ledger(&target);
        ledger.entries.clear();
        let plan = planned(&target, &ledger);
        assert_eq!(plan.mode, "ledger");
        assert_eq!(plan.unenrolled_observations.len(), 1);
        let json = serde_json::to_string(&plan).unwrap();
        for forbidden in [
            "ledger_not_configured",
            "logicalId",
            "operationId",
            "taskMarker",
            "create",
        ] {
            assert!(!json.contains(forbidden), "{forbidden}");
        }
        let mut options = PlanOptions {
            source: crate::PreviewOptions {
                repository: "/unavailable".into(),
                commit: target.commit,
                mw_binary: "/unavailable/mw".into(),
            },
            binding: binding(),
            ledger: LedgerInput::BootstrapPreview,
        };
        options.binding.provider = "invalid".into();
        assert_eq!(
            super::plan(&options).unwrap_err().to_string(),
            "invalid ledger provider"
        );
    }

    #[test]
    fn deterministic_entry_and_observation_order_and_ambiguous_locator_refusal() {
        let mut target = snapshot('1');
        let mut another = target.items[0].clone();
        another.source_path = "aaa.md".into();
        another.source_id = "aaa".into();
        another.id = "aaa:1:0".into();
        target.files.insert(
            "aaa.md".into(),
            SourceBlob {
                object_id: "c".repeat(40),
                content: vec![],
            },
        );
        target.items.push(another);
        let before =
            serde_json::to_string(&build_plan(&target, &binding(), None, None).unwrap()).unwrap();
        target.items.reverse();
        assert_eq!(
            before,
            serde_json::to_string(&build_plan(&target, &binding(), None, None).unwrap()).unwrap()
        );
        let mut ledger = ledger(&target);
        let mut second = ledger.entries[0].clone();
        second.logical_id = "00000000-0000-4000-8000-000000000001".into();
        second.source_ack = observation(&target, &target.items[1]).unwrap();
        ledger.entries.push(second);
        let plan = build_plan(&target, &binding(), Some(&ledger), None).unwrap();
        assert!(plan.entries[0].logical_id < plan.entries[1].logical_id);
        target.items.push(target.items[0].clone());
        assert!(build_plan(&target, &binding(), None, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn orchestrator_reads_only_explicit_commits_and_never_bootstraps_missing_ledger() {
        use std::{fs, os::unix::fs::PermissionsExt, process::Command};

        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| -> String {
            let output = Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args([
                    "-c",
                    "user.name=Planner Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-C",
                ])
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "isolated fixture Git failed");
            String::from_utf8(output.stdout).unwrap().trim().into()
        };
        let commit = || {
            git(&["add", "--all"]);
            git(&["commit", "--quiet", "-m", "isolated planner fixture"]);
            git(&["rev-parse", "HEAD"])
        };
        git(&["init", "--quiet"]);
        let fixture = snapshot('1');
        fs::write(repo.join("tasks.md"), &fixture.files["tasks.md"].content).unwrap();
        let old = commit();
        fs::write(repo.join("unrelated.txt"), "committed descendant").unwrap();
        let descendant = commit();
        let mw = root.join("mw");
        let task = &fixture.items[0];
        let task_json = serde_json::json!({
            "id": task.id, "stableId": null, "identityKind": "positional",
            "sourceRevision": task.source_revision, "sourceId": task.source_id,
            "sourcePath": task.source_path, "line": task.line, "text": task.text,
            "done": false, "metadata": {"due": task.metadata.due, "raw": "", "start": "", "estimate": ""}
        });
        fs::write(&mw, format!(
            "#!/bin/sh\nset -eu\n[ \"$#\" = 3 ] && [ \"$1\" = todos ] && [ \"$2\" = snapshot ] && [ \"$3\" = --json ]\nprintf 'read\\n' >> '{}/parser-reads'\nprintf '{{\"schemaVersion\":\"1\",\"source\":\"saved-markdown\",\"notesDir\":\"%s\",\"dashboardPath\":\"%s\",\"items\":[{task_json}]}}' \"$NOTES_DIR\" \"$DASHBOARD_PATH\"\n", root.display()
        )).unwrap();
        fs::set_permissions(&mw, fs::Permissions::from_mode(0o700)).unwrap();
        let old_snapshot = source::read_snapshot_before(&repo, &old, &mw, deadline()).unwrap();
        let mut ledger = ledger(&old_snapshot);
        fs::write(
            repo.join("ledger.json"),
            serde_json::to_vec(&ledger).unwrap(),
        )
        .unwrap();
        let ledger_commit = commit();
        let mut options = PlanOptions {
            source: crate::PreviewOptions {
                repository: repo.clone(),
                commit: old.clone(),
                mw_binary: mw,
            },
            binding: binding(),
            ledger: LedgerInput::Committed {
                repository: repo.clone(),
                commit: ledger_commit.clone(),
                path: "ledger.json".into(),
            },
        };
        // Ambient tracked and untracked edits are deliberately not source inputs.
        fs::write(repo.join("tasks.md"), "uncommitted private content").unwrap();
        fs::write(repo.join("untracked.md"), "untracked private content").unwrap();
        let location = LedgerLocation {
            repository: repo.clone(),
            commit: ledger_commit.clone(),
            path: "ledger.json".into(),
        };
        fs::write(root.join("parser-reads"), "").unwrap();
        let context = load_context(
            &options.source,
            &options.binding,
            Some(&location),
            deadline(),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("parser-reads")).unwrap(),
            "read\n"
        );
        assert_eq!(context.target.commit, old);
        assert_eq!(context.ledger.as_ref().unwrap().source_watermark, old);
        assert_eq!(context.basis.as_ref().unwrap().commit, ledger_commit);
        let reused = build_plan(
            &context.target,
            &options.binding,
            context.ledger.as_ref(),
            context.basis.clone(),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("parser-reads")).unwrap(),
            "read\n"
        );
        assert_eq!(
            serde_json::to_value(reused).unwrap(),
            serde_json::to_value(super::plan(&options).unwrap()).unwrap()
        );

        fs::write(root.join("parser-reads"), "").unwrap();
        let observation_only =
            load_context(&options.source, &options.binding, None, deadline()).unwrap();
        assert!(observation_only.ledger.is_none() && observation_only.basis.is_none());
        assert_eq!(observation_only.target.commit, old);
        assert_eq!(
            fs::read_to_string(root.join("parser-reads")).unwrap(),
            "read\n"
        );
        let invalid_binding = Binding {
            provider: "invalid".into(),
            ..binding()
        };
        assert_eq!(
            load_context(
                &options.source,
                &invalid_binding,
                Some(&location),
                deadline()
            )
            .err()
            .unwrap()
            .to_string(),
            "invalid ledger provider"
        );
        assert_eq!(
            fs::read_to_string(root.join("parser-reads")).unwrap(),
            "read\n"
        );
        for selected in [&old, &descendant] {
            options.source.commit = selected.clone();
            let result = super::plan(&options).unwrap();
            assert_eq!(result.entries[0].decision, PlanDecision::Noop);
            assert_eq!(result.commit, *selected);
            assert_eq!(result.ledger.unwrap().commit, ledger_commit);
        }
        assert_eq!(
            fs::read_to_string(repo.join("tasks.md")).unwrap(),
            "uncommitted private content"
        );
        if let LedgerInput::Committed { path, .. } = &mut options.ledger {
            *path = "missing.json".into();
        }
        assert!(super::plan(&options).is_err());
        options.ledger = LedgerInput::BootstrapPreview;
        assert_eq!(
            super::plan(&options).unwrap().unenrolled_observations.len(),
            1
        );

        // Forged historical evidence must refuse the whole plan, not merely review it.
        ledger.entries[0].source_ack.expected.text = "forged private content".into();
        fs::write(
            repo.join("ledger.json"),
            serde_json::to_vec(&ledger).unwrap(),
        )
        .unwrap();
        options.ledger = LedgerInput::Committed {
            repository: repo.clone(),
            commit: commit(),
            path: "ledger.json".into(),
        };
        assert_eq!(
            super::plan(&options).unwrap_err().to_string(),
            "ledger source evidence mismatch"
        );

        ledger.entries[0].source_ack = observation(&old_snapshot, &old_snapshot.items[0]).unwrap();
        ledger.source_watermark = descendant;
        fs::write(
            repo.join("ledger.json"),
            serde_json::to_vec(&ledger).unwrap(),
        )
        .unwrap();
        options.ledger = LedgerInput::Committed {
            repository: repo.clone(),
            commit: commit(),
            path: "ledger.json".into(),
        };
        options.source.commit = old;
        assert_eq!(
            super::plan(&options).unwrap_err().to_string(),
            "source history unavailable or not ancestral"
        );
    }
}
