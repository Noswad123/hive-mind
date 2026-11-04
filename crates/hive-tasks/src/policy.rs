use anyhow::{Context, Result, ensure};
use hive_calendar::task_snapshot::{CommittedTask, CommittedTaskSnapshot};
use hive_capture::{
    GoogleTask,
    handoff::{MAX_JSON_BYTES, TaskHandoff, validate_open_task},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskObservation {
    pub schema_version: String,
    pub kind: String,
    pub source_repo_id: String,
    pub account_key: String,
    pub list_id: String,
    pub task: GoogleTask,
}

/// Policy output is not an execution capability; fields cannot be supplied by callers.
pub struct Candidate {
    desired: String,
    source: Value,
    decision: FirstDue,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstDue {
    SetFirstDue,
    AlreadySatisfied,
    Conflict,
}
impl Candidate {
    pub fn desired_due(&self) -> &str {
        &self.desired
    }
    pub fn source(&self) -> &Value {
        &self.source
    }
    pub fn decision(&self) -> FirstDue {
        self.decision
    }
}

fn desired_due(task: &CommittedTask, literal_metadata: &str) -> Result<String> {
    let pattern = regex::Regex::new(r"(?i)\bdue\s*:\s*([^\s]*)")?;
    let committed: Vec<_> = pattern
        .captures_iter(literal_metadata)
        .map(|c| c[1].to_owned())
        .collect();
    let parsed: Vec<_> = pattern
        .captures_iter(&task.metadata.raw)
        .map(|c| c[1].to_owned())
        .collect();
    ensure!(
        committed == parsed,
        "literal metadata and parser due disagreement"
    );
    let tokens: Vec<_> = pattern
        .captures_iter(&task.literal_text)
        .map(|c| c[1].to_owned())
        .chain(committed)
        .collect();
    ensure!(tokens.len() == 1, "desired due is absent or duplicated");
    let date = &tokens[0];
    ensure!(
        regex::Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$")?.is_match(date),
        "invalid due date shape"
    );
    let year: i32 = date[..4].parse()?;
    ensure!((1..=9999).contains(&year), "invalid due year");
    time::Date::from_calendar_date(
        year,
        time::Month::try_from(date[5..7].parse::<u8>()?)?,
        date[8..].parse()?,
    )
    .context("invalid due date")?;
    ensure!(date == &task.metadata.due, "parser due disagreement");
    Ok(format!("{date}T00:00:00.000Z"))
}

pub(crate) fn accepted_fields(task: &GoogleTask, baseline: &GoogleTask) -> Result<()> {
    validate_open_task(task)?;
    let mut remote = task.clone();
    remote.due = None;
    remote.etag = baseline.etag.clone();
    remote.updated = baseline.updated.clone();
    remote.extra.clear();
    let mut baseline = baseline.clone();
    baseline.extra.clear();
    ensure!(remote == baseline, "accepted baseline fields changed");
    Ok(())
}

pub fn evaluate(
    snapshot: &CommittedTaskSnapshot,
    source_repo_id: &str,
    handoff: &TaskHandoff,
    observation: &TaskObservation,
) -> Result<Candidate> {
    handoff.validate()?;
    ensure!(
        handoff.source_repo_id == source_repo_id && observation.source_repo_id == source_repo_id,
        "source repository binding mismatch"
    );
    ensure!(
        serde_json::to_vec(observation)?.len() <= MAX_JSON_BYTES
            && observation.schema_version == "1"
            && observation.kind == "google-task-observation",
        "unsupported observation envelope"
    );
    ensure!(
        observation.account_key == handoff.account_key
            && observation.list_id == handoff.list_id
            && observation.task.id == handoff.provider_id,
        "observation tuple mismatch"
    );
    accepted_fields(&observation.task, &handoff.baseline)?;
    let matching: Vec<_> = snapshot
        .items
        .iter()
        .filter(|t| {
            t.stable_id.as_deref() == Some(&handoff.capture_id)
                || t.id == format!("task:{}", handoff.capture_id)
        })
        .collect();
    ensure!(
        matching.len() == 1,
        "stable source identity missing or ambiguous"
    );
    let task = matching[0];
    ensure!(
        task.identity_kind == "stable"
            && task.stable_id.as_deref() == Some(&handoff.capture_id)
            && task.id == format!("task:{}", handoff.capture_id),
        "source identity mismatch"
    );
    ensure!(
        snapshot.stable_markers.get(&task.id) == Some(&handoff.capture_id),
        "literal source marker disagreement"
    );
    ensure!(
        snapshot.stable_marker_counts.get(&handoff.capture_id) == Some(&1),
        "committed source marker is ambiguous"
    );
    ensure!(
        !task.done && regex::Regex::new(r"^\s*[-*]\s*\[ \]\s+")?.is_match(&task.literal_text),
        "source task completed or parser completion disagreement"
    );
    let metadata = snapshot
        .literal_metadata
        .get(&task.id)
        .context("literal metadata evidence missing")?;
    let desired = desired_due(task, metadata)?;
    let blob = snapshot
        .blob_ids
        .get(&task.source_path)
        .context("source blob missing")?;
    let source = json!({"path":task.source_path,"line":task.line,"sourceId":task.source_id,"sourceRevision":task.source_revision,"gitBlobID":blob,"literalText":task.literal_text,"literalMetadata":metadata,"metadata":{"due":task.metadata.due,"raw":task.metadata.raw,"start":task.metadata.start,"estimate":task.metadata.estimate}});
    let decision = match &observation.task.due {
        None => FirstDue::SetFirstDue,
        Some(d) if due_matches(d, &desired) => FirstDue::AlreadySatisfied,
        Some(_) => FirstDue::Conflict,
    };
    Ok(Candidate {
        desired,
        source,
        decision,
    })
}

pub fn due_matches(value: &str, desired: &str) -> bool {
    value == desired || value == desired.replace(".000Z", "Z")
}

/// Portable observations and digests confer no authority.
pub fn first_due_plan(
    snapshot: &CommittedTaskSnapshot,
    source_repo_id: &str,
    handoff: &TaskHandoff,
    observation: &TaskObservation,
) -> Value {
    let mut out = json!({"schemaVersion":"1","kind":"google-task-first-due-plan","decision":"blocked","publicationReady":false,"remoteVerified":false,"pushVerified":false,"conditionalWriteSafe":false,"hiveWriteProven":false,"observationTrust":"operator-asserted","commit":snapshot.commit,"parserSha256":snapshot.parser_sha256,"sourceRepoId":source_repo_id,"captureId":handoff.capture_id,"accountKey":handoff.account_key,"listId":handoff.list_id,"providerId":handoff.provider_id,"receipt":handoff.receipt});
    match evaluate(snapshot, source_repo_id, handoff, observation) {
        Ok(c) => {
            out["source"] = c.source;
            out["desiredDue"] = json!(c.desired);
            match c.decision {
                FirstDue::SetFirstDue => {
                    out["decision"] = json!("set_first_due");
                    out["patch"] = json!({"due":c.desired});
                }
                FirstDue::AlreadySatisfied => out["decision"] = json!("already_satisfied"),
                FirstDue::Conflict => {
                    out["reason"] =
                        json!("supplied remote due conflicts; rescheduling is not supported")
                }
            }
        }
        Err(e) => out["reason"] = json!(e.to_string()),
    }
    out
}
