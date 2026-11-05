//! Read-only hosted foundation. Consistent runner claims are not authenticated push
//! evidence, current remote main verification, or execution authority. The binding
//! must come from trusted provisioning; a repository file or environment alone is
//! not authentication. No portable JSON can be imported as a capability here.
use anyhow::{Result, anyhow, ensure};
use hive_calendar::task_snapshot::{CommittedTaskSnapshot, read_committed_tasks_pinned};
use hive_capture::{GoogleTask, google::GoogleTasksClient, handoff::TaskHandoff};
use hive_google_auth::{
    AuthError, OAuthTransport, google::GoogleOAuth, hosted::ImmutableTasksWriteStore,
    provider::Authenticator, validate_connection,
};
use serde::{
    Deserialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[cfg(test)]
#[path = "hosted_coordinator.rs"]
mod coordinator;
#[path = "hosted_github.rs"]
mod github;
#[cfg(test)]
#[path = "hosted_remote.rs"]
mod remote;

use crate::{
    policy::{self, FirstDue, TaskObservation},
    storage,
};

/// Only these explicit job context variables are read, never a generic bearer.
pub struct PushClaims {
    actions: String,
    event_name: String,
    git_ref: String,
    sha: String,
    repository: String,
    repository_id: String,
    event_path: PathBuf,
}
impl PushClaims {
    pub fn from_environment(mut read: impl FnMut(&str) -> Option<String>) -> Result<Self> {
        let mut get = |key| read(key).ok_or_else(|| anyhow!("invalid_push_context"));
        Ok(Self {
            actions: get("GITHUB_ACTIONS")?,
            event_name: get("GITHUB_EVENT_NAME")?,
            git_ref: get("GITHUB_REF")?,
            sha: get("GITHUB_SHA")?,
            repository: get("GITHUB_REPOSITORY")?,
            repository_id: get("GITHUB_REPOSITORY_ID")?,
            event_path: get("GITHUB_EVENT_PATH")?.into(),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    schema_version: String,
    kind: String,
    expected_repository_id: String,
    expected_repository: String,
    source_repo_id: String,
    connection: String,
    expected_account_namespace: String,
    trusted_parser_sha256: String,
    reviewed_task_handoff: TaskHandoff,
    handoff_sha256: String,
}

// Private, check-only capability. It is never serialized, exported or used by a writer.
struct AdmittedPush {
    commit: String,
}

pub struct Inputs<'a> {
    pub binding: &'a Path,
    pub repo: &'a Path,
    pub mw_bin: &'a Path,
    pub commit: &'a str,
}

fn hex(s: &str, size: usize) -> bool {
    s.len() == size
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && hex(s, s.len()) && s.bytes().any(|b| b != b'0')
}
fn numeric_id(s: &str) -> bool {
    !s.starts_with('0') && s.parse::<u64>().is_ok_and(|n| n > 0 && n.to_string() == s)
}
fn repository(s: &str) -> bool {
    let parts: Vec<_> = s.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 100
                && *p != "."
                && *p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

fn read_bytes(path: &Path, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        path.is_absolute() && path.as_os_str().len() <= 4096,
        "invalid_input"
    );
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(file.metadata()?.is_file(), "invalid_input");
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "invalid_input");
    Ok(bytes)
}

// Value's default decoder silently overwrites duplicate keys. Reject them at every
// depth, including unknown event fields, before decoding security-relevant fields.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("unique-key JSON")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut m: M,
            ) -> std::result::Result<Unique, M::Error> {
                let mut out = serde_json::Map::new();
                while let Some((key, Unique(value))) = m.next_entry::<String, Unique>()? {
                    if out.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate key"));
                    }
                }
                Ok(Unique(Value::Object(out)))
            }
            fn visit_seq<S: SeqAccess<'de>>(
                self,
                mut s: S,
            ) -> std::result::Result<Unique, S::Error> {
                let mut out = Vec::new();
                while let Some(Unique(v)) = s.next_element()? {
                    out.push(v);
                }
                Ok(Unique(Value::Array(out)))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
        }
        d.deserialize_any(V)
    }
}
fn unique(bytes: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice::<Unique>(bytes)?.0)
}

impl Binding {
    fn read(path: &Path) -> Result<Self> {
        let value = unique(&read_bytes(path, 128 * 1024)?)?;
        let binding: Self = serde_json::from_value(value)?;
        ensure!(
            binding.schema_version == "1"
                && binding.kind == "google-tasks-hosted-binding"
                && numeric_id(&binding.expected_repository_id)
                && repository(&binding.expected_repository)
                && hex(&binding.trusted_parser_sha256, 64),
            "invalid_binding"
        );
        validate_connection(&binding.connection)?;
        ensure!(
            binding
                .expected_account_namespace
                .strip_prefix("google-sub-v1:")
                .is_some_and(|s| hex(s, 64)),
            "invalid_binding"
        );
        let h = &binding.reviewed_task_handoff;
        h.validate()?;
        GoogleTasksClient::validate_task_ids(&h.list_id, &h.provider_id)?;
        ensure!(
            h.list_id != "@default" && !h.list_id.contains('.') && !h.provider_id.contains('.'),
            "invalid_binding"
        );
        // Canonical digest: compact serde_json Value encoding, recursively sorted keys.
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&serde_json::to_value(h)?)?)
        );
        ensure!(
            h.source_repo_id == binding.source_repo_id
                && h.account_key == binding.expected_account_namespace
                && binding.handoff_sha256 == digest,
            "invalid_binding"
        );
        Ok(binding)
    }
    fn admit(&self, claims: PushClaims, commit: &str) -> Result<AdmittedPush> {
        ensure!(
            claims.actions == "true"
                && claims.event_name == "push"
                && claims.git_ref == "refs/heads/main"
                && oid(commit)
                && claims.sha == commit
                && claims.repository == self.expected_repository
                && claims.repository_id == self.expected_repository_id,
            "invalid_push_claims"
        );
        let e = unique(&read_bytes(&claims.event_path, 1024 * 1024)?)?;
        ensure!(
            e["ref"] == "refs/heads/main"
                && e["deleted"] == false
                && e["after"] == commit
                && e["repository"]["id"]
                    .as_u64()
                    .is_some_and(|n| n.to_string() == self.expected_repository_id)
                && e["repository"]["full_name"] == self.expected_repository,
            "invalid_push_event"
        );
        Ok(AdmittedPush {
            commit: commit.into(),
        })
    }
}

fn parser_hash(path: &Path) -> Result<String> {
    storage::path(path, true)?;
    Ok(format!(
        "{:x}",
        Sha256::digest(read_bytes(path, 256 * 1024 * 1024)?)
    ))
}
fn head(repo: &Path) -> Result<String> {
    storage::path(repo, true)?;
    storage::path(&repo.join(".git"), true)?;
    ensure!(
        repo.join(".git").is_dir()
            && storage::git(repo, &["rev-parse", "--absolute-git-dir"], b"")?
                == repo
                    .join(".git")
                    .to_str()
                    .ok_or_else(|| anyhow!("invalid_source"))?
            && !repo.join(".git/shallow").exists()
            && !repo.join(".git/objects/info/alternates").exists()
            && storage::git(repo, &["rev-parse", "--is-shallow-repository"], b"")? == "false",
        "incomplete_source"
    );
    storage::git(repo, &["rev-parse", "--verify", "HEAD^{commit}"], b"")
}
fn observation(h: &TaskHandoff, task: GoogleTask) -> TaskObservation {
    TaskObservation {
        schema_version: "1".into(),
        kind: "google-task-observation".into(),
        source_repo_id: h.source_repo_id.clone(),
        account_key: h.account_key.clone(),
        list_id: h.list_id.clone(),
        task,
    }
}
fn source(
    inputs: &Inputs<'_>,
    b: &Binding,
    admitted: &AdmittedPush,
) -> Result<CommittedTaskSnapshot> {
    ensure!(
        inputs.repo.is_absolute()
            && inputs.mw_bin.is_absolute()
            && head(inputs.repo)? == admitted.commit,
        "source_rejected"
    );
    storage::path(inputs.mw_bin, true)?;
    let snapshot = read_committed_tasks_pinned(
        inputs.repo,
        &admitted.commit,
        inputs.mw_bin,
        &b.trusted_parser_sha256,
    )?;
    ensure!(
        snapshot.commit == admitted.commit && snapshot.parser_sha256 == b.trusted_parser_sha256,
        "source_rejected"
    );
    let h = &b.reviewed_task_handoff;
    policy::evaluate(
        &snapshot,
        &b.source_repo_id,
        h,
        &observation(h, h.baseline.clone()),
    )?;
    unchanged(inputs, b, admitted)?;
    Ok(snapshot)
}
fn unchanged(inputs: &Inputs<'_>, b: &Binding, a: &AdmittedPush) -> Result<()> {
    ensure!(
        head(inputs.repo)? == a.commit && parser_hash(inputs.mw_bin)? == b.trusted_parser_sha256,
        "source_changed"
    );
    Ok(())
}

// Hosted orchestration receives only this GET-only facade, not Http::request's
// optional PATCH parameter. No hosted API accepts an endpoint or write callback.
mod readonly_http {
    use super::*;
    use crate::publisher_http::{Http, Resource};
    pub(super) struct ExactGet(Http);
    impl ExactGet {
        pub(super) fn google() -> Result<Self> {
            Ok(Self(Http::google()?))
        }
        #[cfg(test)]
        pub(super) fn loopback(base: &str) -> Self {
            Self(Http::loopback(base))
        }
        pub(super) fn is_google(&self) -> bool {
            self.0.google
        }
        pub(super) fn get(
            &self,
            token: &str,
            h: &TaskHandoff,
            remaining: Duration,
        ) -> Result<Resource> {
            self.0
                .request(token, &h.list_id, &h.provider_id, None, remaining)?
                .resource(&h.provider_id)
        }
    }
}
use readonly_http::ExactGet;

fn summary(remote: bool, decision: &str, origin_google: bool) -> Value {
    json!({"schemaVersion":"1", "kind":"google-tasks-hosted-summary", "redacted":true,
        "decision":decision, "secretPresent":true, "structureValid":true,
        "pushClaimsAdmitted":true, "currentMainVerified":false, "pushVerified":false,
        "githubVerified":false, "remoteReadVerifiedDuringCollection":remote,
        "googleOrigin":origin_google, "portableEvidenceAuthenticated":false,
        "publicationReady":false, "conditionalWriteSafe":false, "hiveWriteProven":false,
        "hostedExecutionEnabled":false})
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| anyhow!("budget_expired"))
}
fn auth_error(error: AuthError) -> anyhow::Error {
    if error == AuthError::CredentialPersistenceRequired {
        anyhow!("auth_action_required")
    } else {
        anyhow!("authentication_failed")
    }
}

/// Local admission and source preflight precede this one secret callback. Check
/// validates structure only, never refreshes or constructs an HTTP client.
pub fn check(
    inputs: Inputs<'_>,
    claims: PushClaims,
    secret: impl FnOnce() -> Result<Zeroizing<String>>,
) -> Result<Value> {
    run(inputs, claims, secret, false)
}
/// Fresh refresh + UserInfo + exact bound task GET; emits no observation/plan body.
pub fn plan(
    inputs: Inputs<'_>,
    claims: PushClaims,
    secret: impl FnOnce() -> Result<Zeroizing<String>>,
) -> Result<Value> {
    run(inputs, claims, secret, true)
}

/// Authenticated current-main observation only, not original push provenance or
/// permission to dispatch. Admission and the pinned parser precede token lookup.
pub fn verify_main(
    inputs: Inputs<'_>,
    claims: PushClaims,
    token: impl FnOnce() -> Result<Zeroizing<String>>,
) -> Result<Value> {
    verify_main_with(inputs, claims, token, github::GitHub::production)
}

fn verify_main_with(
    inputs: Inputs<'_>,
    claims: PushClaims,
    token: impl FnOnce() -> Result<Zeroizing<String>>,
    transport: impl FnOnce() -> Result<github::GitHub>,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let b = Binding::read(inputs.binding).map_err(|_| anyhow!("invalid_binding"))?;
    let a = b
        .admit(claims, inputs.commit)
        .map_err(|_| anyhow!("push_admission_failed"))?;
    source(&inputs, &b, &a).map_err(|_| anyhow!("source_preflight_failed"))?;
    remaining(deadline)?;
    let token = token().map_err(|_| anyhow!("credential_unavailable"))?;
    unchanged(&inputs, &b, &a).map_err(|_| anyhow!("source_changed"))?;
    let http = transport().map_err(|_| anyhow!("github_verification_failed"))?;
    let current = http
        .verify(&b, &a.commit, &token, deadline)
        .map_err(|_| anyhow!("github_verification_failed"))?;
    unchanged(&inputs, &b, &a).map_err(|_| anyhow!("source_changed"))?;
    remaining(deadline)?;
    Ok(current.summary())
}
fn run(
    inputs: Inputs<'_>,
    claims: PushClaims,
    secret: impl FnOnce() -> Result<Zeroizing<String>>,
    read_remote: bool,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let b = Binding::read(inputs.binding).map_err(|_| anyhow!("invalid_binding"))?;
    let a = b
        .admit(claims, inputs.commit)
        .map_err(|_| anyhow!("push_admission_failed"))?;
    let snapshot = source(&inputs, &b, &a).map_err(|_| anyhow!("source_preflight_failed"))?;
    remaining(deadline)?;
    let bytes = secret().map_err(|_| anyhow!("credential_unavailable"))?;
    let store = ImmutableTasksWriteStore::from_record_bytes(
        &b.connection,
        &b.expected_account_namespace,
        bytes.as_bytes(),
    )
    .map_err(|_| anyhow!("credential_structure_invalid"))?;
    drop(bytes);
    if !read_remote {
        unchanged(&inputs, &b, &a).map_err(|_| anyhow!("source_changed"))?;
        remaining(deadline)?;
        return Ok(summary(false, "set_first_due", false));
    }
    unchanged(&inputs, &b, &a).map_err(|_| anyhow!("source_changed"))?;
    let oauth = GoogleOAuth::new()
        .map_err(auth_error)?
        .with_request_timeout(remaining(deadline)?.min(Duration::from_secs(30)))
        .map_err(auth_error)?;
    let http = ExactGet::google().map_err(|_| anyhow!("tasks_transport_failed"))?;
    collect(&inputs, &b, &a, &snapshot, &store, &oauth, &http, deadline)
}

#[allow(clippy::too_many_arguments)]
fn collect(
    inputs: &Inputs<'_>,
    b: &Binding,
    a: &AdmittedPush,
    snapshot: &CommittedTaskSnapshot,
    store: &ImmutableTasksWriteStore,
    oauth: &dyn OAuthTransport,
    http: &ExactGet,
    deadline: Instant,
) -> Result<Value> {
    remaining(deadline)?;
    unchanged(inputs, b, a).map_err(|_| anyhow!("source_changed"))?;
    let bound = Authenticator::new(store, oauth)
        .access_token(&b.connection)
        .map_err(auth_error)?;
    ensure!(
        bound
            .identity
            .namespace()
            .map_err(|_| anyhow!("account_mismatch"))?
            == b.expected_account_namespace,
        "account_mismatch"
    );
    unchanged(inputs, b, a).map_err(|_| anyhow!("source_changed"))?;
    let h = &b.reviewed_task_handoff;
    let resource = http
        .get(bound.token.expose(), h, remaining(deadline)?)
        .map_err(|_| anyhow!("exact_task_read_failed"))?;
    let task = resource
        .validate(&h.provider_id)
        .map_err(|_| anyhow!("exact_task_read_failed"))?;
    let candidate = policy::evaluate(snapshot, &b.source_repo_id, h, &observation(h, task));
    unchanged(inputs, b, a).map_err(|_| anyhow!("source_changed"))?;
    remaining(deadline)?;
    let decision = match candidate.map(|c| c.decision()) {
        Ok(FirstDue::SetFirstDue) => "set_first_due",
        Ok(FirstDue::AlreadySatisfied) => "already_satisfied",
        _ => "blocked",
    };
    Ok(summary(true, decision, http.is_google()))
}

#[cfg(test)]
#[path = "hosted_tests.rs"]
mod tests;
