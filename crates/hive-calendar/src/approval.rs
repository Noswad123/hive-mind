//! Local approval is an exact-byte authorization followed by a replay, not a ledger import.
use std::{fs::OpenOptions, io::Read, path::Path, time::Instant};

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ApplyOptions, EnrollmentOptions, EntrySelection, Ledger, LedgerLocation, PreparedLedgerChange,
    PreviewOptions, ReackOptions, TaskSelection, candidates, ledger,
    planner::{self, PlanningContext},
    source,
};

const CANDIDATE_LIMIT: usize = 8 * 1024 * 1024;

// Keep the generator's borrowed public wire types unchanged. Approval owns all
// untrusted strings and rejects unknown fields at every level, including the ledger.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireCandidate {
    schema_version: String,
    kind: String,
    operation: WireOperation,
    logical_id: String,
    source_commit: String,
    parser_sha256: String,
    base: WireBase,
    candidate_ledger: Ledger,
    publication_ready: bool,
    remote_verified: bool,
    ledger_head_verified: bool,
    requires_ledger_compare_and_swap: bool,
    diagnostics: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Binding, CommittedSnapshot, LedgerEntry, Lifecycle, OperationKind, OperationPhase,
        PendingOperation, ProviderAck, RemoteDisposition, SourceBlob, SourceTask, TaskMetadata,
        Tombstone,
    };
    use serde_json::{Value, json};
    use std::{collections::BTreeMap, fs, time::Duration};

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(120)
    }

    fn validate(options: &ApplyOptions, deadline: Instant) -> Result<PreparedLedgerChange> {
        let operation = source::SourceSession::operation(deadline)?;
        super::validate(options, deadline, &operation)
    }

    fn capture(path: &Path, expected: &str, deadline: Instant) -> Result<(Vec<u8>, String)> {
        let operation = source::SourceSession::operation(deadline)?;
        super::capture(path, expected, deadline, &operation)
    }

    fn options(create_ref: bool) -> ApplyOptions {
        ApplyOptions {
            source: PreviewOptions {
                repository: "/unavailable/source".into(),
                commit: "2".repeat(40),
                mw_binary: "/unavailable/mw".into(),
            },
            binding: Binding {
                provider: "google_calendar".into(),
                account_id: format!("google-sub-v1:{}", "a".repeat(64)),
                calendar_id: "calendar@example.invalid".into(),
                source_repo_id: "approval-test".into(),
            },
            ledger_repository: "/unavailable/state".into(),
            ledger_path: "state/ledger.json".into(),
            state_ref: "refs/hive/calendar/test".into(),
            candidate_path: "/unavailable/candidate".into(),
            expected_candidate_sha256: "a".repeat(64),
            create_ref,
            accept_parser_change: false,
        }
    }

    fn snapshot() -> CommittedSnapshot {
        let mut snapshot = CommittedSnapshot {
            commit: "2".repeat(40),
            parser_sha256: "a".repeat(64),
            files: BTreeMap::new(),
            items: vec![],
        };
        for index in 0..3 {
            let path = format!("task{index}.md");
            let content = format!("- [ ] Ship {index} due:2026-11-01\n");
            snapshot.files.insert(
                path.clone(),
                SourceBlob {
                    object_id: format!("{:040x}", index + 10),
                    content: content.as_bytes().to_vec(),
                },
            );
            snapshot.items.push(SourceTask {
                id: format!("task{index}:1:0"),
                stable_id: None,
                identity_kind: "positional".into(),
                source_revision: format!("sha256:{:x}", Sha256::digest(content.as_bytes())),
                source_id: format!("task{index}"),
                source_path: path,
                line: 1,
                text: format!("Ship {index} due:2026-11-01"),
                literal_text: content.trim_end().into(),
                done: false,
                metadata: TaskMetadata {
                    due: "2026-11-01".into(),
                    raw: String::new(),
                    start: String::new(),
                    estimate: String::new(),
                },
            });
        }
        snapshot
    }

    fn context(existing: bool, old_parser: bool) -> PlanningContext {
        let target = snapshot();
        let ledger = existing.then(|| {
            let mut entries = Vec::new();
            for (index, task) in target.items.iter().enumerate() {
                let mut ack = planner::observation(&target, task).unwrap();
                ack.commit = "1".repeat(40);
                if old_parser {
                    ack.parser_sha256 = "f".repeat(64);
                }
                entries.push(LedgerEntry {
                    logical_id: format!("11111111-1111-4111-8111-{:012x}", index + 1),
                    lifecycle: Lifecycle::Active,
                    source_ack: ack,
                    provider_ack: Some(ProviderAck {
                        event_id: format!("event{index}"),
                        etag: "original-etag".into(),
                        last_payload_hash: format!("sha256:{}", "d".repeat(64)),
                    }),
                    pending: None,
                    tombstone: None,
                });
            }
            entries[1].pending = Some(PendingOperation {
                operation_id: "22222222-2222-4222-8222-222222222222".into(),
                kind: OperationKind::Update,
                phase: OperationPhase::Uncertain,
                intended_source: entries[1].source_ack.clone(),
                target_event_id: "event1".into(),
                base_etag: Some("original-etag".into()),
                desired_payload_hash: Some(format!("sha256:{}", "e".repeat(64))),
            });
            entries[2].lifecycle = Lifecycle::Retired;
            entries[2].tombstone = Some(Tombstone {
                reason: "retired".into(),
                source_commit: "1".repeat(40),
                operation_id: None,
                remote_disposition: RemoteDisposition::Retained,
            });
            Ledger {
                schema_version: "1".into(),
                binding: options(false).binding,
                source_watermark: "1".repeat(40),
                entries,
            }
        });
        let basis = existing.then(|| planner::LedgerBasis {
            commit: "f".repeat(40),
            path: options(false).ledger_path,
            git_blob_id: "e".repeat(40),
        });
        PlanningContext {
            target,
            ledger,
            basis,
        }
    }

    fn generated(existing: bool, old_parser: bool) -> Value {
        let options = options(!existing);
        let location = LedgerLocation {
            repository: options.ledger_repository,
            commit: "f".repeat(40),
            path: options.ledger_path,
        };
        let selection = TaskSelection {
            path: "task0.md".into(),
            line: 1,
        };
        let candidate = if existing {
            candidates::reack_context(
                context(true, old_parser),
                &ReackOptions {
                    source: options.source,
                    binding: options.binding,
                    ledger: location,
                    entry: EntrySelection::LogicalId("11111111-1111-4111-8111-000000000001".into()),
                    selection,
                    accept_parser_change: true,
                },
            )
            .unwrap()
        } else {
            candidates::enroll_context(
                context(false, false),
                &EnrollmentOptions {
                    source: options.source,
                    binding: options.binding,
                    ledger: location,
                    selection,
                    initialize_ledger: true,
                },
                |bytes| {
                    bytes.fill(0x12);
                    Ok(())
                },
            )
            .unwrap()
        };
        serde_json::to_value(candidate).unwrap()
    }

    fn approved(
        value: &Value,
        options: &ApplyOptions,
        context: PlanningContext,
    ) -> Result<PreparedLedgerChange> {
        let bytes = serde_json::to_vec(value).unwrap();
        let deadline = deadline();
        let operation = source::SourceSession::operation(deadline)?;
        replay(
            options,
            decode(&bytes, options)?,
            context,
            format!("{:x}", Sha256::digest(&bytes)),
            deadline,
            &operation,
        )
    }

    #[test]
    fn cpu_replay_checks_cancellation_through_final_prepared_change() {
        for existing in [false, true] {
            let options = options(!existing);
            let value = generated(existing, false);
            let bytes = serde_json::to_vec(&value).unwrap();
            let deadline = deadline();
            let operation = source::SourceSession::operation(deadline).unwrap();
            let mut checks = 0;
            replay_checked(
                &options,
                decode(&bytes, &options).unwrap(),
                context(existing, false),
                "a".repeat(64),
                || {
                    before(deadline, &operation)?;
                    checks += 1;
                    Ok(())
                },
            )
            .unwrap();
            assert!(checks >= 10);
            // Inject cancellation at every CPU checkpoint without process-wide signals.
            for cancel_at in 1..=checks {
                let operation = source::SourceSession::operation(deadline).unwrap();
                let mut seen = 0;
                let error = replay_checked(
                    &options,
                    decode(&bytes, &options).unwrap(),
                    context(existing, false),
                    "a".repeat(64),
                    || {
                        seen += 1;
                        if seen == cancel_at {
                            operation.cancel();
                        }
                        before(deadline, &operation)
                    },
                )
                .err()
                .expect("cancelled replay must not prepare a change");
                assert_eq!(error.to_string(), "source operation cancelled");
            }
        }
    }

    #[test]
    fn cancelled_root_refuses_before_candidate_capture_or_decode() {
        let deadline = deadline();
        let operation = source::SourceSession::operation(deadline).unwrap();
        operation.cancel();
        let error = super::validate(&options(true), deadline, &operation)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "source operation cancelled");
        assert_eq!(
            super::capture(Path::new("relative"), "invalid", deadline, &operation)
                .unwrap_err()
                .to_string(),
            "source operation cancelled"
        );
    }

    #[test]
    fn deterministic_replay_preserves_full_ledger_and_only_records_needed_parser_acceptance() {
        for existing in [false, true] {
            let mut options = options(!existing);
            options.accept_parser_change = true;
            let value = generated(existing, false);
            let first = approved(&value, &options, context(existing, false)).unwrap();
            let second = approved(&value, &options, context(existing, false)).unwrap();
            assert_eq!(first.candidate_ledger, second.candidate_ledger);
            assert_eq!(
                first.candidate_ledger,
                serde_json::to_vec(
                    &decode(&serde_json::to_vec(&value).unwrap(), &options)
                        .unwrap()
                        .candidate_ledger
                )
                .unwrap()
            );
            assert!(!first.parser_change_accepted);
            assert_eq!(first.binding, options.binding);
            assert_eq!(first.expected_blob_id, existing.then(|| "e".repeat(40)));
        }
        let value = generated(true, true);
        let mut options = options(false);
        assert!(approved(&value, &options, context(true, true)).is_err());
        options.accept_parser_change = true;
        assert!(
            approved(&value, &options, context(true, true))
                .unwrap()
                .parser_change_accepted
        );
    }

    #[test]
    fn strict_wire_rejects_duplicates_unknown_fields_flags_and_types_without_echoing() {
        let options = options(false);
        let value = generated(true, false);
        let text = serde_json::to_string(&value).unwrap();
        for key in [
            "kind",
            "commit",
            "provider",
            "logicalId",
            "text",
            "etag",
            "phase",
            "reason",
        ] {
            let needle = format!("\"{key}\":");
            let duplicate =
                text.replacen(&needle, &format!("\"{key}\":\"private-value\",{needle}"), 1);
            assert_eq!(
                decode(duplicate.as_bytes(), &options)
                    .unwrap_err()
                    .to_string(),
                "invalid candidate JSON or schema"
            );
        }
        for pointer in [
            "",
            "/base",
            "/candidateLedger",
            "/candidateLedger/binding",
            "/candidateLedger/entries/0",
            "/candidateLedger/entries/0/sourceAck",
            "/candidateLedger/entries/0/sourceAck/expected",
            "/candidateLedger/entries/0/providerAck",
            "/candidateLedger/entries/1/pending",
            "/candidateLedger/entries/1/pending/intendedSource",
            "/candidateLedger/entries/2/tombstone",
        ] {
            let mut bad = value.clone();
            bad.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("private-key".into(), json!({"private-value": 1}));
            let error = decode(&serde_json::to_vec(&bad).unwrap(), &options).unwrap_err();
            assert!(!format!("{error:#}").contains("private"));
        }
        for (pointer, replacement) in [
            ("/schemaVersion", json!(1)),
            ("/schemaVersion", json!("2")),
            ("/kind", json!("other")),
            ("/operation", json!("delete")),
            ("/publicationReady", json!(true)),
            ("/remoteVerified", json!(true)),
            ("/ledgerHeadVerified", json!(true)),
            ("/requiresLedgerCompareAndSwap", json!(false)),
            ("/diagnostics", json!(["private-message"])),
            ("/base/condition", json!("unknown")),
            ("/base/gitBlobID", Value::Null),
            ("/candidateLedger/entries/0/sourceAck/line", json!("1")),
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                decode(&serde_json::to_vec(&bad).unwrap(), &options).is_err(),
                "{pointer}"
            );
        }
        for bytes in [
            br#"{"hidden":[{"x":1,"x":2}]}"#.as_slice(),
            b"{} {}",
            br#"{"a":1,"\u0061":2}"#,
        ] {
            assert!(decode(bytes, &options).is_err());
        }
    }

    #[test]
    fn independent_inputs_parser_basis_and_source_evidence_cannot_be_forged() {
        let options = options(false);
        let value = generated(true, false);
        for (pointer, replacement) in [
            ("/sourceCommit", json!("3".repeat(40))),
            ("/base/path", json!("other.json")),
            ("/base/commit", json!("HEAD")),
            ("/base/gitBlobID", json!("d".repeat(40))),
            ("/parserSha256", json!("b".repeat(64))),
            (
                "/candidateLedger/binding/calendarId",
                json!("other@example.invalid"),
            ),
            ("/candidateLedger/sourceWatermark", json!("1".repeat(40))),
            (
                "/candidateLedger/entries/0/sourceAck/expected/text",
                json!("forged private title"),
            ),
            (
                "/candidateLedger/entries/0/sourceAck/commit",
                json!("1".repeat(40)),
            ),
            (
                "/candidateLedger/entries/0/sourceAck/gitBlobID",
                json!("d".repeat(40)),
            ),
            (
                "/candidateLedger/entries/0/sourceAck/parserSha256",
                json!("b".repeat(64)),
            ),
            (
                "/candidateLedger/entries/0/sourceAck/expected/raw",
                json!("private-forged"),
            ),
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = replacement;
            let error = approved(&bad, &options, context(true, false))
                .err()
                .unwrap();
            assert!(!format!("{error:#}").contains("private"), "{pointer}");
        }
        let mut bad = value;
        bad["base"]["commit"] = json!("d".repeat(40));
        assert!(approved(&bad, &options, context(true, false)).is_err());
    }

    #[test]
    fn replay_never_washes_out_provider_pending_tombstone_order_or_unselected_edits() {
        let options = options(false);
        let value = generated(true, false);
        for (pointer, replacement) in [
            (
                "/candidateLedger/entries/0/providerAck/etag",
                json!("forged-etag"),
            ),
            (
                "/candidateLedger/entries/0/providerAck/eventId",
                json!("other0"),
            ),
            ("/candidateLedger/entries/1/pending", Value::Null),
            (
                "/candidateLedger/entries/1/pending/phase",
                json!("prepared"),
            ),
            (
                "/candidateLedger/entries/2/tombstone/reason",
                json!("forged retirement"),
            ),
            (
                "/candidateLedger/entries/2/tombstone/remoteDisposition",
                json!("unknown"),
            ),
            (
                "/candidateLedger/entries/1/sourceAck/expected/text",
                json!("forged unselected"),
            ),
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                approved(&bad, &options, context(true, false)).is_err(),
                "{pointer}"
            );
        }
        let mut bad = value.clone();
        bad["candidateLedger"]["entries"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        assert!(approved(&bad, &options, context(true, false)).is_err());
        let mut bad = value;
        bad["candidateLedger"]["entries"]
            .as_array_mut()
            .unwrap()
            .remove(2);
        assert!(approved(&bad, &options, context(true, false)).is_err());
    }

    #[test]
    fn enrollment_requires_canonical_v4_identity_and_replays_without_new_randomness() {
        let value = generated(false, false);
        let options = options(true);
        for id in [
            "11111111-1111-1111-8111-111111111111",
            "11111111-1111-4111-7111-111111111111",
            "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
            "not-a-uuid",
        ] {
            let mut bad = value.clone();
            bad["logicalId"] = json!(id);
            bad["candidateLedger"]["entries"][0]["logicalId"] = json!(id);
            assert!(approved(&bad, &options, context(false, false)).is_err());
        }
        let result = approved(&value, &options, context(false, false)).unwrap();
        assert_eq!(result.logical_id, "12121212-1212-4212-9212-121212121212");
        let mut bad = value.clone();
        bad["candidateLedger"]["entries"][0]["sourceAck"]["expected"]["text"] = json!("forged");
        assert!(approved(&bad, &options, context(false, false)).is_err());
        let mut no_creation = options;
        no_creation.create_ref = false;
        assert!(decode(&serde_json::to_vec(&value).unwrap(), &no_creation).is_err());
    }

    #[test]
    fn capture_is_bounded_exact_byte_approval_and_never_reopens() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("candidate");
        let bytes = b"private exact bytes\n";
        fs::write(&path, bytes).unwrap();
        let digest = format!("{:x}", Sha256::digest(bytes));
        let (captured, actual) = capture(&path, &digest, deadline()).unwrap();
        fs::write(&path, b"replaced").unwrap();
        assert_eq!(captured, bytes);
        assert_eq!(actual, digest);
        for bad in [
            String::new(),
            "A".repeat(64),
            "a".repeat(63),
            "a".repeat(64),
        ] {
            let error = capture(&path, &bad, deadline()).unwrap_err().to_string();
            assert!(
                error == "candidate digest mismatch"
                    || error == "invalid approved candidate digest"
            );
        }
        assert!(capture(Path::new("relative"), &digest, deadline()).is_err());
        assert!(capture(temp.path(), &digest, deadline()).is_err());
        assert!(capture(&path, &digest, Instant::now()).is_err());
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(CANDIDATE_LIMIT as u64 + 1).unwrap();
        assert_eq!(
            capture(&path, &digest, deadline()).unwrap_err().to_string(),
            "candidate file limit exceeded"
        );
        #[cfg(unix)]
        {
            use std::{
                ffi::CString,
                os::unix::{ffi::OsStrExt, fs::symlink},
            };
            let link = temp.path().join("symlink");
            symlink(&path, &link).unwrap();
            assert!(capture(&link, &digest, deadline()).is_err());
            let fifo = temp.path().join("fifo");
            let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
            assert_eq!(
                capture(&fifo, &digest, deadline()).unwrap_err().to_string(),
                "candidate file must be regular"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn local_objects_validate_original_history_before_replacement_and_parser_drift() {
        use std::{os::unix::fs::PermissionsExt, process::Command};

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
                    "user.name=Approval Fixture",
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
            git(&["commit", "--quiet", "-m", "isolated approval fixture"]);
            git(&["rev-parse", "HEAD"])
        };
        git(&["init", "--quiet"]);
        let fixture = snapshot();
        fs::write(repo.join("task0.md"), &fixture.files["task0.md"].content).unwrap();
        let source_commit = commit();
        let task = &fixture.items[0];
        let task_json = json!({
            "id": task.id, "stableId": null, "identityKind": "positional",
            "sourceRevision": task.source_revision, "sourceId": task.source_id,
            "sourcePath": task.source_path, "line": task.line, "text": task.text,
            "done": false, "metadata": {"due": task.metadata.due, "raw": "", "start": "", "estimate": ""}
        });
        let mw = root.join("mw");
        let script = format!(
            "#!/bin/sh\nset -eu\n[ \"$#\" = 3 ] && [ \"$1\" = todos ] && [ \"$2\" = snapshot ] && [ \"$3\" = --json ]\nprintf '{{\"schemaVersion\":\"1\",\"source\":\"saved-markdown\",\"notesDir\":\"%s\",\"dashboardPath\":\"%s\",\"items\":[{task_json}]}}' \"$NOTES_DIR\" \"$DASHBOARD_PATH\"\n"
        );
        fs::write(&mw, &script).unwrap();
        fs::set_permissions(&mw, fs::Permissions::from_mode(0o700)).unwrap();
        let mut options = options(true);
        options.source = PreviewOptions {
            repository: repo.clone(),
            commit: source_commit.clone(),
            mw_binary: mw.clone(),
        };
        options.ledger_repository = repo.clone();
        options.candidate_path = root.join("candidate.json");
        let enrollment = EnrollmentOptions {
            source: PreviewOptions {
                repository: repo.clone(),
                commit: source_commit.clone(),
                mw_binary: mw.clone(),
            },
            binding: options.binding.clone(),
            ledger: LedgerLocation {
                repository: repo.clone(),
                commit: source_commit.clone(),
                path: options.ledger_path.clone(),
            },
            selection: TaskSelection {
                path: "task0.md".into(),
                line: 1,
            },
            initialize_ledger: true,
        };
        let candidate = candidates::enroll(&enrollment).unwrap();
        let approve = |value: &Value, options: &mut ApplyOptions| {
            let bytes = serde_json::to_vec_pretty(value).unwrap();
            fs::write(&options.candidate_path, &bytes).unwrap();
            options.expected_candidate_sha256 = format!("{:x}", Sha256::digest(&bytes));
        };
        let mut value = serde_json::to_value(&candidate).unwrap();
        approve(&value, &mut options);
        let change = validate(&options, deadline()).unwrap();
        assert_eq!(change.source_commit, source_commit);
        assert_eq!(change.expected_blob_id, None);
        assert_eq!(git(&["rev-parse", "HEAD"]), source_commit);
        assert_eq!(git(&["status", "--porcelain"]), "");
        // A real regular blob at the declared initialization path cannot be adopted.
        fs::create_dir(repo.join("state")).unwrap();
        fs::write(repo.join(&options.ledger_path), &change.candidate_ledger).unwrap();
        let ledger_commit = commit();
        value["base"]["commit"] = json!(ledger_commit);
        approve(&value, &mut options);
        assert!(validate(&options, deadline()).is_err());

        let reack = ReackOptions {
            source: PreviewOptions {
                repository: repo.clone(),
                commit: source_commit.clone(),
                mw_binary: mw.clone(),
            },
            binding: options.binding.clone(),
            ledger: LedgerLocation {
                repository: repo.clone(),
                commit: ledger_commit.clone(),
                path: options.ledger_path.clone(),
            },
            entry: EntrySelection::LogicalId(candidate.logical_id),
            selection: TaskSelection {
                path: "task0.md".into(),
                line: 1,
            },
            accept_parser_change: false,
        };
        value = serde_json::to_value(candidates::reack(&reack).unwrap()).unwrap();
        options.create_ref = false;
        approve(&value, &mut options);
        let first = validate(&options, deadline()).unwrap();
        assert_eq!(
            first.candidate_ledger,
            validate(&options, deadline()).unwrap().candidate_ledger
        );
        assert_eq!(git(&["rev-parse", "HEAD"]), ledger_commit);
        assert_eq!(git(&["status", "--porcelain"]), "");

        // Same parser output, different invoked artifact: the approved digest is stale.
        fs::write(&mw, format!("{script}# different artifact\n")).unwrap();
        assert_eq!(
            validate(&options, deadline()).err().unwrap().to_string(),
            "candidate source or parser changed"
        );
        fs::write(&mw, &script).unwrap();

        // Replacing the selected ack must not wash forged original metadata away.
        let mut forged = candidate.candidate_ledger;
        forged.entries[0].source_ack.expected.raw = "private forged history".into();
        fs::write(
            repo.join(&options.ledger_path),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        let forged_commit = commit();
        value["base"]["commit"] = json!(forged_commit);
        value["base"]["gitBlobID"] = json!(git(&[
            "rev-parse",
            &format!("{forged_commit}:{}", options.ledger_path)
        ]));
        approve(&value, &mut options);
        assert_eq!(
            validate(&options, deadline()).err().unwrap().to_string(),
            "ledger source evidence mismatch"
        );
        assert_eq!(git(&["rev-parse", "HEAD"]), forged_commit);
        assert_eq!(git(&["status", "--porcelain"]), "");
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireBase {
    commit: String,
    path: String,
    #[serde(rename = "gitBlobID")]
    git_blob_id: Option<String>,
    condition: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireOperation {
    Enroll,
    Reack,
}

fn before(deadline: Instant, operation: &source::SourceSession) -> Result<()> {
    operation.check()?;
    ensure!(
        Instant::now() < deadline,
        "calendar approval deadline exceeded"
    );
    Ok(())
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn capture(
    path: &Path,
    expected: &str,
    deadline: Instant,
    operation: &source::SourceSession,
) -> Result<(Vec<u8>, String)> {
    before(deadline, operation)?;
    ensure!(hex(expected, 64), "invalid approved candidate digest");
    ensure!(path.is_absolute(), "candidate file must be absolute");
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    anyhow::bail!("candidate capture unsupported on this platform");
    let mut file = options
        .open(path)
        .map_err(|_| anyhow!("candidate file unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| anyhow!("candidate file unavailable"))?;
    ensure!(metadata.is_file(), "candidate file must be regular");
    ensure!(
        metadata.len() <= CANDIDATE_LIMIT as u64,
        "candidate file limit exceeded"
    );
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut hash = Sha256::new();
    loop {
        before(deadline, operation)?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| anyhow!("candidate file read failed"))?;
        if count == 0 {
            break;
        }
        ensure!(
            bytes.len() + count <= CANDIDATE_LIMIT,
            "candidate file limit exceeded"
        );
        bytes.extend_from_slice(&buffer[..count]);
        hash.update(&buffer[..count]);
    }
    let digest = format!("{:x}", hash.finalize());
    ensure!(digest == expected, "candidate digest mismatch");
    before(deadline, operation)?;
    Ok((bytes, digest))
}

fn decode(bytes: &[u8], options: &ApplyOptions) -> Result<WireCandidate> {
    ensure!(
        bytes.len() <= CANDIDATE_LIMIT,
        "candidate file limit exceeded"
    );
    let mut candidate: WireCandidate = ledger::decode_unique_json(bytes)
        .map_err(|_| anyhow!("invalid candidate JSON or schema"))?;
    ensure!(
        candidate.schema_version == "1" && candidate.kind == "calendar-ledger-candidate",
        "unsupported candidate envelope"
    );
    ensure!(
        !candidate.publication_ready
            && !candidate.remote_verified
            && !candidate.ledger_head_verified
            && candidate.requires_ledger_compare_and_swap,
        "invalid candidate safety flags"
    );
    ensure!(
        candidate.diagnostics
            == [
                "candidate_only_not_persisted",
                "remote_not_verified",
                "ledger_head_not_verified",
                "ledger_compare_and_swap_required",
                "executor_not_implemented",
            ],
        "invalid candidate diagnostics"
    );
    ensure!(
        candidate.source_commit == options.source.commit,
        "candidate source mismatch"
    );
    ensure!(
        candidate.base.path == options.ledger_path,
        "candidate ledger path mismatch"
    );
    ensure!(
        matches!(candidate.base.commit.len(), 40 | 64)
            && hex(&candidate.base.commit, candidate.base.commit.len()),
        "invalid candidate base commit"
    );
    ensure!(
        hex(&candidate.parser_sha256, 64),
        "invalid candidate parser digest"
    );
    match candidate.base.condition.as_str() {
        "path_absent_at_commit" => ensure!(
            candidate.base.git_blob_id.is_none()
                && matches!(candidate.operation, WireOperation::Enroll)
                && options.create_ref,
            "invalid candidate initialization"
        ),
        "existing_blob_at_commit" => ensure!(
            !options.create_ref
                && candidate
                    .base
                    .git_blob_id
                    .as_deref()
                    .is_some_and(|id| hex(id, candidate.base.commit.len())),
            "invalid candidate existing basis"
        ),
        _ => return Err(anyhow!("invalid candidate base condition")),
    }
    let bytes = serde_json::to_vec(&candidate.candidate_ledger)
        .map_err(|_| anyhow!("candidate ledger encoding failed"))?;
    candidate.candidate_ledger = ledger::decode(&bytes, &options.binding)?;
    ensure!(
        candidate.candidate_ledger.source_watermark == candidate.source_commit,
        "candidate watermark mismatch"
    );
    Ok(candidate)
}

pub(crate) fn validate(
    options: &ApplyOptions,
    deadline: Instant,
    operation: &source::SourceSession,
) -> Result<PreparedLedgerChange> {
    before(deadline, operation)?;
    let (bytes, digest) = capture(
        &options.candidate_path,
        &options.expected_candidate_sha256,
        deadline,
        operation,
    )?;
    before(deadline, operation)?;
    let candidate = decode(&bytes, options)?;
    before(deadline, operation)?;
    let location = LedgerLocation {
        repository: options.ledger_repository.clone(),
        commit: candidate.base.commit.clone(),
        path: options.ledger_path.clone(),
    };
    before(deadline, operation)?;
    let context = if candidate.base.condition == "path_absent_at_commit" {
        let absent = source::require_json_absent_before(
            &location.repository,
            &location.commit,
            &location.path,
            deadline,
        );
        before(deadline, operation)?;
        absent?;
        let context = planner::load_context(&options.source, &options.binding, None, deadline);
        before(deadline, operation)?;
        context?
    } else {
        // Validate the ORIGINAL history before replacing even the selected ack.
        // Historical old-parser semantics remain unverified under the planner's
        // explicit existing contract; acceptance below only migrates this entry.
        let context =
            planner::load_context(&options.source, &options.binding, Some(&location), deadline);
        before(deadline, operation)?;
        context?
    };
    before(deadline, operation)?;
    replay(options, candidate, context, digest, deadline, operation)
}

fn approved_uuid(id: &str) -> Result<[u8; 16]> {
    let valid = id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        });
    ensure!(
        valid && id.as_bytes()[14] == b'4' && b"89ab".contains(&id.as_bytes()[19]),
        "invalid approved enrollment identity"
    );
    let mut bytes = [0u8; 16];
    let mut digits = id.bytes().filter(|b| *b != b'-');
    let nibble = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
    for byte in &mut bytes {
        *byte = nibble(digits.next().expect("validated UUID")) * 16
            + nibble(digits.next().expect("validated UUID"));
    }
    Ok(bytes)
}

// Only the caller that loaded and validated original history supplies this context.
fn replay(
    options: &ApplyOptions,
    candidate: WireCandidate,
    context: PlanningContext,
    digest: String,
    deadline: Instant,
    operation: &source::SourceSession,
) -> Result<PreparedLedgerChange> {
    replay_checked(options, candidate, context, digest, || {
        before(deadline, operation)
    })
}

fn replay_checked(
    options: &ApplyOptions,
    candidate: WireCandidate,
    context: PlanningContext,
    digest: String,
    mut check: impl FnMut() -> Result<()>,
) -> Result<PreparedLedgerChange> {
    check()?;
    ensure!(
        context.target.commit == candidate.source_commit
            && context.target.parser_sha256 == candidate.parser_sha256,
        "candidate source or parser changed"
    );
    ensure!(
        context.basis.as_ref().map(|b| &b.git_blob_id) == candidate.base.git_blob_id.as_ref(),
        "candidate ledger blob mismatch"
    );
    let mut selected = candidate
        .candidate_ledger
        .entries
        .iter()
        .filter(|entry| entry.logical_id == candidate.logical_id);
    let entry = selected
        .next()
        .ok_or_else(|| anyhow!("candidate selected entry missing"))?;
    ensure!(
        selected.next().is_none(),
        "candidate selected entry ambiguous"
    );
    let selection = TaskSelection {
        path: entry.source_ack.path.clone(),
        line: entry.source_ack.line,
    };
    check()?;
    let location = LedgerLocation {
        repository: options.ledger_repository.clone(),
        commit: candidate.base.commit.clone(),
        path: options.ledger_path.clone(),
    };
    let source = PreviewOptions {
        repository: options.source.repository.clone(),
        commit: options.source.commit.clone(),
        mw_binary: options.source.mw_binary.clone(),
    };
    let mut migration = false;
    let result = match candidate.operation {
        WireOperation::Enroll => {
            let approved = approved_uuid(&candidate.logical_id)?;
            let enrollment = EnrollmentOptions {
                source,
                binding: options.binding.clone(),
                ledger: location,
                selection,
                initialize_ledger: candidate.base.condition == "path_absent_at_commit",
            };
            check()?;
            candidates::enroll_context(context, &enrollment, |bytes| {
                check()?;
                bytes.copy_from_slice(&approved);
                Ok(())
            })
        }
        WireOperation::Reack => {
            let old = context
                .ledger
                .as_ref()
                .and_then(|ledger| {
                    ledger
                        .entries
                        .iter()
                        .find(|entry| entry.logical_id == candidate.logical_id)
                })
                .ok_or_else(|| anyhow!("candidate original entry missing"))?;
            migration = old.source_ack.parser_sha256 != context.target.parser_sha256;
            ensure!(
                !migration || options.accept_parser_change,
                "candidate parser change requires acceptance"
            );
            let reack = ReackOptions {
                source,
                binding: options.binding.clone(),
                ledger: location,
                entry: EntrySelection::LogicalId(candidate.logical_id.clone()),
                selection,
                accept_parser_change: options.accept_parser_change,
            };
            check()?;
            candidates::reack_context(context, &reack)
        }
    };
    check()?;
    let replayed = result?;
    ensure!(
        replayed.candidate_ledger == candidate.candidate_ledger,
        "candidate ledger transition mismatch"
    );
    check()?;
    let replay_value =
        serde_json::to_value(&replayed).map_err(|_| anyhow!("candidate replay encoding failed"))?;
    check()?;
    let approved_value = serde_json::to_value(&candidate)
        .map_err(|_| anyhow!("candidate approval encoding failed"))?;
    check()?;
    ensure!(
        replay_value == approved_value,
        "candidate replay envelope mismatch"
    );
    check()?;
    let bytes = serde_json::to_vec(&replayed.candidate_ledger)
        .map_err(|_| anyhow!("candidate ledger encoding failed"))?;
    check()?;
    ledger::decode(&bytes, &options.binding)?;
    check()?;
    let change = PreparedLedgerChange {
        candidate_sha256: digest,
        operation: replayed.operation,
        logical_id: replayed.logical_id,
        source_commit: replayed.source_commit,
        binding: options.binding.clone(),
        parser_change_accepted: migration && options.accept_parser_change,
        base_commit: replayed.base.commit,
        path: replayed.base.path,
        expected_blob_id: replayed.base.git_blob_id,
        candidate_ledger: bytes,
    };
    check()?;
    Ok(change)
}
