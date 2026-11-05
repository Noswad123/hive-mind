//! Explicit disposable-task experiment, deliberately separate from production writers.
use anyhow::{Result, anyhow, ensure};
use clap::{Args, Subcommand};
use hive_capture::{GoogleTask, handoff::validate_open_task};
use hive_google_auth::{
    AuthProfile, AuthorizationGrant, ClientConfig, CredentialStore, GoogleIdentity, OAuthTransport,
    SecretString, TokenSet, google::GoogleOAuth, provider::Authenticator,
    storage::MacKeychainStore, validate_connection,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};

#[path = "tasks_probe_http.rs"]
mod http;
#[path = "tasks_probe_journal.rs"]
mod journal;
use http::{Http, Response, strong_etag};
use journal::{Journal, RECORD_LIMIT};

#[derive(Debug, Subcommand)]
pub enum ProbeCommand {
    /// Exact authenticated read; unsigned evidence, never execution authority.
    Inspect(Selector),
    /// At most two due-only PATCHes. Leaves chosen due on success; never restores.
    Run(Run),
}

#[derive(Debug, Args, Serialize)]
pub struct Selector {
    #[arg(long)]
    connection: String,
    #[arg(long)]
    expected_account_key: String,
    #[arg(long)]
    list_id: String,
    #[arg(long)]
    task_id: String,
    #[arg(long)]
    journal: PathBuf,
}

#[derive(Debug, Args, Serialize)]
pub struct Run {
    #[command(flatten)]
    selector: Selector,
    #[arg(long)]
    expected_etag: String,
    #[arg(long)]
    expected_original_due: String,
    #[arg(long)]
    chosen_due: String,
    #[arg(long)]
    expected_baseline_sha256: String,
    #[arg(long)]
    acknowledge_disposable: bool,
}

fn hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn date(s: &str) -> bool {
    let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]").unwrap();
    s.len() == 10
        && time::Date::parse(s, &format).is_ok_and(|d| d.year() >= 1 && d.to_string() == s)
}

impl ProbeCommand {
    fn selector(&self) -> &Selector {
        match self {
            Self::Inspect(s) => s,
            Self::Run(r) => &r.selector,
        }
    }
    fn validate(&self) -> Result<()> {
        let s = self.selector();
        validate_connection(&s.connection)?;
        ensure!(
            s.expected_account_key
                .strip_prefix("google-sub-v1:")
                .is_some_and(hex),
            "expected Google account namespace required"
        );
        for id in [&s.list_id, &s.task_id] {
            ensure!(
                !id.is_empty()
                    && id.len() <= 256
                    && id.trim() == id.as_str()
                    && id.as_str() != "@default"
                    && !id.contains('.')
                    && !id.chars().any(char::is_control),
                "invalid exact task/list ID"
            );
        }
        if let Self::Run(r) = self {
            ensure!(
                r.acknowledge_disposable,
                "--acknowledge-disposable required; acknowledgment does not prove disposability"
            );
            ensure!(
                strong_etag(&r.expected_etag),
                "expected strong quoted ETag required"
            );
            ensure!(
                date(&r.expected_original_due) && date(&r.chosen_due),
                "exact YYYY-MM-DD dates required"
            );
            ensure!(
                r.expected_original_due != r.chosen_due,
                "chosen due must differ from original due"
            );
            ensure!(
                hex(&r.expected_baseline_sha256),
                "expected baseline SHA256 required"
            );
        }
        journal::validate_path(&s.journal)
    }
}

// Journal each OAuth request separately; exchange/login is deliberately unavailable.
struct LoggedOAuth<'a, F> {
    factory: &'a F,
    journal: &'a Journal,
}
impl<F, T> OAuthTransport for LoggedOAuth<'_, F>
where
    F: Fn(Duration) -> hive_google_auth::Result<T> + Send + Sync,
    T: OAuthTransport,
{
    fn exchange(
        &self,
        _: &ClientConfig,
        _: &AuthorizationGrant,
    ) -> hive_google_auth::Result<TokenSet> {
        Err(hive_google_auth::AuthError::Denied)
    }
    fn refresh(
        &self,
        client: &ClientConfig,
        token: &SecretString,
    ) -> hive_google_auth::Result<TokenSet> {
        self.journal
            .request("authRefresh", |remaining| {
                let transport = (self.factory)(remaining.min(Duration::from_secs(30)))?;
                self.journal.remaining()?;
                transport.refresh(client, token).map_err(Into::into)
            })
            .map_err(|_| hive_google_auth::AuthError::Transport)
    }
    fn identity(&self, token: &SecretString) -> hive_google_auth::Result<GoogleIdentity> {
        self.journal
            .request("authUserInfo", |remaining| {
                let transport = (self.factory)(remaining.min(Duration::from_secs(30)))?;
                self.journal.remaining()?;
                transport.identity(token).map_err(Into::into)
            })
            .map_err(|_| hive_google_auth::AuthError::Transport)
    }
}

pub(super) fn command(command: ProbeCommand) -> Result<Value> {
    command.validate()?;
    let journal = Journal::create(&command.selector().journal)?;
    freeze(&command, &journal)?;
    // Native dependencies are constructed only after all preflight and durable binding.
    let result = (|| {
        let store = MacKeychainStore::for_profile(AuthProfile::TasksWrite)
            .map_err(|_| anyhow!("inconclusive: TasksWrite credentials unavailable"))?;
        let oauth = GoogleOAuth::new().map_err(|_| anyhow!("inconclusive: OAuth unavailable"))?;
        collect(
            &command,
            &journal,
            &store,
            &|remaining| oauth.with_request_timeout(remaining),
            &Http::google()?,
        )
    })();
    finish(result, &journal)
}

fn freeze(command: &ProbeCommand, journal: &Journal) -> Result<()> {
    journal.record(json!({"event":"frozenBinding", "mode":if matches!(command, ProbeCommand::Run(_)) {"run"} else {"inspect"},
        "binding":command.selector(), "approval": match command {ProbeCommand::Run(r) => serde_json::to_value(r)?, _ => Value::Null},
        "warning":"process-crash evidence only, no power-loss guarantee; no resume/replay; acknowledgment does not prove disposability",
        "budget":"120-second admission/network budget, not a hard wall-clock bound on native Keychain UI or filesystem sync"}))
}

fn finish(result: Result<Value>, journal: &Journal) -> Result<Value> {
    let result = result.and_then(|report| {
        ensure!(
            serde_json::to_string_pretty(&report)?.len() <= RECORD_LIMIT - 1024,
            "unsupported: report size limit; stop"
        );
        journal.remaining()?;
        // A sync may overrun the budget: this is a candidate, not publication.
        journal.record(json!({"event":"resultCandidate","notPublication":true,"report":report}))?;
        journal.remaining()?;
        Ok(report)
    });
    match result {
        Ok(report) => Ok(report),
        Err(error) => {
            // All errors below are static, never raw provider errors, URLs, or secrets.
            let reason = error.to_string();
            let classification = if reason.starts_with("failed:") {
                "failed"
            } else if reason.starts_with("unsupported:") {
                "unsupported"
            } else {
                "inconclusive"
            };
            journal.record(json!({"event":"stopped", "result":classification, "reason":reason, "noResume":true}))?;
            Err(error)
        }
    }
}

fn collect<F, T>(
    command: &ProbeCommand,
    journal: &Journal,
    store: &dyn CredentialStore,
    oauth: &F,
    http: &Http,
) -> Result<Value>
where
    F: Fn(Duration) -> hive_google_auth::Result<T> + Send + Sync,
    T: OAuthTransport,
{
    ensure!(
        store.profile() == AuthProfile::TasksWrite,
        "inconclusive: TasksWrite profile required"
    );
    let s = command.selector();
    let logged = LoggedOAuth {
        factory: oauth,
        journal,
    };
    let bound = Authenticator::new(store, &logged)
        .access_token(&s.connection)
        .map_err(|_| {
            anyhow!("inconclusive: fresh saved-connection authorization failed; no fallback")
        })?;
    ensure!(
        bound
            .identity
            .namespace()
            .map_err(|_| anyhow!("inconclusive: invalid account namespace"))?
            == s.expected_account_key,
        "failed: expected account namespace mismatch; no Tasks requests"
    );
    execute(command, journal, http, bound.token.expose())
}

fn digest(task: &Value) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(task)?)))
}

struct Task {
    resource: Value,
    semantic: Value,
    etag: String,
    due: String,
}

fn task(response: Response, id: &str) -> Result<Task> {
    ensure!(
        response.status == 200,
        "inconclusive: exact task request did not return 200; stop"
    );
    let resource = response.json()?;
    // These are mandatory even though the shared read representation defaults them.
    for field in ["id", "title", "status"] {
        ensure!(
            resource.get(field).is_some_and(Value::is_string),
            "unsupported: mandatory task strings missing"
        );
    }
    let parsed: GoogleTask = serde_json::from_value(resource.clone())
        .map_err(|_| anyhow!("unsupported: invalid task shape"))?;
    ensure!(parsed.id == id, "failed: exact task identity mismatch");
    validate_open_task(&parsed)
        .map_err(|_| anyhow!("unsupported: requires open top-level title-only task"))?;
    ensure!(
        !parsed.title.trim().is_empty(),
        "unsupported: empty task title"
    );
    let etag = parsed
        .etag
        .ok_or_else(|| anyhow!("unsupported: missing task ETag"))?;
    ensure!(strong_etag(&etag), "unsupported: strong task ETag required");
    ensure!(
        response.etag.as_ref().is_none_or(|h| h == &etag),
        "inconclusive: HTTP/resource ETag mismatch"
    );
    if let Some(updated) = parsed.updated {
        ensure!(
            updated.len() <= 64
                && time::OffsetDateTime::parse(
                    &updated,
                    &time::format_description::well_known::Rfc3339
                )
                .is_ok(),
            "unsupported: invalid updated metadata"
        );
    }
    let due = parsed
        .due
        .ok_or_else(|| anyhow!("unsupported: already dated task required"))?;
    let day = due.get(..10).unwrap_or("");
    ensure!(
        date(day) && (due == format!("{day}T00:00:00.000Z") || due == format!("{day}T00:00:00Z")),
        "unsupported: valid midnight UTC due required"
    );
    // Only ETag and validated updated metadata may vary, besides the chosen due.
    // Raw resource hashing retains absent-vs-present fields and all provider metadata.
    let mut semantic = resource.clone();
    let object = semantic
        .as_object_mut()
        .ok_or_else(|| anyhow!("unsupported: task object required"))?;
    for key in ["etag", "updated", "due"] {
        object.remove(key);
    }
    Ok(Task {
        resource,
        semantic,
        etag,
        due: day.to_owned(),
    })
}

fn execute(command: &ProbeCommand, journal: &Journal, http: &Http, token: &str) -> Result<Value> {
    let s = command.selector();
    let request = |stage, patch| {
        journal.request(stage, |remaining| {
            let response = http.request(token, &s.list_id, &s.task_id, patch, remaining)?;
            journal
                .record(json!({"event":"httpStatus", "stage":stage, "status":response.status}))?;
            Ok(response)
        })
    };
    let baseline = task(request("baselineGET", None)?, &s.task_id)?;
    let baseline_digest = digest(&baseline.resource)?;
    let mut report = json!({"kind":"tasks-conditional-probe", "origin":http.origin.label(),
        "portableEvidenceAuthenticated":false,"executionCapability":false,"productionReady":false,
        "reviewFieldsPrivate":true,"replayAllowed":false,
        "accountKey":s.expected_account_key,"listId":s.list_id,"taskId":s.task_id,
        "originalDue":baseline.due,"originalEtag":baseline.etag,"baselineSha256":baseline_digest,
        "reviewResource":baseline.resource});
    let ProbeCommand::Run(r) = command else {
        report["result"] = json!("inspectionOnlyUnsignedEvidence");
        return Ok(report);
    };
    ensure!(
        baseline.etag == r.expected_etag
            && baseline.due == r.expected_original_due
            && baseline_digest == r.expected_baseline_sha256,
        "failed: approved baseline ETag/due/digest drift; no PATCH"
    );
    let changed = task(
        request("matchingPATCH", Some((&baseline.etag, &r.chosen_due)))?,
        &s.task_id,
    )?;
    ensure!(
        changed.due == r.chosen_due && changed.semantic == baseline.semantic,
        "failed: matching PATCH changed unexpected fields or due; stop"
    );
    let confirmed = task(request("changedGET", None)?, &s.task_id)?;
    ensure!(
        confirmed.due == r.chosen_due
            && confirmed.semantic == baseline.semantic
            && confirmed.etag != baseline.etag
            && confirmed.etag == changed.etag,
        "inconclusive: changed task/ETag drift; stop"
    );
    let stale = request(
        "stalePATCH",
        Some((&baseline.etag, &r.expected_original_due)),
    )?;
    ensure!(
        !(200..300).contains(&stale.status),
        "failed: stale PATCH accepted (2xx), even if unchanged; no rollback; inspect manually"
    );
    ensure!(
        stale.status == 412,
        "inconclusive: stale PATCH did not return 412; stop"
    );
    let rejection = stale.json()?;
    ensure!(
        rejection
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_u64)
            == Some(412),
        "unsupported: expected Google error JSON envelope with code 412; stop"
    );
    let final_task = task(request("finalGET", None)?, &s.task_id)?;
    ensure!(
        final_task.due == r.chosen_due
            && final_task.semantic == baseline.semantic
            && final_task.etag == confirmed.etag,
        "inconclusive: final task/ETag drift; possible external edit"
    );
    report["result"] = json!("matchingAcceptedAndStaleRejected");
    report["googleConditionalBehaviorObserved"] = json!(http.origin.google());
    report["chosenDueLeftInPlace"] = json!(r.chosen_due);
    report["finalEtag"] = json!(final_task.etag);
    Ok(report)
}

#[cfg(test)]
#[path = "tasks_probe_tests.rs"]
mod tests;
