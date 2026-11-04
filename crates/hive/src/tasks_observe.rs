use super::{TaskObservation, read_json};
use anyhow::{Result, anyhow, ensure};
use hive_capture::{
    GoogleTask,
    google::GoogleTasksClient,
    handoff::{MAX_JSON_BYTES, TaskHandoff, validate_open_task},
};
use hive_google_auth::{
    AuthProfile, CredentialStore, OAuthTransport, google::GoogleOAuth, provider::Authenticator,
    storage::MacKeychainStore, validate_connection,
};
use serde_json::json;
use std::path::Path;

pub(super) fn command(connection: &str, path: &Path) -> Result<String> {
    observe_with(connection, path, |handoff, digest| {
        let store = MacKeychainStore::for_profile(AuthProfile::TasksWrite)?;
        let transport = GoogleOAuth::new()?;
        collect(
            connection,
            handoff,
            digest,
            &store,
            &transport,
            |token, list, task| GoogleTasksClient::new(token)?.get_task(list, task),
        )
    })
}

// The factory cannot construct native storage until all file and tuple preflight succeeds.
fn observe_with(
    connection: &str,
    path: &Path,
    setup: impl FnOnce(&TaskHandoff, &str) -> Result<String>,
) -> Result<String> {
    validate_connection(connection)?;
    let (handoff, digest): (TaskHandoff, _) =
        read_json(path).map_err(|_| anyhow!("invalid or unavailable handoff input"))?;
    handoff
        .validate()
        .map_err(|_| anyhow!("invalid handoff evidence"))?;
    GoogleTasksClient::validate_task_ids(&handoff.list_id, &handoff.provider_id)?;
    setup(&handoff, &digest)
}

fn collect(
    connection: &str,
    handoff: &TaskHandoff,
    digest: &str,
    store: &dyn CredentialStore,
    transport: &dyn OAuthTransport,
    get_task: impl FnOnce(&str, &str, &str) -> Result<GoogleTask>,
) -> Result<String> {
    ensure!(
        store.profile() == AuthProfile::TasksWrite,
        "Tasks write-profile connection required"
    );
    // A publicly constructible BoundAccessToken is not authority: always run the
    // authenticator here, including its single refresh and fresh UserInfo check.
    let bound = Authenticator::new(store, transport).access_token(connection)?;
    ensure!(
        bound.identity.namespace()? == handoff.account_key,
        "handoff account binding mismatch"
    );
    let task = get_task(bound.token.expose(), &handoff.list_id, &handoff.provider_id)?;
    ensure!(
        task.id == handoff.provider_id,
        "observed task identity mismatch"
    );
    validate_open_task(&task).map_err(|_| anyhow!("unsupported observed task shape"))?;
    let observation = TaskObservation {
        schema_version: "1".into(),
        kind: "google-task-observation".into(),
        source_repo_id: handoff.source_repo_id.clone(),
        account_key: handoff.account_key.clone(),
        list_id: handoff.list_id.clone(),
        task,
    };
    let output = json!({
        "schemaVersion":"1", "kind":"google-task-live-observation",
        "sourceRepoId":handoff.source_repo_id, "captureId":handoff.capture_id,
        "accountKey":handoff.account_key, "listId":handoff.list_id,
        "providerId":handoff.provider_id, "handoffSha256":digest,
        "observedAt":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,
        "producer":"hive tasks observe",
        "method":"saved-connection-refresh+userinfo+exact-GET",
        "provenance":"Authenticated read in the producing execution only; portable unsigned JSON is not a trust capability. Explicitly extract observation for operator-asserted offline due-plan input.",
        "remoteReadVerifiedDuringCollection":true, "portableEvidenceAuthenticated":false,
        "publicationReady":false, "pushVerified":false, "conditionalWriteSafe":false,
        "hiveWriteProven":false, "observation":observation
    });
    let encoded = serde_json::to_string_pretty(&output)?;
    ensure!(
        encoded.len() < MAX_JSON_BYTES,
        "live observation output size limit exceeded"
    );
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_capture::{handoff::ReceiptEvidence, store::deterministic_capture_id};
    use hive_google_auth::{
        AuthError, AuthorizationGrant, ClientConfig, CredentialGuard, GoogleIdentity, GrantRecord,
        SecretString, StoredConnection, TokenSet,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    };

    const SUBJECT: &str = "raw-subject-canary";
    const ACCESS: &str = "bearer-canary";
    const REFRESH: &str = "refresh-canary";

    fn secret(value: &str) -> SecretString {
        SecretString::new(value).unwrap()
    }
    fn identity(subject: &str) -> GoogleIdentity {
        GoogleIdentity::new(subject).unwrap()
    }
    fn handoff() -> TaskHandoff {
        let account = identity(SUBJECT).namespace().unwrap();
        let capture = deterministic_capture_id(&account, "list-one", "task-one").unwrap();
        TaskHandoff {
            schema_version: "1".into(),
            kind: "google-task-handoff".into(),
            source_repo_id: "fixture-repo".into(),
            capture_id: capture.clone(),
            account_key: account,
            list_id: "list-one".into(),
            provider_id: "task-one".into(),
            accepted_current_baseline: true,
            baseline: serde_json::from_value(
                json!({"id":"task-one", "title":"Baseline", "status":"needsAction"}),
            )
            .unwrap(),
            receipt: ReceiptEvidence {
                method: "todos-add".into(),
                sha256: "a".repeat(64),
                stable_id: capture,
                source_id: "note-one".into(),
                source_path: "inbox.md".into(),
                source_revision: format!("sha256:{}", "b".repeat(64)),
            },
        }
    }
    struct Guard;
    impl CredentialGuard for Guard {}
    struct Store {
        record: Mutex<Option<StoredConnection>>,
        error: Option<AuthError>,
        profile: AuthProfile,
        locks: AtomicUsize,
    }
    impl Store {
        fn new() -> Self {
            Self {
                record: Mutex::new(Some(StoredConnection {
                    version: 1,
                    connection: "work".into(),
                    identity: identity(SUBJECT),
                    generation: 1,
                    grant: Some(GrantRecord {
                        client: ClientConfig {
                            client_id: "fixture.apps.googleusercontent.com".into(),
                            client_secret: None,
                        },
                        refresh_token: secret(REFRESH),
                        scopes: AuthProfile::TasksWrite
                            .scopes()
                            .iter()
                            .map(|s| (*s).into())
                            .collect(),
                    }),
                })),
                error: None,
                profile: AuthProfile::TasksWrite,
                locks: AtomicUsize::new(0),
            }
        }
    }
    impl CredentialStore for Store {
        fn profile(&self) -> AuthProfile {
            self.profile
        }
        fn lock(
            &self,
            connection: &str,
        ) -> hive_google_auth::Result<Box<dyn CredentialGuard + '_>> {
            assert_eq!(connection, "work");
            self.locks.fetch_add(1, SeqCst);
            if let Some(error) = self.error {
                return Err(error);
            }
            Ok(Box::new(Guard))
        }
        fn load(&self, _: &str) -> hive_google_auth::Result<Option<StoredConnection>> {
            Ok(self.record.lock().unwrap().clone())
        }
        fn save(&self, _: &str, record: &StoredConnection) -> hive_google_auth::Result<()> {
            *self.record.lock().unwrap() = Some(record.clone());
            Ok(())
        }
    }
    struct Transport {
        subject: &'static str,
        error: Option<AuthError>,
        refreshes: AtomicUsize,
        identities: AtomicUsize,
    }
    impl Transport {
        fn new() -> Self {
            Self {
                subject: SUBJECT,
                error: None,
                refreshes: AtomicUsize::new(0),
                identities: AtomicUsize::new(0),
            }
        }
    }
    impl OAuthTransport for Transport {
        fn exchange(
            &self,
            _: &ClientConfig,
            _: &AuthorizationGrant,
        ) -> hive_google_auth::Result<TokenSet> {
            panic!("observe must never sign in or exchange an authorization code")
        }
        fn refresh(
            &self,
            _: &ClientConfig,
            refresh: &SecretString,
        ) -> hive_google_auth::Result<TokenSet> {
            assert_eq!(refresh.expose(), REFRESH);
            assert_eq!(self.refreshes.fetch_add(1, SeqCst), 0, "no retries");
            if let Some(error) = self.error {
                return Err(error);
            }
            Ok(TokenSet {
                access_token: secret(ACCESS),
                refresh_token: None,
                expires_in: 3600,
                scopes: Some(
                    AuthProfile::TasksWrite
                        .scopes()
                        .iter()
                        .map(|s| (*s).into())
                        .collect(),
                ),
            })
        }
        fn identity(&self, token: &SecretString) -> hive_google_auth::Result<GoogleIdentity> {
            assert_eq!(token.expose(), ACCESS);
            assert_eq!(self.identities.fetch_add(1, SeqCst), 0);
            Ok(identity(self.subject))
        }
    }
    fn no_get(_: &str, _: &str, _: &str) -> Result<GoogleTask> {
        panic!("unexpected Tasks request")
    }
    fn assert_redacted(text: &str) {
        for canary in [SUBJECT, ACCESS, REFRESH, "other-raw-subject"] {
            assert!(!text.contains(canary), "secret leaked");
        }
    }

    #[tokio::test]
    async fn async_dispatch_collects_with_fresh_verified_bearer_and_real_blocking_clients() {
        let runtime_thread = std::thread::current().id();
        super::super::dispatch(move || {
            assert_ne!(std::thread::current().id(), runtime_thread);
            // These production constructors build real reqwest::blocking clients.
            // Build and drop both inside the dispatch boundary; never send a request.
            let _oauth_client = GoogleOAuth::new()?;
            let _tasks_client = GoogleTasksClient::new(ACCESS)?;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("handoff.json");
            let handoff = handoff();
            let bytes = serde_json::to_vec_pretty(&handoff).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let store = Store::new();
            let transport = Transport::new();
            let output = observe_with("work", &path, |h, digest| {
                // Removing the file proves neither digest nor collection reopens it.
                std::fs::remove_file(&path).unwrap();
                collect(
                    "work",
                    h,
                    digest,
                    &store,
                    &transport,
                    |token, list, task| {
                        assert_eq!((token, list, task), (ACCESS, "list-one", "task-one"));
                        assert_eq!(transport.identities.load(SeqCst), 1);
                        let mut task = h.baseline.clone();
                        task.title = "Changed baseline is still a valid read".into();
                        Ok(task)
                    },
                )
            })
            .unwrap();
            assert!(output.len() < MAX_JSON_BYTES);
            assert_redacted(&output);
            let value: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(value["kind"], "google-task-live-observation");
            assert_eq!(
                value["handoffSha256"],
                format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&bytes))
            );
            assert_eq!(value["accountKey"], handoff.account_key);
            assert_eq!(value["captureId"], handoff.capture_id);
            assert_eq!(value["remoteReadVerifiedDuringCollection"], true);
            for flag in [
                "portableEvidenceAuthenticated",
                "publicationReady",
                "pushVerified",
                "conditionalWriteSafe",
                "hiveWriteProven",
            ] {
                assert_eq!(value[flag], false);
            }
            time::OffsetDateTime::parse(
                value["observedAt"].as_str().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            assert!(serde_json::from_value::<TaskObservation>(value.clone()).is_err());
            assert!(
                serde_json::from_value::<TaskObservation>(value["observation"].clone()).is_ok()
            );
            assert_eq!(transport.refreshes.load(SeqCst), 1);
            Ok(())
        })
        .await
        .unwrap();
    }

    #[test]
    fn invalid_inputs_never_construct_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handoff.json");
        let no_setup =
            |_: &TaskHandoff, _: &str| -> Result<String> { panic!("credential factory called") };
        for bytes in [
            b"not JSON raw-subject-canary".to_vec(),
            vec![b' '; MAX_JSON_BYTES + 1],
            b"{}".to_vec(),
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert_redacted(
                &observe_with("work", &path, no_setup)
                    .unwrap_err()
                    .to_string(),
            );
        }
        for case in ["tuple", "baseline", "ids", "account"] {
            let mut h = handoff();
            match case {
                "tuple" => h.provider_id = "wrong".into(),
                "baseline" => h.baseline.status = "completed".into(),
                "account" => h.account_key = SUBJECT.into(),
                _ => {
                    h.list_id = "..".into();
                    h.capture_id =
                        deterministic_capture_id(&h.account_key, &h.list_id, &h.provider_id)
                            .unwrap();
                    h.receipt.stable_id = h.capture_id.clone();
                }
            }
            std::fs::write(&path, serde_json::to_vec(&h).unwrap()).unwrap();
            assert!(observe_with("work", &path, no_setup).is_err(), "{case}");
        }
        std::fs::write(&path, serde_json::to_vec(&handoff()).unwrap()).unwrap();
        for name in ["", "../bad", &"x".repeat(65)] {
            assert!(observe_with(name, &path, no_setup).is_err());
        }
        assert!(observe_with("work", Path::new("relative.json"), no_setup).is_err());
        assert!(observe_with("work", dir.path(), no_setup).is_err());
        assert!(observe_with("work", &dir.path().join("missing"), no_setup).is_err());
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(observe_with("work", &link, no_setup).is_err());
        }
    }

    #[test]
    fn credential_failures_and_wrong_subjects_never_reach_tasks() {
        for case in [
            "missing",
            "logout",
            "locked",
            "denied",
            "busy",
            "refresh",
            "userinfo",
            "handoff-account",
            "readonly",
        ] {
            let mut store = Store::new();
            let mut transport = Transport::new();
            let mut h = handoff();
            match case {
                "missing" => *store.record.lock().unwrap() = None,
                "logout" => store.record.lock().unwrap().as_mut().unwrap().grant = None,
                "locked" => store.error = Some(AuthError::StorageLocked),
                "denied" => store.error = Some(AuthError::StorageDenied),
                "busy" => store.error = Some(AuthError::Busy),
                "refresh" => transport.error = Some(AuthError::ReauthenticationRequired),
                "userinfo" => transport.subject = "other-raw-subject",
                "handoff-account" => {
                    h.account_key = identity("other-raw-subject").namespace().unwrap()
                }
                _ => store.profile = AuthProfile::TasksReadonly,
            }
            let error = collect("work", &h, "digest", &store, &transport, no_get).unwrap_err();
            assert_redacted(&format!("{error:#}"));
            let refreshed = matches!(case, "refresh" | "userinfo" | "handoff-account");
            assert_eq!(
                transport.refreshes.load(SeqCst),
                usize::from(refreshed),
                "{case}"
            );
            assert_eq!(
                transport.identities.load(SeqCst),
                usize::from(matches!(case, "userinfo" | "handoff-account")),
                "{case}"
            );
        }
    }

    #[test]
    fn wrong_task_unsupported_task_and_oversized_output_fail_closed() {
        for case in ["id", "completed", "unknown", "size", "provider-error"] {
            let store = Store::new();
            let transport = Transport::new();
            let h = handoff();
            let error = collect("work", &h, "digest", &store, &transport, |_, _, _| {
                let mut task = h.baseline.clone();
                match case {
                    "id" => task.id = "wrong".into(),
                    "completed" => task.status = "completed".into(),
                    "unknown" => {
                        task.extra.insert("futureField".into(), json!(true));
                    }
                    "size" => {
                        task.etag = Some("x".repeat(MAX_JSON_BYTES));
                        validate_open_task(&task).unwrap();
                    }
                    _ => return Err(anyhow!("Google Tasks request failed; no automatic retry")),
                }
                Ok(task)
            })
            .unwrap_err();
            if case == "size" {
                assert_eq!(
                    error.to_string(),
                    "live observation output size limit exceeded"
                );
            }
            assert_redacted(&format!("{error:#}"));
            assert_eq!(transport.refreshes.load(SeqCst), 1);
        }
    }
}
