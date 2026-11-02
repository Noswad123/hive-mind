//! Source-only projection. Titles and repository-relative paths are private user data;
//! preview JSON is intended for command stdout, not public CI logs.

use anyhow::{Result, anyhow};
use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;
use time::{Date, Month};

use crate::{CommittedSnapshot, SourceTask};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub schema_version: &'static str,
    pub kind: &'static str,
    pub source: &'static str,
    pub commit: String,
    pub parser_sha256: String,
    pub publication_ready: bool,
    pub diagnostics: Vec<&'static str>,
    pub counts: PreviewCounts,
    pub items: Vec<PreviewItem>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCounts {
    pub total: usize,
    pub completed_skipped: usize,
    pub undated_skipped: usize,
    pub valid_due: usize,
    pub invalid_due: usize,
    pub publication_blocked: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewItem {
    /// MW's positional ID is informational, not a durable calendar identity.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stable_id: Option<String>,
    pub identity_kind: String,
    pub source: ItemSource,
    pub publication_ready: bool,
    pub diagnostics: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prospective_event: Option<ProspectiveEvent>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSource {
    pub path: String,
    pub source_id: String,
    pub line: usize,
    pub source_revision: String,
    #[serde(rename = "gitBlobID")]
    pub git_blob_id: String,
}

#[derive(Debug, Serialize)]
pub struct ProspectiveEvent {
    pub summary: String,
    pub start: CalendarDate,
    pub end: CalendarDate,
    pub availability: &'static str,
    pub transparency: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CalendarDate {
    pub date: String,
}

pub(crate) struct DueEvidence {
    pub due: Option<String>,
    pub event: Option<ProspectiveEvent>,
    pub diagnostic: Option<&'static str>,
    pub undated: bool,
}

/// Validate committed due evidence independently of task completion.
pub(crate) fn due_evidence(task: &SourceTask) -> Result<DueEvidence> {
    static PATTERNS: OnceLock<std::result::Result<(Regex, Regex), regex::Error>> = OnceLock::new();
    // MW title normalization can hide malformed tokens in the committed line.
    let (label, shape) = PATTERNS
        .get_or_init(|| {
            Ok((
                Regex::new(r"(?i)\bdue\s*:\s*([^\s]*)")?,
                Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$")?,
            ))
        })
        .as_ref()
        .map_err(|error| anyhow!("{error}"))?;
    let tokens: Vec<_> = label
        .captures_iter(&task.literal_text)
        .chain(label.captures_iter(&task.metadata.raw))
        .map(|capture| capture[1].to_owned())
        .collect();
    let mut evidence = DueEvidence {
        due: None,
        event: None,
        diagnostic: None,
        undated: tokens.is_empty() && task.metadata.due.is_empty(),
    };
    if evidence.undated {
        return Ok(evidence);
    }
    let date = (|| -> std::result::Result<Date, &'static str> {
        if tokens.len() > 1 {
            return Err("duplicate_due");
        }
        let token = tokens.first().ok_or("parser_due_mismatch")?;
        if !shape.is_match(token) {
            return Err("date_invalid");
        }
        let year: i32 = token[..4].parse().map_err(|_| "date_invalid")?;
        if !(1..=9999).contains(&year) {
            return Err("date_invalid");
        }
        let month: u8 = token[5..7].parse().map_err(|_| "date_invalid")?;
        let day: u8 = token[8..].parse().map_err(|_| "date_invalid")?;
        let date = Date::from_calendar_date(
            year,
            Month::try_from(month).map_err(|_| "date_invalid")?,
            day,
        )
        .map_err(|_| "date_invalid")?;
        if token != &task.metadata.due {
            return Err("parser_due_mismatch");
        }
        Ok(date)
    })();
    match date {
        Ok(date) => {
            evidence.due = Some(format_date(date));
            // time may support larger years; the provider contract does not.
            if let Some(end) = date.next_day().filter(|end| end.year() <= 9999) {
                evidence.event = Some(ProspectiveEvent {
                    summary: task.text.clone(),
                    start: CalendarDate {
                        date: format_date(date),
                    },
                    end: CalendarDate {
                        date: format_date(end),
                    },
                    availability: "Available",
                    transparency: "transparent",
                });
            } else {
                evidence.diagnostic = Some("end_date_overflow");
            }
        }
        Err(diagnostic) => evidence.diagnostic = Some(diagnostic),
    }
    Ok(evidence)
}

pub(crate) fn project(snapshot: &CommittedSnapshot) -> Result<Preview> {
    let mut tasks: Vec<_> = snapshot.items.iter().collect();
    tasks.sort_by(|a, b| {
        (&a.source_path, a.line, &a.id)
            .cmp(&(&b.source_path, b.line, &b.id))
            .then_with(|| {
                (
                    &a.text,
                    &a.literal_text,
                    &a.stable_id,
                    &a.identity_kind,
                    &a.source_id,
                    &a.source_revision,
                    a.done,
                    &a.metadata.due,
                    &a.metadata.raw,
                    &a.metadata.start,
                    &a.metadata.estimate,
                )
                    .cmp(&(
                        &b.text,
                        &b.literal_text,
                        &b.stable_id,
                        &b.identity_kind,
                        &b.source_id,
                        &b.source_revision,
                        b.done,
                        &b.metadata.due,
                        &b.metadata.raw,
                        &b.metadata.start,
                        &b.metadata.estimate,
                    ))
            })
    });
    let mut preview = Preview {
        schema_version: "1",
        kind: "calendar-due-preview",
        source: "committed-markdown",
        commit: snapshot.commit.clone(),
        parser_sha256: snapshot.parser_sha256.clone(),
        publication_ready: false,
        diagnostics: vec![
            "enrollment_not_configured",
            "ledger_not_configured",
            "identity_contract_unresolved",
        ],
        counts: PreviewCounts {
            total: tasks.len(),
            ..Default::default()
        },
        items: Vec::new(),
    };
    for task in tasks {
        if task.done {
            preview.counts.completed_skipped += 1;
            continue;
        }
        let evidence = due_evidence(task)?;
        if evidence.undated {
            preview.counts.undated_skipped += 1;
            continue;
        }
        let blob = snapshot
            .files
            .get(&task.source_path)
            .ok_or_else(|| anyhow!("source blob missing from committed snapshot"))?;
        let mut item = PreviewItem {
            id: task.id.clone(),
            stable_id: task.stable_id.clone(),
            identity_kind: task.identity_kind.clone(),
            source: ItemSource {
                path: task.source_path.clone(),
                source_id: task.source_id.clone(),
                line: task.line,
                source_revision: task.source_revision.clone(),
                git_blob_id: blob.object_id.clone(),
            },
            publication_ready: false,
            diagnostics: vec![
                if task.stable_id.is_some() {
                    "identity_mapping_unconfirmed"
                } else {
                    "durable_identity_unresolved"
                },
                "enrollment_not_configured",
                "ledger_not_configured",
            ],
            due: evidence.due,
            prospective_event: evidence.event,
        };
        if let Some(diagnostic) = evidence.diagnostic {
            item.diagnostics.push(diagnostic);
            preview.counts.invalid_due += 1;
        } else {
            preview.counts.valid_due += 1;
        }
        preview.counts.publication_blocked += 1;
        preview.items.push(item);
    }
    Ok(preview)
}

fn format_date(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceBlob, SourceTask, TaskMetadata};
    use std::collections::BTreeMap;

    fn task(text: &str, raw: &str, due: &str) -> SourceTask {
        SourceTask {
            id: "notes/tasks.md:3".into(),
            stable_id: None,
            identity_kind: "positional".into(),
            source_revision: "exact-commit".into(),
            source_id: "notes/tasks.md".into(),
            source_path: "notes/tasks.md".into(),
            line: 3,
            text: text.into(),
            literal_text: text.into(),
            done: false,
            metadata: TaskMetadata {
                raw: raw.into(),
                due: due.into(),
                start: String::new(),
                estimate: String::new(),
            },
        }
    }

    fn snapshot(items: Vec<SourceTask>) -> CommittedSnapshot {
        CommittedSnapshot {
            commit: "exact-commit".into(),
            parser_sha256: "exact-parser-sha256".into(),
            files: BTreeMap::from([(
                "notes/tasks.md".into(),
                SourceBlob {
                    object_id: "exact-git-blob".into(),
                    content: b"- [ ] source fixture\n".to_vec(),
                },
            )]),
            items,
        }
    }

    #[test]
    fn dates_use_exclusive_next_calendar_day() {
        for (due, end) in [
            ("0001-01-01", "0001-01-02"),
            ("2024-02-28", "2024-02-29"),
            ("2024-02-29", "2024-03-01"),
            ("2000-02-29", "2000-03-01"),
            ("2025-02-28", "2025-03-01"),
            ("2026-04-30", "2026-05-01"),
            ("2026-12-31", "2027-01-01"),
            ("9999-12-30", "9999-12-31"),
        ] {
            let preview =
                project(&snapshot(vec![task("A task", &format!("due:{due}"), due)])).unwrap();
            let event = preview.items[0].prospective_event.as_ref().unwrap();
            assert_eq!(event.summary, "A task");
            assert_eq!(event.start.date, due);
            assert_eq!(event.end.date, end);
            assert_eq!(event.availability, "Available");
            assert_eq!(event.transparency, "transparent");
            assert_eq!(preview.counts.valid_due, 1);
        }
    }

    #[test]
    fn malformed_explicit_dates_are_visible_even_when_parser_due_is_empty() {
        for value in [
            "",
            "   ",
            "tomorrow",
            "2024-2-01",
            "2024-02-1",
            "2024-02-29junk",
            "2024-02-29,",
            "2024-02-29T10:00",
            "2023-02-29",
            "1900-02-29",
            "2024-04-31",
            "2024-00-01",
            "2024-13-01",
            "2024-01-00",
            "0000-01-01",
            "10000-01-01",
            "-001-01-01",
        ] {
            for inline in [true, false] {
                let label = format!("due:{value}");
                let input = if inline {
                    task(&label, "", "")
                } else {
                    task("Task", &label, "")
                };
                let preview = project(&snapshot(vec![input])).unwrap();
                assert_eq!(preview.counts.invalid_due, 1, "{label}");
                assert!(
                    preview.items[0].diagnostics.contains(&"date_invalid"),
                    "{label}"
                );
                assert!(preview.items[0].prospective_event.is_none());
            }
        }
    }

    #[test]
    fn labels_allow_case_and_value_spaces_but_require_word_boundary() {
        for label in ["DUE:2024-02-29", "Due: 2024-02-29", "due:\t2024-02-29"] {
            for inline in [true, false] {
                let input = if inline {
                    task(label, "", "2024-02-29")
                } else {
                    task("Task", label, "2024-02-29")
                };
                assert_eq!(project(&snapshot(vec![input])).unwrap().counts.valid_due, 1);
            }
        }
        let preview = project(&snapshot(vec![task("overdue:2024-02-29", "", "")])).unwrap();
        assert_eq!(preview.counts.undated_skipped, 1);
    }

    #[test]
    fn normalized_title_cannot_hide_malformed_literal_due() {
        let mut input = task("Ship due: 2028-02-29", "", "2028-02-29");
        input.literal_text = "- [ ] Ship due: area:Code 2028-02-29".into();
        let preview = project(&snapshot(vec![input])).unwrap();
        assert_eq!(preview.counts.invalid_due, 1);
        assert_eq!(preview.counts.valid_due, 0);
        assert!(preview.items[0].diagnostics.contains(&"date_invalid"));
        assert!(preview.items[0].prospective_event.is_none());
    }

    #[test]
    fn pre_colon_spaces_detect_explicit_due_but_do_not_override_parser() {
        // MW does not recognize this label. A normalized-looking title must not
        // turn the scanner's broader malformed-label detection into support.
        for inline in [true, false] {
            let mut input = if inline {
                task("Ship Due : 2028-02-29", "", "")
            } else {
                task("Ship", "Due : 2028-02-29", "")
            };
            input.text = "Ship due:2028-02-29".into();
            let preview = project(&snapshot(vec![input])).unwrap();
            assert_eq!(preview.counts.invalid_due, 1);
            assert!(
                preview.items[0]
                    .diagnostics
                    .contains(&"parser_due_mismatch")
            );
            assert!(preview.items[0].prospective_event.is_none());
        }
    }

    #[test]
    fn duplicates_are_blocked_within_and_across_fields() {
        for (text, raw) in [
            ("due:2024-02-29 due:2024-02-29", ""),
            ("Task", "due:2024-02-29 DUE:2024-03-01"),
            ("due:2024-02-29", "due:2024-02-29"),
            ("due:", "due:2024-02-29"),
        ] {
            let mut input = task(text, raw, "2024-02-29");
            input.text = "Normalized title".into();
            let preview = project(&snapshot(vec![input])).unwrap();
            assert!(preview.items[0].diagnostics.contains(&"duplicate_due"));
            assert_eq!(preview.counts.invalid_due, 1);
        }
    }

    #[test]
    fn parser_value_must_match_recognized_due() {
        for (raw, due) in [
            ("due:2024-02-29", ""),
            ("due:2024-02-29", "2024-03-01"),
            ("", "2024-02-29"),
        ] {
            let preview = project(&snapshot(vec![task("Task", raw, due)])).unwrap();
            assert!(
                preview.items[0]
                    .diagnostics
                    .contains(&"parser_due_mismatch")
            );
            assert!(preview.items[0].prospective_event.is_none());
        }
    }

    #[test]
    fn end_overflow_blocks_only_the_item() {
        let preview = project(&snapshot(vec![
            task("Last", "due:9999-12-31", "9999-12-31"),
            task("Safe", "due:2024-02-29", "2024-02-29"),
        ]))
        .unwrap();
        let last = preview
            .items
            .iter()
            .find(|item| item.due.as_deref() == Some("9999-12-31"))
            .unwrap();
        assert!(last.diagnostics.contains(&"end_date_overflow"));
        assert!(last.prospective_event.is_none());
        assert_eq!(preview.counts.invalid_due, 1);
        assert_eq!(preview.counts.valid_due, 1);
    }

    #[test]
    fn due_evidence_validates_completed_tasks_without_changing_preview() {
        for (text, raw, due, diagnostic, retained_due) in [
            ("Task", "due:bad", "", "date_invalid", None),
            ("due:", "", "", "date_invalid", None),
            (
                "due:2024-02-29",
                "due:2024-02-29",
                "2024-02-29",
                "duplicate_due",
                None,
            ),
            (
                "Task",
                "due:2024-02-29",
                "2024-03-01",
                "parser_due_mismatch",
                None,
            ),
            ("Task", "", "2024-02-29", "parser_due_mismatch", None),
            (
                "Task",
                "due:9999-12-31",
                "9999-12-31",
                "end_date_overflow",
                Some("9999-12-31"),
            ),
        ] {
            for done in [false, true] {
                let mut input = task(text, raw, due);
                input.done = done;
                let evidence = due_evidence(&input).unwrap();
                assert_eq!(evidence.diagnostic, Some(diagnostic));
                assert_eq!(evidence.due.as_deref(), retained_due);
                assert!(evidence.event.is_none());
                assert!(!evidence.undated);
                if done {
                    let mut snapshot = snapshot(vec![input]);
                    // Completed preview tasks must not even require source blobs.
                    snapshot.files.clear();
                    assert_eq!(
                        serde_json::to_value(project(&snapshot).unwrap()).unwrap(),
                        serde_json::json!({
                            "schemaVersion": "1",
                            "kind": "calendar-due-preview",
                            "source": "committed-markdown",
                            "commit": "exact-commit",
                            "parserSha256": "exact-parser-sha256",
                            "publicationReady": false,
                            "diagnostics": [
                                "enrollment_not_configured",
                                "ledger_not_configured",
                                "identity_contract_unresolved"
                            ],
                            "counts": {
                                "total": 1,
                                "completedSkipped": 1,
                                "undatedSkipped": 0,
                                "validDue": 0,
                                "invalidDue": 0,
                                "publicationBlocked": 0
                            },
                            "items": []
                        })
                    );
                }
            }
        }
    }

    #[test]
    fn due_evidence_ignores_completion_start_and_estimate() {
        for done in [false, true] {
            for due in ["", "2024-02-29"] {
                let mut input = task("Task", "start:bad estimate:2h", due);
                if !due.is_empty() {
                    input.metadata.raw.push_str(&format!(" due:{due}"));
                }
                input.done = done;
                input.metadata.start = "bad".into();
                input.metadata.estimate = "2h".into();
                let evidence = due_evidence(&input).unwrap();
                assert!(evidence.diagnostic.is_none());
                assert_eq!(evidence.undated, due.is_empty());
                if due.is_empty() {
                    assert!(evidence.due.is_none());
                    assert!(evidence.event.is_none());
                } else {
                    assert_eq!(evidence.due.as_deref(), Some(due));
                    let event = evidence.event.unwrap();
                    assert_eq!(event.summary, "Task");
                    assert_eq!(event.start.date, due);
                    assert_eq!(event.end.date, "2024-03-01");
                    assert_eq!(event.availability, "Available");
                    assert_eq!(event.transparency, "transparent");
                }
            }
        }
    }

    #[test]
    fn completed_and_undated_skip_and_start_estimate_never_supply_timing() {
        let mut completed = task("Completed", "due:bad", "");
        completed.done = true;
        let mut undated = task("No due", "start:2024-01-01 estimate:2h", "");
        undated.metadata.start = "2024-01-01".into();
        undated.metadata.estimate = "2h".into();
        let mut dated = task(
            "Dated",
            "due:2024-02-29 start:2024-01-01 estimate:2h",
            "2024-02-29",
        );
        dated.metadata.start = "2024-01-01".into();
        dated.metadata.estimate = "2h".into();
        let preview = project(&snapshot(vec![completed, undated, dated])).unwrap();
        assert_eq!(preview.counts.total, 3);
        assert_eq!(preview.counts.completed_skipped, 1);
        assert_eq!(preview.counts.undated_skipped, 1);
        assert_eq!(preview.items.len(), 1);
        let event = serde_json::to_value(&preview.items[0].prospective_event).unwrap();
        assert_eq!(event["start"], serde_json::json!({"date": "2024-02-29"}));
        assert_eq!(event["end"], serde_json::json!({"date": "2024-03-01"}));
    }

    #[test]
    fn provenance_identity_and_source_only_contract() {
        let unmarked = task(
            "Private title",
            "due:2024-02-29 private-marker",
            "2024-02-29",
        );
        let mut marked = unmarked.clone();
        marked.stable_id = Some("existing-stable-id".into());
        marked.identity_kind = "stable".into();
        let snapshot = snapshot(vec![unmarked, marked]);
        let before = format!("{:?}", snapshot.items);
        let preview = project(&snapshot).unwrap();
        assert_eq!(before, format!("{:?}", snapshot.items));
        assert_eq!(preview.counts.publication_blocked, 2);
        assert!(!preview.publication_ready);
        for item in &preview.items {
            assert!(!item.publication_ready);
            if item.stable_id.is_some() {
                assert!(item.diagnostics.contains(&"identity_mapping_unconfirmed"));
                assert!(!item.diagnostics.contains(&"durable_identity_unresolved"));
            } else {
                assert!(item.diagnostics.contains(&"durable_identity_unresolved"));
                assert!(!item.diagnostics.contains(&"identity_mapping_unconfirmed"));
            }
            assert_eq!(item.source.path, "notes/tasks.md");
            assert_eq!(item.source.source_id, "notes/tasks.md");
            assert_eq!(item.source.line, 3);
            assert_eq!(item.source.source_revision, "exact-commit");
            assert_eq!(item.source.git_blob_id, "exact-git-blob");
        }
        assert_eq!(
            preview
                .items
                .iter()
                .filter(|item| item.stable_id.is_some())
                .count(),
            1
        );
        let json = serde_json::to_value(&preview).unwrap();
        assert_eq!(json["schemaVersion"], "1");
        assert_eq!(json["kind"], "calendar-due-preview");
        assert_eq!(json["source"], "committed-markdown");
        assert_eq!(json["commit"], "exact-commit");
        assert_eq!(json["parserSha256"], "exact-parser-sha256");
        assert_eq!(
            json["diagnostics"],
            serde_json::json!([
                "enrollment_not_configured",
                "ledger_not_configured",
                "identity_contract_unresolved",
            ])
        );
        let serialized = serde_json::to_string(&preview).unwrap();
        for forbidden in [
            "private-marker",
            "metadata",
            "dateTime",
            "timeZone",
            "duration",
            "googleEvent",
            "remoteId",
            "action",
            "noop",
            "/tmp/",
            "/Users/",
        ] {
            assert!(!serialized.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn order_is_path_line_id_and_deterministic_even_for_ties() {
        let mut tasks = vec![
            task("Zulu", "due:2024-02-29", "2024-02-29"),
            task("Alpha", "due:2024-02-29", "2024-02-29"),
            task("First line", "due:2024-02-29", "2024-02-29"),
            task("First ID", "due:2024-02-29", "2024-02-29"),
            task("First path", "due:2024-02-29", "2024-02-29"),
        ];
        tasks[2].line = 1;
        tasks[3].id = "aaa".into();
        tasks[4].source_path = "aaa.md".into();
        let mut snapshot = snapshot(tasks);
        snapshot.files.insert(
            "aaa.md".into(),
            SourceBlob {
                object_id: "another-blob".into(),
                content: vec![],
            },
        );
        let first = project(&snapshot).unwrap();
        let titles: Vec<_> = first
            .items
            .iter()
            .map(|item| item.prospective_event.as_ref().unwrap().summary.as_str())
            .collect();
        assert_eq!(
            titles,
            ["First path", "First line", "First ID", "Alpha", "Zulu"]
        );
        snapshot.items.reverse();
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&project(&snapshot).unwrap()).unwrap()
        );
    }

    #[test]
    fn empty_preview_stays_blocked_and_counts_partition_tasks() {
        let preview = project(&snapshot(vec![])).unwrap();
        assert!(!preview.publication_ready);
        assert_eq!(preview.diagnostics.len(), 3);
        assert!(preview.items.is_empty());
        assert_eq!(preview.counts.total, 0);
        let preview = project(&snapshot(vec![
            task("Valid", "due:2024-02-29", "2024-02-29"),
            task("Invalid", "due:", ""),
            task("Undated", "", ""),
        ]))
        .unwrap();
        let counts = preview.counts;
        assert_eq!(
            counts.total,
            counts.completed_skipped
                + counts.undated_skipped
                + counts.valid_due
                + counts.invalid_due
        );
        assert_eq!(
            counts.publication_blocked,
            counts.valid_due + counts.invalid_due
        );
    }
}
