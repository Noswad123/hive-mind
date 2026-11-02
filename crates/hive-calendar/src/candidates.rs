//! Candidate-only mutations. Nothing here writes source, publication state, or provider state.
//! Future persistence must compare-and-swap both the ledger HEAD and this exact path/blob
//! basis before provider operations. Absence at a selected commit is not HEAD absence.
//! Enrollment IDs may differ between invocations until a candidate is persisted.
//! Reack is an explicit human mapping, not a claim of semantic task identity.
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, ensure};
use serde::Serialize;

use crate::{
    Binding, CommittedSnapshot, EnrollmentOptions, EntrySelection, Ledger, LedgerEntry,
    LedgerLocation, Lifecycle, ReackOptions, SourceAck, SourceTask, TaskSelection, ledger,
    planner::{self, PlanningContext},
    projection, source,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateBase {
    pub commit: String,
    pub path: String,
    #[serde(rename = "gitBlobID")]
    pub git_blob_id: Option<String>,
    pub condition: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateOperation {
    Enroll,
    Reack,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerCandidate {
    pub schema_version: &'static str,
    pub kind: &'static str,
    pub operation: CandidateOperation,
    pub logical_id: String,
    pub source_commit: String,
    pub parser_sha256: String,
    pub base: CandidateBase,
    pub candidate_ledger: Ledger,
    pub publication_ready: bool,
    pub remote_verified: bool,
    pub ledger_head_verified: bool,
    pub requires_ledger_compare_and_swap: bool,
    pub diagnostics: Vec<&'static str>,
}

pub(crate) fn enroll(options: &EnrollmentOptions) -> Result<LedgerCandidate> {
    let deadline = Instant::now() + Duration::from_secs(120);
    validate_selection(&options.selection)?;
    ledger::validate_binding(&options.binding)?;
    let context = if options.initialize_ledger {
        source::require_json_absent_before(
            &options.ledger.repository,
            &options.ledger.commit,
            &options.ledger.path,
            deadline,
        )?;
        planner::load_context(&options.source, &options.binding, None, deadline)?
    } else {
        planner::load_context(
            &options.source,
            &options.binding,
            Some(&options.ledger),
            deadline,
        )?
    };
    let result = enroll_context(context, options, |bytes| {
        getrandom::fill(bytes).map_err(|_| anyhow!("candidate entropy unavailable"))
    })?;
    ensure!(
        Instant::now() < deadline,
        "calendar candidate deadline exceeded"
    );
    Ok(result)
}

pub(crate) fn reack(options: &ReackOptions) -> Result<LedgerCandidate> {
    let deadline = Instant::now() + Duration::from_secs(120);
    validate_selection(&options.selection)?;
    validate_entry_selection(&options.entry)?;
    let context = planner::load_context(
        &options.source,
        &options.binding,
        Some(&options.ledger),
        deadline,
    )?;
    let result = reack_context(context, options)?;
    ensure!(
        Instant::now() < deadline,
        "calendar candidate deadline exceeded"
    );
    Ok(result)
}

fn validate_selection(selection: &TaskSelection) -> Result<()> {
    ensure!(
        (1..=1_000_000).contains(&selection.line)
            && !selection.path.trim().is_empty()
            && selection.path.len() <= 4096
            && selection.path.to_ascii_lowercase().ends_with(".md")
            && !selection.path.contains('\\')
            && !selection.path.chars().any(char::is_control)
            && selection.path.split('/').count() <= 64
            && selection.path.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && !part.eq_ignore_ascii_case(".git")
            }),
        "invalid candidate task selection"
    );
    Ok(())
}

fn validate_entry_selection(selection: &EntrySelection) -> Result<()> {
    match selection {
        EntrySelection::AcknowledgedLocation { path, line } => validate_selection(&TaskSelection {
            path: path.clone(),
            line: *line,
        }),
        EntrySelection::LogicalId(id) => {
            ensure!(
                id.len() == 36
                    && id.bytes().enumerate().all(|(i, b)| {
                        if [8, 13, 18, 23].contains(&i) {
                            b == b'-'
                        } else {
                            b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
                        }
                    }),
                "invalid candidate entry selection"
            );
            Ok(())
        }
    }
}

fn selected<'a>(
    target: &'a CommittedSnapshot,
    selection: &TaskSelection,
) -> Result<&'a SourceTask> {
    validate_selection(selection)?;
    let mut matches = target
        .items
        .iter()
        .filter(|task| task.source_path == selection.path && task.line == selection.line);
    let task = matches
        .next()
        .ok_or_else(|| anyhow!("candidate task not found"))?;
    ensure!(
        matches.next().is_none(),
        "ambiguous candidate task selection"
    );
    Ok(task)
}

// Deliberately ignore MW ID and parser provenance: neither can unreserve the
// same physical source, even when a retired entry or pending intent holds it.
fn conflicts(left: &SourceAck, right: &SourceAck) -> bool {
    (left.path == right.path
        && left.git_blob_id == right.git_blob_id
        && left.source_revision == right.source_revision
        && left.line == right.line)
        || (left.stable_id.is_some() && left.stable_id == right.stable_id)
}

fn require_unreserved(ledger: &Ledger, ack: &SourceAck, except: Option<usize>) -> Result<()> {
    for (index, entry) in ledger.entries.iter().enumerate() {
        if except == Some(index) {
            continue;
        }
        ensure!(
            !std::iter::once(&entry.source_ack)
                .chain(
                    entry
                        .pending
                        .as_ref()
                        .map(|pending| &pending.intended_source)
                )
                .any(|reserved| conflicts(reserved, ack)),
            "candidate source already reserved"
        );
    }
    Ok(())
}

fn fresh_id(ledger: &Ledger, mut entropy: impl FnMut(&mut [u8]) -> Result<()>) -> Result<String> {
    for _ in 0..16 {
        let mut bytes = [0u8; 16];
        entropy(&mut bytes).map_err(|_| anyhow!("candidate entropy unavailable"))?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let mut id = String::with_capacity(36);
        for (index, byte) in bytes.iter().enumerate() {
            use std::fmt::Write;
            if [4, 6, 8, 10].contains(&index) {
                id.push('-');
            }
            write!(id, "{byte:02x}").expect("writing to a String cannot fail");
        }
        if ledger.entries.iter().all(|entry| entry.logical_id != id) {
            return Ok(id);
        }
    }
    Err(anyhow!("candidate logical identity allocation failed"))
}

// Contexts passed here already have their entire original history validated.
// Only an exact target observation and the watermark change; do not reparse history.
pub(crate) fn enroll_context(
    context: PlanningContext,
    options: &EnrollmentOptions,
    entropy: impl FnMut(&mut [u8]) -> Result<()>,
) -> Result<LedgerCandidate> {
    let PlanningContext {
        target,
        ledger,
        basis,
    } = context;
    ensure!(
        options.initialize_ledger == ledger.is_none() && ledger.is_some() == basis.is_some(),
        "candidate ledger context mismatch"
    );
    let task = selected(&target, &options.selection)?;
    let evidence = projection::due_evidence(task)?;
    ensure!(
        !task.done && evidence.event.is_some() && evidence.diagnostic.is_none(),
        "enrollment requires open task with valid due"
    );
    let ack = planner::observation(&target, task)?;
    let mut ledger = ledger.unwrap_or_else(|| Ledger {
        schema_version: "1".into(),
        binding: options.binding.clone(),
        source_watermark: target.commit.clone(),
        entries: vec![],
    });
    require_unreserved(&ledger, &ack, None)?;
    ensure!(
        ledger.entries.iter().all(|entry| entry.pending.is_none()),
        "candidate reconciliation required"
    );
    let plan = planner::build_plan(&target, &options.binding, Some(&ledger), None)?;
    // Safe prototype: even unrelated new enrollments must wait for an explicit
    // resolution workflow if any old (including retired) basis is unrecognized.
    ensure!(
        plan.entries
            .iter()
            .all(|entry| entry.current_source.is_some()),
        "enrollment blocked by unresolved ledger source basis"
    );
    let logical_id = fresh_id(&ledger, entropy)?;
    ledger.entries.push(LedgerEntry {
        logical_id: logical_id.clone(),
        lifecycle: Lifecycle::Enrolled,
        source_ack: ack,
        provider_ack: None,
        pending: None,
        tombstone: None,
    });
    ledger.source_watermark = target.commit.clone();
    finish(
        &target,
        &options.binding,
        &options.ledger,
        basis,
        ledger,
        logical_id,
        CandidateOperation::Enroll,
    )
}

pub(crate) fn reack_context(
    context: PlanningContext,
    options: &ReackOptions,
) -> Result<LedgerCandidate> {
    let PlanningContext {
        target,
        ledger,
        basis,
    } = context;
    let mut ledger = ledger.ok_or_else(|| anyhow!("committed candidate ledger required"))?;
    ensure!(basis.is_some(), "committed candidate ledger basis required");
    validate_entry_selection(&options.entry)?;
    let mut matches = ledger
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| match &options.entry {
            EntrySelection::LogicalId(id) => entry.logical_id == *id,
            EntrySelection::AcknowledgedLocation { path, line } => {
                entry.source_ack.path == *path && entry.source_ack.line == *line
            }
        });
    let (index, entry) = matches
        .next()
        .ok_or_else(|| anyhow!("candidate entry not found"))?;
    ensure!(
        matches.next().is_none(),
        "ambiguous candidate entry selection"
    );
    ensure!(
        entry.lifecycle != Lifecycle::Retired,
        "retired candidate entry cannot be reacknowledged"
    );
    ensure!(entry.pending.is_none(), "candidate reconciliation required");
    ensure!(
        entry.source_ack.parser_sha256 == target.parser_sha256 || options.accept_parser_change,
        "candidate parser change requires acceptance"
    );
    let task = selected(&target, &options.selection)?;
    ensure!(
        projection::due_evidence(task)?.diagnostic.is_none(),
        "candidate task has invalid due evidence"
    );
    let ack = planner::observation(&target, task)?;
    require_unreserved(&ledger, &ack, Some(index))?;
    let logical_id = ledger.entries[index].logical_id.clone();
    ledger.entries[index].source_ack = ack;
    ledger.source_watermark = target.commit.clone();
    finish(
        &target,
        &options.binding,
        &options.ledger,
        basis,
        ledger,
        logical_id,
        CandidateOperation::Reack,
    )
}

fn finish(
    target: &CommittedSnapshot,
    binding: &Binding,
    location: &LedgerLocation,
    basis: Option<planner::LedgerBasis>,
    ledger: Ledger,
    logical_id: String,
    operation: CandidateOperation,
) -> Result<LedgerCandidate> {
    let base = match basis {
        Some(basis) => CandidateBase {
            commit: basis.commit,
            path: basis.path,
            git_blob_id: Some(basis.git_blob_id),
            condition: "existing_blob_at_commit",
        },
        None => CandidateBase {
            commit: location.commit.clone(),
            path: location.path.clone(),
            git_blob_id: None,
            condition: "path_absent_at_commit",
        },
    };
    let bytes =
        serde_json::to_vec(&ledger).map_err(|_| anyhow!("candidate ledger encoding failed"))?;
    let candidate_ledger = ledger::decode(&bytes, binding)?;
    Ok(LedgerCandidate {
        schema_version: "1",
        kind: "calendar-ledger-candidate",
        operation,
        logical_id,
        source_commit: target.commit.clone(),
        parser_sha256: target.parser_sha256.clone(),
        base,
        candidate_ledger,
        publication_ready: false,
        remote_verified: false,
        ledger_head_verified: false,
        requires_ledger_compare_and_swap: true,
        diagnostics: vec![
            "candidate_only_not_persisted",
            "remote_not_verified",
            "ledger_head_not_verified",
            "ledger_compare_and_swap_required",
            "executor_not_implemented",
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        OperationKind, OperationPhase, PendingOperation, PreviewOptions, ProviderAck,
        RemoteDisposition, SourceBlob, TaskMetadata, Tombstone,
    };
    use std::collections::BTreeMap;

    fn binding() -> Binding {
        Binding {
            provider: "google_calendar".into(),
            account_id: format!("google-sub-v1:{}", "a".repeat(64)),
            calendar_id: "calendar@example.invalid".into(),
            source_repo_id: "candidate-fixture".into(),
        }
    }

    fn location() -> LedgerLocation {
        LedgerLocation {
            repository: "/unavailable/ledger-repo".into(),
            commit: "f".repeat(40),
            path: "state/calendar.json".into(),
        }
    }

    fn selection() -> TaskSelection {
        TaskSelection {
            path: "tasks.md".into(),
            line: 1,
        }
    }

    fn enrollment(initialize_ledger: bool) -> EnrollmentOptions {
        EnrollmentOptions {
            source: PreviewOptions {
                repository: "/unavailable/source-repo".into(),
                commit: "2".repeat(40),
                mw_binary: "/unavailable/mw".into(),
            },
            binding: binding(),
            ledger: location(),
            selection: selection(),
            initialize_ledger,
        }
    }

    fn reack_options() -> ReackOptions {
        let options = enrollment(false);
        ReackOptions {
            source: options.source,
            binding: options.binding,
            ledger: options.ledger,
            entry: EntrySelection::LogicalId("11111111-1111-4111-8111-111111111111".into()),
            selection: options.selection,
            accept_parser_change: false,
        }
    }

    fn target() -> CommittedSnapshot {
        let task = SourceTask {
            id: "tasks:1:0".into(),
            stable_id: None,
            identity_kind: "positional".into(),
            source_revision: format!("sha256:{}", "c".repeat(64)),
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
        };
        CommittedSnapshot {
            commit: "2".repeat(40),
            parser_sha256: "a".repeat(64),
            files: BTreeMap::from([(
                "tasks.md".into(),
                SourceBlob {
                    object_id: "b".repeat(40),
                    content: format!("{}\n", task.literal_text).into_bytes(),
                },
            )]),
            items: vec![task],
        }
    }

    fn empty() -> Ledger {
        Ledger {
            schema_version: "1".into(),
            binding: binding(),
            source_watermark: "1".repeat(40),
            entries: vec![],
        }
    }

    fn entry(target: &CommittedSnapshot, index: usize) -> LedgerEntry {
        let mut source_ack = planner::observation(target, &target.items[index]).unwrap();
        source_ack.commit = "1".repeat(40);
        LedgerEntry {
            logical_id: format!("11111111-1111-4111-8111-{:012x}", index + 1),
            lifecycle: Lifecycle::Active,
            source_ack,
            provider_ack: Some(ProviderAck {
                event_id: format!("event{index}"),
                etag: "old-etag".into(),
                last_payload_hash: format!("sha256:{}", "d".repeat(64)),
            }),
            pending: None,
            tombstone: None,
        }
    }

    fn context(target: CommittedSnapshot, ledger: Option<Ledger>) -> PlanningContext {
        let basis = ledger.as_ref().map(|_| planner::LedgerBasis {
            commit: location().commit,
            path: location().path,
            git_blob_id: "e".repeat(40),
        });
        PlanningContext {
            target,
            ledger,
            basis,
        }
    }

    fn existing(target: &CommittedSnapshot) -> Ledger {
        let mut ledger = empty();
        let mut item = entry(target, 0);
        item.logical_id = "11111111-1111-4111-8111-111111111111".into();
        ledger.entries.push(item);
        ledger
    }

    fn second(target: &mut CommittedSnapshot) {
        let mut task = target.items[0].clone();
        task.source_path = "other.md".into();
        task.source_id = "other".into();
        task.id = "other:1:0".into();
        target.files.insert(
            task.source_path.clone(),
            SourceBlob {
                object_id: "d".repeat(40),
                content: vec![],
            },
        );
        target.items.push(task);
    }

    fn retire(entry: &mut LedgerEntry) {
        entry.lifecycle = Lifecycle::Retired;
        entry.tombstone = Some(Tombstone {
            reason: "explicit retirement".into(),
            source_commit: "1".repeat(40),
            operation_id: None,
            remote_disposition: RemoteDisposition::Retained,
        });
    }

    fn pending(entry: &mut LedgerEntry, kind: OperationKind, phase: OperationPhase) {
        if kind == OperationKind::Create {
            entry.lifecycle = Lifecycle::Enrolled;
            entry.provider_ack = None;
        }
        entry.pending = Some(PendingOperation {
            operation_id: "22222222-2222-4222-8222-222222222222".into(),
            kind,
            phase,
            intended_source: entry.source_ack.clone(),
            target_event_id: entry
                .provider_ack
                .as_ref()
                .map(|ack| ack.event_id.clone())
                .unwrap_or_else(|| "event0".into()),
            base_etag: (kind != OperationKind::Create).then(|| "old-etag".into()),
            desired_payload_hash: (kind != OperationKind::Delete)
                .then(|| format!("sha256:{}", "e".repeat(64))),
        });
    }

    #[test]
    fn markerless_enrollment_uuid_v4_and_exact_absent_base_contract() {
        let snapshot = target();
        let before = snapshot.items.clone();
        let candidate = enroll_context(context(snapshot, None), &enrollment(true), |bytes| {
            bytes.fill(0xff);
            Ok(())
        })
        .unwrap();
        assert_eq!(candidate.logical_id, "ffffffff-ffff-4fff-bfff-ffffffffffff");
        let item = &candidate.candidate_ledger.entries[0];
        assert_eq!(item.lifecycle, Lifecycle::Enrolled);
        assert!(item.provider_ack.is_none() && item.pending.is_none() && item.tombstone.is_none());
        assert_eq!(item.source_ack.stable_id, None);
        assert_eq!(
            item.source_ack.expected.literal_text,
            before[0].literal_text
        );
        assert!(!item.source_ack.expected.literal_text.contains("<!--"));
        let value = serde_json::to_value(&candidate).unwrap();
        assert_eq!(value["schemaVersion"], "1");
        assert_eq!(value["kind"], "calendar-ledger-candidate");
        assert_eq!(value["operation"], "enroll");
        assert_eq!(value["sourceCommit"], "2".repeat(40));
        assert_eq!(value["base"]["commit"], "f".repeat(40));
        assert_eq!(value["base"]["path"], "state/calendar.json");
        assert!(value["base"]["gitBlobID"].is_null());
        assert_eq!(value["base"]["condition"], "path_absent_at_commit");
        assert!(
            !candidate.publication_ready
                && !candidate.remote_verified
                && !candidate.ledger_head_verified
        );
        assert!(candidate.requires_ledger_compare_and_swap);
        assert_eq!(
            ledger::decode(
                &serde_json::to_vec(&candidate.candidate_ledger).unwrap(),
                &binding()
            )
            .unwrap(),
            candidate.candidate_ledger
        );
    }

    #[test]
    fn entropy_failure_redacted_and_collisions_bounded_and_retried() {
        assert_eq!(
            fresh_id(&empty(), |_| Err(anyhow!("private entropy error")))
                .unwrap_err()
                .to_string(),
            "candidate entropy unavailable"
        );
        let mut ledger = existing(&target());
        ledger.entries[0].logical_id = "00000000-0000-4000-8000-000000000000".into();
        let mut calls = 0;
        assert_eq!(
            fresh_id(&ledger, |_| {
                calls += 1;
                Ok(())
            })
            .unwrap_err()
            .to_string(),
            "candidate logical identity allocation failed"
        );
        assert_eq!(calls, 16);
        calls = 0;
        let id = fresh_id(&ledger, |bytes| {
            calls += 1;
            if calls == 2 {
                bytes[15] = 1;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(id, "00000000-0000-4000-8000-000000000001");
    }

    #[test]
    fn enrollment_refuses_reserved_and_unresolved_including_retired_before_entropy() {
        for state in [0, 2, 3, 4, 5, 6, 7] {
            let mut snapshot = target();
            second(&mut snapshot);
            let mut ledger = existing(&snapshot);
            let mut options = enrollment(false);
            if state > 0 {
                options.selection.path = "other.md".into();
            }
            match state {
                0 => retire(&mut ledger.entries[0]),
                2 => ledger.entries[0].source_ack.parser_sha256 = "f".repeat(64),
                3 => snapshot.items[0].text = "Edited".into(),
                4 => {
                    snapshot.items.remove(0);
                }
                5 => pending(
                    &mut ledger.entries[0],
                    OperationKind::Update,
                    OperationPhase::Prepared,
                ),
                6 => {
                    retire(&mut ledger.entries[0]);
                    snapshot.items[0].line = 2;
                    snapshot.items[0].id = "tasks:2:0".into();
                }
                _ => {
                    retire(&mut ledger.entries[0]);
                    ledger.entries[0].source_ack.parser_sha256 = "f".repeat(64);
                }
            }
            let result = enroll_context(context(snapshot, Some(ledger)), &options, |_| {
                panic!("must refuse before allocating")
            });
            assert!(result.is_err(), "state {state}");
        }
    }

    #[test]
    fn exact_other_entry_allows_enrollment_and_retains_it_unchanged() {
        for retired in [false, true] {
            let mut snapshot = target();
            second(&mut snapshot);
            let mut ledger = existing(&snapshot);
            if retired {
                retire(&mut ledger.entries[0]);
            }
            let original = ledger.entries[0].clone();
            let mut options = enrollment(false);
            options.selection.path = "other.md".into();
            let result =
                enroll_context(context(snapshot, Some(ledger)), &options, |_| Ok(())).unwrap();
            assert_eq!(result.candidate_ledger.entries[0], original);
            assert_eq!(
                result.base.git_blob_id.as_deref(),
                Some("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
            );
            assert_eq!(result.base.condition, "existing_blob_at_commit");
        }
    }

    #[test]
    fn reservations_ignore_parser_and_mw_id_and_include_pending_and_stable_hints() {
        let snapshot = target();
        let mut ledger = existing(&snapshot);
        let mut ack = ledger.entries[0].source_ack.clone();
        ack.mw_id = "different".into();
        ack.parser_sha256 = "f".repeat(64);
        ack.commit = "3".repeat(40);
        assert!(require_unreserved(&ledger, &ack, None).is_err());
        ledger.entries[0].source_ack.path = "old.md".into();
        pending(
            &mut ledger.entries[0],
            OperationKind::Update,
            OperationPhase::Uncertain,
        );
        ledger.entries[0].pending.as_mut().unwrap().intended_source = ack.clone();
        assert!(require_unreserved(&ledger, &ack, None).is_err());
        ledger.entries[0].pending = None;
        ledger.entries[0].source_ack.stable_id = Some("a".repeat(32));
        ack.stable_id = Some("a".repeat(32));
        assert!(require_unreserved(&ledger, &ack, None).is_err());
    }

    #[test]
    fn reack_moves_and_date_changes_preserve_uuid_provider_lifecycle_and_other_entries() {
        for lifecycle in [Lifecycle::Enrolled, Lifecycle::Active] {
            let mut snapshot = target();
            second(&mut snapshot);
            let mut ledger = existing(&snapshot);
            ledger.entries.push(entry(&snapshot, 1));
            retire(&mut ledger.entries[1]);
            if lifecycle == Lifecycle::Enrolled {
                ledger.entries[0].lifecycle = lifecycle;
                ledger.entries[0].provider_ack = None;
            }
            let before = ledger.clone();
            snapshot.commit = "3".repeat(40);
            snapshot.items[0].source_path = "moved.md".into();
            snapshot.items[0].line = 4;
            snapshot.items[0].id = "tasks:4:0".into();
            snapshot.items[0].metadata.due = "2026-12-01".into();
            snapshot.items[0].literal_text = "- [ ] Ship due:2026-12-01".into();
            let blob = snapshot.files.remove("tasks.md").unwrap();
            snapshot.files.insert("moved.md".into(), blob);
            let mut options = reack_options();
            options.selection = TaskSelection {
                path: "moved.md".into(),
                line: 4,
            };
            options.entry = EntrySelection::AcknowledgedLocation {
                path: "tasks.md".into(),
                line: 1,
            };
            let result = reack_context(context(snapshot, Some(ledger)), &options).unwrap();
            let mut expected = before;
            expected.entries[0].source_ack = result.candidate_ledger.entries[0].source_ack.clone();
            expected.source_watermark = "3".repeat(40);
            assert_eq!(result.candidate_ledger, expected);
            assert_eq!(result.logical_id, expected.entries[0].logical_id);
            assert_eq!(result.operation, CandidateOperation::Reack);
            assert_eq!(result.base.commit, "f".repeat(40));
            assert_eq!(result.base.path, "state/calendar.json");
            assert_eq!(result.base.git_blob_id, Some("e".repeat(40)));
        }
    }

    #[test]
    fn reack_refuses_all_pending_kinds_phases_and_retired() {
        for kind in [
            OperationKind::Create,
            OperationKind::Update,
            OperationKind::Delete,
        ] {
            for phase in [OperationPhase::Prepared, OperationPhase::Uncertain] {
                let snapshot = target();
                let mut ledger = existing(&snapshot);
                pending(&mut ledger.entries[0], kind, phase);
                assert!(reack_context(context(snapshot, Some(ledger)), &reack_options()).is_err());
            }
        }
        let snapshot = target();
        let mut ledger = existing(&snapshot);
        retire(&mut ledger.entries[0]);
        assert!(reack_context(context(snapshot, Some(ledger)), &reack_options()).is_err());
    }

    #[test]
    fn reack_allows_done_undated_and_open_but_never_invalid_due_on_done() {
        for state in 0..7 {
            let mut snapshot = target();
            let ledger = existing(&snapshot);
            match state {
                0 => {}
                1 => snapshot.items[0].done = true,
                2 => {
                    snapshot.items[0].metadata.due.clear();
                    snapshot.items[0].literal_text = "- [ ] Ship".into();
                }
                _ => {
                    snapshot.items[0].done = true;
                    snapshot.items[0].literal_text = match state {
                        3 => "- [x] Ship due:bad",
                        4 => "- [x] Ship due:2026-11-01 due:2026-11-01",
                        5 => "- [x] Ship due:2026-11-02",
                        _ => "- [x] Ship due:9999-12-31",
                    }
                    .into();
                    if state == 6 {
                        snapshot.items[0].metadata.due = "9999-12-31".into();
                    }
                }
            }
            assert_eq!(
                reack_context(context(snapshot, Some(ledger)), &reack_options()).is_ok(),
                state < 3,
                "{state}"
            );
        }
    }

    #[test]
    fn parser_acceptance_only_migrates_selected_entry() {
        let mut snapshot = target();
        second(&mut snapshot);
        let mut ledger = existing(&snapshot);
        ledger.entries.push(entry(&snapshot, 1));
        for entry in &mut ledger.entries {
            entry.source_ack.parser_sha256 = "f".repeat(64);
        }
        let mut options = reack_options();
        assert_eq!(
            reack_context(context(target(), Some(ledger.clone())), &options)
                .unwrap_err()
                .to_string(),
            "candidate parser change requires acceptance"
        );
        options.accept_parser_change = true;
        let result = reack_context(context(snapshot, Some(ledger.clone())), &options).unwrap();
        assert_eq!(
            result.candidate_ledger.entries[0].source_ack.parser_sha256,
            "a".repeat(64)
        );
        assert_eq!(result.candidate_ledger.entries[1], ledger.entries[1]);
    }

    #[test]
    fn reack_refuses_other_entry_and_pending_reservations_even_retired() {
        for state in 0..3 {
            let mut snapshot = target();
            second(&mut snapshot);
            let mut ledger = existing(&snapshot);
            ledger.entries.push(entry(&snapshot, 1));
            if state == 1 {
                retire(&mut ledger.entries[1]);
            }
            if state == 2 {
                pending(
                    &mut ledger.entries[1],
                    OperationKind::Update,
                    OperationPhase::Uncertain,
                );
                ledger.entries[1].source_ack.path = "older.md".into();
            }
            let mut options = reack_options();
            options.selection.path = "other.md".into();
            assert_eq!(
                reack_context(context(snapshot, Some(ledger)), &options)
                    .unwrap_err()
                    .to_string(),
                "candidate source already reserved"
            );
        }
    }

    #[test]
    fn exact_reack_can_be_identical_without_provider_noop_claim() {
        let snapshot = target();
        let mut ledger = existing(&snapshot);
        ledger.entries[0].source_ack.commit = snapshot.commit.clone();
        ledger.source_watermark = snapshot.commit.clone();
        let result =
            reack_context(context(snapshot, Some(ledger.clone())), &reack_options()).unwrap();
        assert_eq!(result.candidate_ledger, ledger);
        assert!(!serde_json::to_string(&result).unwrap().contains("noop"));
    }

    #[test]
    fn selectors_missing_state_and_initialization_mismatch_refuse() {
        for path in [
            "",
            "/tasks.md",
            "../tasks.md",
            "a//tasks.md",
            "a/.git/tasks.md",
            "a\\tasks.md",
            "private\n.md",
        ] {
            let bad = TaskSelection {
                path: path.into(),
                line: 1,
            };
            assert_eq!(
                validate_selection(&bad).unwrap_err().to_string(),
                "invalid candidate task selection"
            );
        }
        assert!(
            validate_selection(&TaskSelection {
                path: "tasks.md".into(),
                line: 0
            })
            .is_err()
        );
        let mut snapshot = target();
        snapshot.items.push(snapshot.items[0].clone());
        assert!(selected(&snapshot, &selection()).is_err());
        snapshot.items.clear();
        assert!(selected(&snapshot, &selection()).is_err());
        assert!(
            enroll_context(context(target(), None), &enrollment(false), |_| panic!(
                "missing ledger"
            ))
            .is_err()
        );
        assert!(
            enroll_context(
                context(target(), Some(empty())),
                &enrollment(true),
                |_| panic!("existing ledger")
            )
            .is_err()
        );
        assert!(reack_context(context(target(), None), &reack_options()).is_err());
        assert!(reack_context(context(target(), Some(empty())), &reack_options()).is_err());
    }

    #[test]
    fn final_candidate_revalidates_ledger_bounds_and_binding() {
        let snapshot = target();
        let mut ledger = existing(&snapshot);
        ledger.entries[0].source_ack.expected.raw = "private".repeat(10_000);
        assert_eq!(
            finish(
                &snapshot,
                &binding(),
                &location(),
                None,
                ledger,
                "unused".into(),
                CandidateOperation::Reack
            )
            .unwrap_err()
            .to_string(),
            "ledger expected task limit exceeded"
        );
        let mut other_binding = binding();
        other_binding.calendar_id = "other@example.invalid".into();
        assert_eq!(
            finish(
                &snapshot,
                &other_binding,
                &location(),
                None,
                empty(),
                "unused".into(),
                CandidateOperation::Reack,
            )
            .unwrap_err()
            .to_string(),
            "ledger binding mismatch"
        );
    }

    #[test]
    fn enrollment_refuses_completed_undated_bad_due_and_oversized_source() {
        for state in 0..5 {
            let mut snapshot = target();
            match state {
                0 => snapshot.items[0].done = true,
                1 => {
                    snapshot.items[0].metadata.due.clear();
                    snapshot.items[0].literal_text = "- [ ] Ship".into();
                }
                2 => snapshot.items[0].literal_text = "- [ ] Ship due:bad".into(),
                3 => snapshot.items[0].metadata.raw = "private".repeat(10_000),
                _ => snapshot.items[0].metadata.estimate = "private".repeat(200),
            }
            assert!(
                enroll_context(context(snapshot, None), &enrollment(true), |_| {
                    panic!("invalid source must fail before entropy")
                })
                .is_err(),
                "{state}"
            );
        }
    }

    #[test]
    fn reack_preserves_unselected_pending_and_refuses_ambiguous_old_locations() {
        let mut snapshot = target();
        second(&mut snapshot);
        let mut ledger = existing(&snapshot);
        ledger.entries.push(entry(&snapshot, 1));
        pending(
            &mut ledger.entries[1],
            OperationKind::Update,
            OperationPhase::Uncertain,
        );
        let unchanged = ledger.entries[1].clone();
        let result = reack_context(context(snapshot, Some(ledger)), &reack_options()).unwrap();
        assert_eq!(result.candidate_ledger.entries[1], unchanged);

        let snapshot = target();
        let mut ledger = existing(&snapshot);
        let mut other = ledger.entries[0].clone();
        other.logical_id = "22222222-2222-4222-8222-222222222222".into();
        other.source_ack.git_blob_id = "d".repeat(40);
        other.provider_ack.as_mut().unwrap().event_id = "other0".into();
        ledger.entries.push(other);
        let mut options = reack_options();
        options.entry = EntrySelection::AcknowledgedLocation {
            path: "tasks.md".into(),
            line: 1,
        };
        assert_eq!(
            reack_context(context(snapshot, Some(ledger)), &options)
                .unwrap_err()
                .to_string(),
            "ambiguous candidate entry selection"
        );
    }
}
