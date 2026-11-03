use std::path::PathBuf;

use anyhow::{Result, bail};
use hive_capture::{google::GoogleTasksClient, store::deterministic_capture_id};
use hive_google_auth::{
    AuthError, AuthProfile, AuthorizationFlow, BoundAccessToken, ClientConfig, ConnectionStatus,
    CredentialStore, OAuthTransport, google::GoogleOAuth, loopback::LoopbackFlow,
    provider::Authenticator, storage::MacKeychainStore, validate_connection,
};

#[derive(Debug, clap::Args)]
#[group(id = "google_credentials", required = true, multiple = false)]
pub(crate) struct GoogleCredentialArgs {
    /// Explicit saved, account-bound Google connection; never falls back to an environment token.
    #[arg(long)]
    pub connection: Option<String>,
    /// Explicit HIVE_GOOGLE_ environment variable containing an access token.
    #[arg(long)]
    pub access_token_env: Option<String>,
}

#[derive(Debug, clap::Subcommand)]
pub enum CaptureAuthCommand {
    /// Explicitly sign in using an official Google Desktop OAuth client JSON file.
    Login {
        #[arg(long)]
        connection: String,
        #[arg(long)]
        client_json: PathBuf,
    },
    /// Inspect local credential presence only; does not contact Google or verify grant validity.
    Status {
        #[arg(long)]
        connection: String,
    },
    /// Clear local credentials, retaining the account binding; does not revoke Google's grant.
    Logout {
        #[arg(long)]
        connection: String,
    },
}

pub struct ResolvedGoogleClient {
    pub client: GoogleTasksClient,
    pub identity: Option<hive_core::GoogleIdentity>,
}

pub(crate) enum BackgroundCredentials {
    Ready {
        resolved: ResolvedGoogleClient,
        generation: u64,
    },
    NeedsAction {
        generation: Option<u64>,
        reason: &'static str,
    },
    Transient {
        reason: &'static str,
    },
}

pub(crate) fn background_client(
    connection: &str,
    expected_namespace: &str,
    paused_generation: Option<u64>,
) -> Result<BackgroundCredentials> {
    validate_connection(connection)?;
    let store = MacKeychainStore::new_background()?;
    let transport = GoogleOAuth::new()?;
    background_client_with(
        connection,
        expected_namespace,
        paused_generation,
        &store,
        &transport,
    )
}

fn credential_metadata(
    store: &dyn CredentialStore,
    connection: &str,
) -> hive_google_auth::Result<Option<(u64, String, bool)>> {
    let _guard = store.lock(connection)?;
    let Some(record) = store.load(connection)? else {
        return Ok(None);
    };
    let namespace = record
        .identity
        .namespace()
        .map_err(|_| AuthError::StorageCorrupt)?;
    Ok(Some((record.generation, namespace, record.grant.is_some())))
}

fn background_failure(error: AuthError, generation: Option<u64>) -> BackgroundCredentials {
    match error {
        AuthError::Busy => BackgroundCredentials::Transient {
            reason: "credential_busy",
        },
        AuthError::Transport | AuthError::Timeout => BackgroundCredentials::Transient {
            reason: "authentication_transport",
        },
        AuthError::StorageLocked | AuthError::StorageDenied => BackgroundCredentials::NeedsAction {
            generation,
            reason: "keychain_locked_or_denied",
        },
        _ => BackgroundCredentials::NeedsAction {
            generation,
            reason: "sign_in_required",
        },
    }
}

fn background_client_with(
    connection: &str,
    expected_namespace: &str,
    paused_generation: Option<u64>,
    store: &dyn CredentialStore,
    transport: &dyn OAuthTransport,
) -> Result<BackgroundCredentials> {
    validate_connection(connection)?;
    let metadata = match credential_metadata(store, connection) {
        Ok(metadata) => metadata,
        Err(error) => return Ok(background_failure(error, None)),
    };
    let Some((generation, namespace, present)) = metadata else {
        return Ok(BackgroundCredentials::NeedsAction {
            generation: None,
            reason: "not_connected",
        });
    };
    if !present || namespace != expected_namespace {
        return Ok(BackgroundCredentials::NeedsAction {
            generation: Some(generation),
            reason: "connection_binding_mismatch_or_logged_out",
        });
    }
    if paused_generation == Some(generation) {
        return Ok(BackgroundCredentials::NeedsAction {
            generation: Some(generation),
            reason: "sign_in_required",
        });
    }
    let attempt = Authenticator::new(store, transport).access_token_attempt(connection);
    match attempt.outcome {
        Ok(bound) => {
            if bound.identity.namespace()? != expected_namespace {
                return Ok(BackgroundCredentials::NeedsAction {
                    generation: Some(generation),
                    reason: "connection_binding_mismatch",
                });
            }
            let current = credential_metadata(store, connection)?;
            let Some((current_generation, current_namespace, true)) = current else {
                return Ok(BackgroundCredentials::NeedsAction {
                    generation: None,
                    reason: "not_connected",
                });
            };
            if current_namespace != expected_namespace {
                return Ok(BackgroundCredentials::NeedsAction {
                    generation: Some(current_generation),
                    reason: "connection_binding_mismatch",
                });
            }
            Ok(BackgroundCredentials::Ready {
                resolved: ResolvedGoogleClient {
                    client: GoogleTasksClient::new(bound.token.expose())?,
                    identity: Some(bound.identity),
                },
                generation: current_generation,
            })
        }
        Err(error) => Ok(background_failure(error, attempt.generation)),
    }
}

impl GoogleCredentialArgs {
    pub fn resolve_for_list(&self) -> Result<ResolvedGoogleClient> {
        self.resolve_with(None, false, native_access_token, read_token_env)
    }

    pub fn resolve_for_capture(&self, account_key: Option<&str>) -> Result<ResolvedGoogleClient> {
        self.resolve_with(account_key, true, native_access_token, read_token_env)
    }

    fn resolve_with(
        &self,
        account_key: Option<&str>,
        capture: bool,
        saved: impl FnOnce(&str) -> Result<BoundAccessToken>,
        environment: impl FnOnce(&str) -> Result<String>,
    ) -> Result<ResolvedGoogleClient> {
        match (&self.connection, &self.access_token_env) {
            (Some(connection), None) => {
                validate_connection(connection)?;
                if account_key.is_some() {
                    bail!("account-key is not allowed with a saved Google connection");
                }
                let bound = saved(connection)?;
                Ok(ResolvedGoogleClient {
                    client: GoogleTasksClient::new(bound.token.expose())?,
                    identity: Some(bound.identity),
                })
            }
            (None, Some(variable)) => {
                if !variable.starts_with("HIVE_GOOGLE_")
                    || !variable.bytes().enumerate().all(|(i, ch)| {
                        ch == b'_' || ch.is_ascii_alphabetic() || (i > 0 && ch.is_ascii_digit())
                    })
                {
                    bail!(
                        "access-token-env must name a HIVE_GOOGLE_ environment variable; credentials must not use the MindWeaver child environment"
                    );
                }
                if capture {
                    let account = account_key.ok_or_else(|| {
                        anyhow::anyhow!(
                            "account-key is required with an environment Google access token"
                        )
                    })?;
                    if account.starts_with("google-sub-v1:") {
                        bail!("account-key must not use the reserved Google identity namespace");
                    }
                    deterministic_capture_id(account, "preflight-list", "preflight-task")?;
                }
                let token = environment(variable)?;
                Ok(ResolvedGoogleClient {
                    client: GoogleTasksClient::new(&token)?,
                    identity: None,
                })
            }
            _ => bail!(
                "select exactly one of connection or access-token-env; no credential fallback"
            ),
        }
    }
}

fn read_token_env(variable: &str) -> Result<String> {
    std::env::var(variable).map_err(|_| {
        anyhow::anyhow!(
            "Google OAuth access token is unavailable in the selected environment variable"
        )
    })
}

fn native_access_token(connection: &str) -> Result<BoundAccessToken> {
    validate_connection(connection)?;
    let store = MacKeychainStore::new()?;
    let transport = GoogleOAuth::new()?;
    Ok(Authenticator::new(&store, &transport).access_token(connection)?)
}

// Preparation is pure except for the explicitly selected client file. It always
// completes before the production factory constructs native storage.
struct PreparedCommand {
    command: CaptureAuthCommand,
    client: Option<ClientConfig>,
}

impl PreparedCommand {
    fn new(command: CaptureAuthCommand) -> Result<Self> {
        let connection = match &command {
            CaptureAuthCommand::Login { connection, .. }
            | CaptureAuthCommand::Status { connection }
            | CaptureAuthCommand::Logout { connection } => connection,
        };
        validate_connection(connection)?;
        let client = match &command {
            CaptureAuthCommand::Login { client_json, .. } => {
                Some(ClientConfig::from_file(client_json)?)
            }
            _ => None,
        };
        Ok(Self { command, client })
    }

    fn execute(
        self,
        store: &dyn CredentialStore,
        transport: &dyn OAuthTransport,
        flow: &dyn AuthorizationFlow,
    ) -> Result<ConnectionStatus> {
        let auth = Authenticator::new(store, transport);
        Ok(match self.command {
            CaptureAuthCommand::Login { connection, .. } => auth.login(
                &connection,
                self.client.as_ref().expect("prepared login"),
                flow,
            )?,
            CaptureAuthCommand::Status { connection } => auth.status(&connection)?,
            CaptureAuthCommand::Logout { connection } => auth.logout(&connection)?,
        })
    }
}

pub fn auth_command(command: CaptureAuthCommand) -> Result<()> {
    auth_command_for(command, AuthProfile::TasksReadonly)
}

pub(crate) fn auth_command_for(command: CaptureAuthCommand, profile: AuthProfile) -> Result<()> {
    let prepared = PreparedCommand::new(command)?;
    let store = MacKeychainStore::for_profile(profile)?;
    let transport = GoogleOAuth::new()?;
    let flow = LoopbackFlow::for_profile(profile);
    let status = prepared.execute(&store, &transport, &flow)?;
    let json = serde_json::to_string_pretty(&status)
        .map_err(|_| anyhow::anyhow!("could not encode Google connection status"))?;
    use std::io::Write;
    writeln!(std::io::stdout().lock(), "{json}")
        .map_err(|_| anyhow::anyhow!("could not write Google connection status"))?;
    Ok(())
}

#[cfg(test)]
fn execute_with(
    command: CaptureAuthCommand,
    store: &dyn CredentialStore,
    transport: &dyn OAuthTransport,
    flow: &dyn AuthorizationFlow,
) -> Result<ConnectionStatus> {
    PreparedCommand::new(command)?.execute(store, transport, flow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_google_auth::{
        AuthError, AuthorizationGrant, CredentialGuard, GoogleIdentity, REQUIRED_SCOPES,
        SecretString, StoredConnection, TokenSet,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    };

    const ACCESS: &str = "fixture-access-canary";
    const REFRESH: &str = "fixture-refresh-canary";
    const SUBJECT: &str = "fixture-subject-canary";
    const CLIENT: &str = "fixture-client-secret-canary";
    const CODE: &str = "fixture-code-canary";
    const VERIFIER: &str = "fixture-verifier-canary";

    #[derive(Default)]
    struct Store {
        record: Mutex<Option<StoredConnection>>,
        held: AtomicBool,
        locks: AtomicUsize,
        fail_save: AtomicBool,
        login_on_drop: AtomicUsize,
    }

    struct Guard<'a>(&'a Store);
    impl CredentialGuard for Guard<'_> {}
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.held.store(false, SeqCst);
            if self
                .0
                .login_on_drop
                .fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1))
                == Ok(1)
            {
                // Deterministically install a foreground grant after the attempt
                // unlocks but before its caller can handle the failed outcome.
                let client = self
                    .0
                    .record
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .grant
                    .as_ref()
                    .unwrap()
                    .client
                    .clone();
                self.0.fail_save.store(false, SeqCst);
                Authenticator::new(self.0, &Transport::new(self.0))
                    .login("work", &client, &Flow(self.0))
                    .unwrap();
            }
        }
    }

    impl CredentialStore for Store {
        fn lock(&self, _: &str) -> hive_google_auth::Result<Box<dyn CredentialGuard + '_>> {
            assert!(!self.held.swap(true, SeqCst));
            self.locks.fetch_add(1, SeqCst);
            Ok(Box::new(Guard(self)))
        }
        fn load(&self, _: &str) -> hive_google_auth::Result<Option<StoredConnection>> {
            assert!(self.held.load(SeqCst));
            Ok(self.record.lock().unwrap().clone())
        }
        fn save(&self, _: &str, record: &StoredConnection) -> hive_google_auth::Result<()> {
            assert!(self.held.load(SeqCst));
            if self.fail_save.load(SeqCst) {
                return Err(AuthError::StorageUnavailable);
            }
            *self.record.lock().unwrap() = Some(record.clone());
            Ok(())
        }
    }

    struct Transport<'a> {
        store: &'a Store,
        profile: AuthProfile,
        exchanges: AtomicUsize,
        refreshes: AtomicUsize,
        identities: AtomicUsize,
        mismatch: bool,
        refresh_error: Option<AuthError>,
        identity_error: Option<AuthError>,
        rotate: bool,
    }
    impl<'a> Transport<'a> {
        fn new(store: &'a Store) -> Self {
            Self {
                store,
                profile: AuthProfile::TasksReadonly,
                exchanges: AtomicUsize::new(0),
                refreshes: AtomicUsize::new(0),
                identities: AtomicUsize::new(0),
                mismatch: false,
                refresh_error: None,
                identity_error: None,
                rotate: false,
            }
        }
        fn tokens(&self) -> TokenSet {
            assert!(self.store.held.load(SeqCst));
            TokenSet {
                access_token: secret(ACCESS),
                refresh_token: Some(secret(REFRESH)),
                expires_in: 3600,
                scopes: Some(self.profile.scopes().iter().map(|s| (*s).into()).collect()),
            }
        }
    }
    impl OAuthTransport for Transport<'_> {
        fn exchange(
            &self,
            client: &ClientConfig,
            grant: &AuthorizationGrant,
        ) -> hive_google_auth::Result<TokenSet> {
            assert_eq!(client.client_secret.as_ref().unwrap().expose(), CLIENT);
            assert_eq!(grant.code.expose(), CODE);
            assert_eq!(grant.verifier.expose(), VERIFIER);
            self.exchanges.fetch_add(1, SeqCst);
            Ok(self.tokens())
        }
        fn refresh(
            &self,
            _: &ClientConfig,
            refresh: &SecretString,
        ) -> hive_google_auth::Result<TokenSet> {
            assert_eq!(refresh.expose(), REFRESH);
            assert_eq!(
                self.refreshes.fetch_add(1, SeqCst),
                0,
                "unexpected refresh retry"
            );
            if let Some(error) = self.refresh_error {
                return Err(error);
            }
            let mut tokens = self.tokens();
            if self.rotate {
                tokens.refresh_token = Some(secret("fixture-rotated-refresh-canary"));
            }
            Ok(tokens)
        }
        fn identity(&self, token: &SecretString) -> hive_google_auth::Result<GoogleIdentity> {
            assert!(self.store.held.load(SeqCst));
            assert_eq!(token.expose(), ACCESS);
            self.identities.fetch_add(1, SeqCst);
            if let Some(error) = self.identity_error {
                return Err(error);
            }
            Ok(GoogleIdentity::new(if self.mismatch {
                "other-fixture-subject"
            } else {
                SUBJECT
            })
            .unwrap())
        }
    }

    struct Flow<'a>(&'a Store);
    impl AuthorizationFlow for Flow<'_> {
        fn authorize(&self, _: &ClientConfig) -> hive_google_auth::Result<AuthorizationGrant> {
            assert!(self.0.held.load(SeqCst));
            Ok(AuthorizationGrant {
                code: secret(CODE),
                verifier: secret(VERIFIER),
                redirect_uri: "http://127.0.0.1:1234/oauth2/callback".into(),
            })
        }
    }
    fn secret(value: &str) -> SecretString {
        SecretString::new(value).unwrap()
    }
    fn client_file() -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "{{\"installed\":{{\"client_id\":\"fixture.apps.googleusercontent.com\",\"client_secret\":\"{CLIENT}\",\"auth_uri\":\"https://accounts.google.com/o/oauth2/auth\",\"token_uri\":\"https://oauth2.googleapis.com/token\",\"redirect_uris\":[\"http://localhost\"]}}}}").unwrap();
        file
    }
    fn login(file: &tempfile::NamedTempFile) -> CaptureAuthCommand {
        CaptureAuthCommand::Login {
            connection: "work".into(),
            client_json: file.path().into(),
        }
    }

    #[test]
    fn calendar_command_plumbing_uses_separate_profile_and_local_status_logout() {
        struct CalendarStore(Store);
        impl CredentialStore for CalendarStore {
            fn profile(&self) -> AuthProfile {
                AuthProfile::CalendarEvents
            }
            fn lock(
                &self,
                connection: &str,
            ) -> hive_google_auth::Result<Box<dyn CredentialGuard + '_>> {
                self.0.lock(connection)
            }
            fn load(&self, connection: &str) -> hive_google_auth::Result<Option<StoredConnection>> {
                self.0.load(connection)
            }
            fn save(
                &self,
                connection: &str,
                record: &StoredConnection,
            ) -> hive_google_auth::Result<()> {
                self.0.save(connection, record)
            }
        }
        struct CalendarFlow<'a>(&'a Store);
        impl AuthorizationFlow for CalendarFlow<'_> {
            fn profile(&self) -> AuthProfile {
                AuthProfile::CalendarEvents
            }
            fn authorize(
                &self,
                client: &ClientConfig,
            ) -> hive_google_auth::Result<AuthorizationGrant> {
                Flow(self.0).authorize(client)
            }
        }
        let file = client_file();
        let tasks = Store::default();
        let tasks_transport = Transport::new(&tasks);
        execute_with(login(&file), &tasks, &tasks_transport, &Flow(&tasks)).unwrap();
        let tasks_before = format!("{:?}", tasks.record.lock().unwrap());
        let calendar = CalendarStore(Store::default());
        let transport = Transport {
            profile: AuthProfile::CalendarEvents,
            ..Transport::new(&calendar.0)
        };
        let status = execute_with(
            login(&file),
            &calendar,
            &transport,
            &CalendarFlow(&calendar.0),
        )
        .unwrap();
        assert_eq!(status.scopes, AuthProfile::CalendarEvents.scopes());
        let local = Transport::new(&calendar.0);
        let before = execute_with(
            CaptureAuthCommand::Status {
                connection: "work".into(),
            },
            &calendar,
            &local,
            &CalendarFlow(&calendar.0),
        )
        .unwrap();
        let after = execute_with(
            CaptureAuthCommand::Logout {
                connection: "work".into(),
            },
            &calendar,
            &local,
            &CalendarFlow(&calendar.0),
        )
        .unwrap();
        assert!(before.credential_stored);
        assert!(!after.credential_stored);
        assert_eq!(before.account_namespace, after.account_namespace);
        assert_eq!(
            calendar
                .0
                .record
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .generation,
            2
        );
        assert_eq!(local.exchanges.load(SeqCst), 0);
        assert_eq!(local.refreshes.load(SeqCst), 0);
        assert_eq!(local.identities.load(SeqCst), 0);
        assert_eq!(format!("{:?}", tasks.record.lock().unwrap()), tasks_before);
        assert_eq!(
            tasks
                .record
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .grant
                .as_ref()
                .unwrap()
                .scopes,
            REQUIRED_SCOPES
        );
    }
    fn args(connection: Option<&str>, environment: Option<&str>) -> GoogleCredentialArgs {
        GoogleCredentialArgs {
            connection: connection.map(str::to_owned),
            access_token_env: environment.map(str::to_owned),
        }
    }
    fn no_saved(_: &str) -> Result<BoundAccessToken> {
        panic!("unexpected saved credential access")
    }
    fn no_env(_: &str) -> Result<String> {
        panic!("unexpected environment read")
    }

    fn assert_needs_action(result: BackgroundCredentials, generation: Option<u64>, reason: &str) {
        let BackgroundCredentials::NeedsAction {
            generation: actual_generation,
            reason: actual_reason,
        } = result
        else {
            panic!("expected background credentials to need action")
        };
        assert_eq!(actual_generation, generation);
        assert_eq!(actual_reason, reason);
    }

    #[test]
    fn background_metadata_tracks_login_and_logout_without_transport() {
        let file = client_file();
        let store = Store::default();
        assert_eq!(credential_metadata(&store, "work").unwrap(), None);
        let transport = Transport::new(&store);
        let status = execute_with(login(&file), &store, &transport, &Flow(&store)).unwrap();
        let namespace = status.account_namespace.unwrap();
        let local = Transport::new(&store);
        assert_eq!(
            credential_metadata(&store, "work").unwrap(),
            Some((1, namespace.clone(), true))
        );
        execute_with(
            CaptureAuthCommand::Logout {
                connection: "work".into(),
            },
            &store,
            &local,
            &Flow(&store),
        )
        .unwrap();
        assert_eq!(
            credential_metadata(&store, "work").unwrap(),
            Some((2, namespace, false))
        );
        assert!(!store.held.load(SeqCst));
        assert_eq!(local.exchanges.load(SeqCst), 0);
        assert_eq!(local.refreshes.load(SeqCst), 0);
        assert_eq!(local.identities.load(SeqCst), 0);
    }

    #[test]
    fn background_ready_refreshes_once_with_verified_namespace_and_latest_generation() {
        for rotate in [false, true] {
            let file = client_file();
            let store = Store::default();
            let login_transport = Transport::new(&store);
            let status =
                execute_with(login(&file), &store, &login_transport, &Flow(&store)).unwrap();
            let namespace = status.account_namespace.unwrap();
            let transport = Transport {
                rotate,
                ..Transport::new(&store)
            };
            let result =
                background_client_with("work", &namespace, None, &store, &transport).unwrap();
            let BackgroundCredentials::Ready {
                resolved,
                generation,
            } = result
            else {
                panic!("verified refresh did not release a background client")
            };
            assert_eq!(resolved.identity.unwrap().namespace().unwrap(), namespace);
            assert_eq!(generation, if rotate { 2 } else { 1 });
            assert_eq!(
                credential_metadata(&store, "work").unwrap().unwrap().0,
                generation
            );
            assert_eq!(transport.refreshes.load(SeqCst), 1);
            assert_eq!(transport.identities.load(SeqCst), 1);
            assert_eq!(transport.exchanges.load(SeqCst), 0);
        }
    }

    #[test]
    fn background_missing_mismatched_and_tombstoned_connections_never_refresh_or_fallback() {
        for case in ["missing", "mismatch", "tombstone"] {
            let file = client_file();
            let store = Store::default();
            let setup = Transport::new(&store);
            let namespace = GoogleIdentity::new(SUBJECT).unwrap().namespace().unwrap();
            if case != "missing" {
                execute_with(login(&file), &store, &setup, &Flow(&store)).unwrap();
            }
            if case == "tombstone" {
                execute_with(
                    CaptureAuthCommand::Logout {
                        connection: "work".into(),
                    },
                    &store,
                    &setup,
                    &Flow(&store),
                )
                .unwrap();
            }
            let expected = if case == "mismatch" {
                "google-sub-v1:other"
            } else {
                &namespace
            };
            let transport = Transport::new(&store);
            let result =
                background_client_with("work", expected, None, &store, &transport).unwrap();
            let (generation, reason) = match case {
                "missing" => (None, "not_connected"),
                "mismatch" => (Some(1), "connection_binding_mismatch_or_logged_out"),
                _ => (Some(2), "connection_binding_mismatch_or_logged_out"),
            };
            assert_needs_action(result, generation, reason);
            assert_eq!(transport.refreshes.load(SeqCst), 0);
            assert_eq!(transport.identities.load(SeqCst), 0);
            assert_eq!(transport.exchanges.load(SeqCst), 0);
        }
    }

    #[test]
    fn background_paused_generation_resumes_only_after_explicit_fake_login() {
        let file = client_file();
        let store = Store::default();
        let setup = Transport::new(&store);
        let status = execute_with(login(&file), &store, &setup, &Flow(&store)).unwrap();
        let namespace = status.account_namespace.unwrap();
        let transport = Transport::new(&store);
        for _ in 0..2 {
            assert_needs_action(
                background_client_with("work", &namespace, Some(1), &store, &transport).unwrap(),
                Some(1),
                "sign_in_required",
            );
        }
        assert_eq!(transport.refreshes.load(SeqCst), 0);
        assert_eq!(transport.identities.load(SeqCst), 0);
        assert_eq!(transport.exchanges.load(SeqCst), 0);
        execute_with(login(&file), &store, &setup, &Flow(&store)).unwrap();
        assert!(matches!(
            background_client_with("work", &namespace, Some(1), &store, &transport).unwrap(),
            BackgroundCredentials::Ready { generation: 2, .. }
        ));
        assert_eq!(transport.refreshes.load(SeqCst), 1);
        assert_eq!(transport.identities.load(SeqCst), 1);
        assert_eq!(transport.exchanges.load(SeqCst), 0);
        assert_eq!(setup.exchanges.load(SeqCst), 2);
    }

    #[test]
    fn background_refresh_errors_are_typed_and_never_retried() {
        for error in [
            AuthError::ReauthenticationRequired,
            AuthError::Transport,
            AuthError::Timeout,
        ] {
            let file = client_file();
            let store = Store::default();
            let setup = Transport::new(&store);
            let status = execute_with(login(&file), &store, &setup, &Flow(&store)).unwrap();
            let transport = Transport {
                refresh_error: Some(error),
                ..Transport::new(&store)
            };
            let result = background_client_with(
                "work",
                &status.account_namespace.unwrap(),
                None,
                &store,
                &transport,
            )
            .unwrap();
            if error == AuthError::ReauthenticationRequired {
                assert_needs_action(result, Some(1), "sign_in_required");
            } else {
                assert!(matches!(
                    result,
                    BackgroundCredentials::Transient {
                        reason: "authentication_transport"
                    }
                ));
            }
            assert_eq!(transport.refreshes.load(SeqCst), 1);
            assert_eq!(transport.identities.load(SeqCst), 0);
            assert_eq!(transport.exchanges.load(SeqCst), 0);
        }
    }

    #[test]
    fn background_failed_attempt_never_pauses_foreground_login_on_guard_drop() {
        for failed_save in [false, true] {
            let file = client_file();
            let store = Store::default();
            let setup = Transport::new(&store);
            let status = execute_with(login(&file), &store, &setup, &Flow(&store)).unwrap();
            let namespace = status.account_namespace.unwrap();
            let transport = Transport {
                refresh_error: (!failed_save).then_some(AuthError::ReauthenticationRequired),
                rotate: failed_save,
                ..Transport::new(&store)
            };
            store.fail_save.store(failed_save, SeqCst);
            // The first drop is the preflight metadata guard; the second is
            // the failed refresh guard. Neither relies on thread scheduling.
            store.login_on_drop.store(2, SeqCst);
            let locks = store.locks.load(SeqCst);
            assert_needs_action(
                background_client_with("work", &namespace, None, &store, &transport).unwrap(),
                Some(1),
                "sign_in_required",
            );
            assert_eq!(store.locks.load(SeqCst) - locks, 3);
            assert_eq!(
                credential_metadata(&store, "work").unwrap(),
                Some((2, namespace.clone(), true))
            );
            let healthy = Transport::new(&store);
            assert!(matches!(
                background_client_with("work", &namespace, Some(1), &store, &healthy).unwrap(),
                BackgroundCredentials::Ready { generation: 2, .. }
            ));
            assert_eq!(transport.refreshes.load(SeqCst), 1);
            assert_eq!(transport.identities.load(SeqCst), 0);
            assert_eq!(transport.exchanges.load(SeqCst), 0);
            assert_eq!(healthy.refreshes.load(SeqCst), 1);
            assert_eq!(healthy.identities.load(SeqCst), 1);
            assert_eq!(healthy.exchanges.load(SeqCst), 0);
        }
    }

    #[test]
    fn background_reauthentication_after_rotation_pauses_latest_generation() {
        let file = client_file();
        let store = Store::default();
        let setup = Transport::new(&store);
        let status = execute_with(login(&file), &store, &setup, &Flow(&store)).unwrap();
        let namespace = status.account_namespace.unwrap();
        let transport = Transport {
            rotate: true,
            identity_error: Some(AuthError::ReauthenticationRequired),
            ..Transport::new(&store)
        };
        assert_needs_action(
            background_client_with("work", &namespace, None, &store, &transport).unwrap(),
            Some(2),
            "sign_in_required",
        );
        assert_eq!(
            credential_metadata(&store, "work").unwrap(),
            Some((2, namespace.clone(), true))
        );
        assert_needs_action(
            background_client_with("work", &namespace, Some(2), &store, &transport).unwrap(),
            Some(2),
            "sign_in_required",
        );
        assert_eq!(transport.refreshes.load(SeqCst), 1);
        assert_eq!(transport.identities.load(SeqCst), 1);
        assert_eq!(transport.exchanges.load(SeqCst), 0);
    }

    #[test]
    fn background_busy_locked_and_denied_storage_fail_without_transport_or_ui() {
        struct FailingStore {
            error: AuthError,
            on_load: bool,
        }
        impl CredentialStore for FailingStore {
            fn lock(&self, _: &str) -> hive_google_auth::Result<Box<dyn CredentialGuard + '_>> {
                if self.on_load {
                    Ok(Box::new(EmptyGuard))
                } else {
                    Err(self.error)
                }
            }
            fn load(&self, _: &str) -> hive_google_auth::Result<Option<StoredConnection>> {
                assert!(self.on_load);
                Err(self.error)
            }
            fn save(&self, _: &str, _: &StoredConnection) -> hive_google_auth::Result<()> {
                panic!("background storage failure must not write credentials")
            }
        }
        for error in [
            AuthError::Busy,
            AuthError::StorageLocked,
            AuthError::StorageDenied,
        ] {
            for on_load in [false, true] {
                let store = FailingStore { error, on_load };
                assert_eq!(credential_metadata(&store, "work").unwrap_err(), error);
                let unused = Store::default();
                let transport = Transport::new(&unused);
                let result =
                    background_client_with("work", "expected", None, &store, &transport).unwrap();
                if error == AuthError::Busy {
                    assert!(matches!(
                        result,
                        BackgroundCredentials::Transient {
                            reason: "credential_busy"
                        }
                    ));
                } else {
                    assert_needs_action(result, None, "keychain_locked_or_denied");
                }
                assert_eq!(transport.refreshes.load(SeqCst), 0);
                assert_eq!(transport.identities.load(SeqCst), 0);
                assert_eq!(transport.exchanges.load(SeqCst), 0);
            }
        }
    }

    struct EmptyGuard;
    impl CredentialGuard for EmptyGuard {}

    #[test]
    fn background_failure_reasons_are_static_allowlisted_and_secret_free() {
        for (error, transient, expected) in [
            (AuthError::Busy, true, "credential_busy"),
            (AuthError::Transport, true, "authentication_transport"),
            (AuthError::Timeout, true, "authentication_transport"),
            (AuthError::StorageLocked, false, "keychain_locked_or_denied"),
            (AuthError::StorageDenied, false, "keychain_locked_or_denied"),
            (
                AuthError::ReauthenticationRequired,
                false,
                "sign_in_required",
            ),
            (AuthError::IdentityMismatch, false, "sign_in_required"),
            (AuthError::StorageCorrupt, false, "sign_in_required"),
        ] {
            let reason = match background_failure(error, Some(42)) {
                BackgroundCredentials::Transient { reason } => {
                    assert!(transient);
                    reason
                }
                BackgroundCredentials::NeedsAction { generation, reason } => {
                    assert!(!transient);
                    assert_eq!(generation, Some(42));
                    reason
                }
                BackgroundCredentials::Ready { .. } => panic!("failure released a client"),
            };
            assert_eq!(reason, expected);
            for canary in [
                ACCESS,
                REFRESH,
                SUBJECT,
                CLIENT,
                CODE,
                VERIFIER,
                "fixture-rotated-refresh-canary",
            ] {
                assert!(!reason.contains(canary));
            }
        }
    }

    #[test]
    fn source_validation_precedes_all_credential_access() {
        for (source, account, capture) in [
            (args(None, None), None, false),
            (args(Some("work"), Some("HIVE_GOOGLE_TOKEN")), None, false),
            (args(Some("../invalid"), None), None, false),
            (args(Some("work"), None), Some("operator"), true),
            (args(Some("work"), None), Some(""), true),
            (args(None, Some("OTHER_TOKEN")), None, false),
            (args(None, Some("HIVE_GOOGLE_BAD-NAME")), None, false),
            (args(None, Some("HIVE_GOOGLE_\u{e9}")), None, false),
            (args(None, Some("HIVE_GOOGLE_TOKEN")), None, true),
            (args(None, Some("HIVE_GOOGLE_TOKEN")), Some("  "), true),
            (
                args(None, Some("HIVE_GOOGLE_TOKEN")),
                Some("google-sub-v1:forged"),
                true,
            ),
            (
                args(None, Some("HIVE_GOOGLE_TOKEN")),
                Some("bad\nkey"),
                true,
            ),
        ] {
            assert!(
                source
                    .resolve_with(account, capture, no_saved, no_env)
                    .is_err()
            );
        }
    }

    #[test]
    fn explicit_environment_lists_and_capture_need_no_saved_credentials() {
        let source = args(None, Some("HIVE_GOOGLE_FIXTURE_TOKEN"));
        for (account, capture) in [(None, false), (Some("stable-operator"), true)] {
            let resolved = source
                .resolve_with(account, capture, no_saved, |name| {
                    assert_eq!(name, "HIVE_GOOGLE_FIXTURE_TOKEN");
                    Ok(ACCESS.into())
                })
                .unwrap();
            assert!(resolved.identity.is_none());
        }
    }

    #[test]
    fn clap_requires_exactly_one_explicit_source() {
        use clap::Parser;
        #[derive(Debug, Parser)]
        struct Cli {
            #[command(flatten)]
            credentials: GoogleCredentialArgs,
        }
        assert!(Cli::try_parse_from(["test"]).is_err());
        assert!(
            Cli::try_parse_from([
                "test",
                "--connection",
                "work",
                "--access-token-env",
                "HIVE_GOOGLE_TOKEN"
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["test", "--connection", "work"]).is_ok());
        assert!(Cli::try_parse_from(["test", "--access-token-env", "HIVE_GOOGLE_TOKEN"]).is_ok());
    }

    #[test]
    fn auth_dispatch_allowlists_output_and_logout_is_local() {
        let file = client_file();
        let store = Store::default();
        let transport = Transport::new(&store);
        let status = execute_with(login(&file), &store, &transport, &Flow(&store)).unwrap();
        let value = serde_json::to_value(&status).unwrap();
        let mut keys: Vec<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "accountNamespace",
                "connection",
                "credentialStored",
                "schemaVersion",
                "scopes"
            ]
        );
        let output = serde_json::to_string(&status).unwrap();
        for canary in [
            ACCESS,
            REFRESH,
            SUBJECT,
            CLIENT,
            CODE,
            VERIFIER,
            "127.0.0.1",
            "code_verifier",
        ] {
            assert!(!output.contains(canary));
        }
        let local = Transport::new(&store);
        let before = execute_with(
            CaptureAuthCommand::Status {
                connection: "work".into(),
            },
            &store,
            &local,
            &Flow(&store),
        )
        .unwrap();
        let after = execute_with(
            CaptureAuthCommand::Logout {
                connection: "work".into(),
            },
            &store,
            &local,
            &Flow(&store),
        )
        .unwrap();
        assert!(before.credential_stored);
        assert!(!after.credential_stored);
        assert_eq!(before.account_namespace, after.account_namespace);
        assert!(
            store
                .record
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .grant
                .is_none()
        );
        assert_eq!(local.exchanges.load(SeqCst), 0);
        assert_eq!(local.refreshes.load(SeqCst), 0);
        assert_eq!(local.identities.load(SeqCst), 0);
    }

    #[test]
    fn missing_client_and_invalid_connection_fail_before_lock() {
        let fixture = tempfile::tempdir().unwrap();
        let store = Store::default();
        let transport = Transport::new(&store);
        for (connection, expected) in [
            ("work", AuthError::InvalidClient),
            ("../bad", AuthError::InvalidConnection),
        ] {
            let error = execute_with(
                CaptureAuthCommand::Login {
                    connection: connection.into(),
                    client_json: fixture.path().join("missing.json"),
                },
                &store,
                &transport,
                &Flow(&store),
            )
            .unwrap_err();
            assert_eq!(error.downcast_ref::<AuthError>(), Some(&expected));
        }
        assert_eq!(store.locks.load(SeqCst), 0);
    }

    #[test]
    fn save_failure_preserves_typed_static_error() {
        let file = client_file();
        let store = Store::default();
        store.fail_save.store(true, SeqCst);
        let transport = Transport::new(&store);
        let error = execute_with(login(&file), &store, &transport, &Flow(&store)).unwrap_err();
        assert_eq!(
            error.downcast_ref::<AuthError>(),
            Some(&AuthError::StorageUnavailable)
        );
        assert_eq!(error.to_string(), "secure credential storage failed");
        assert!(store.record.lock().unwrap().is_none());
    }

    #[test]
    fn saved_resolution_refreshes_once_and_preserves_verified_namespace() {
        let file = client_file();
        let store = Store::default();
        let transport = Transport::new(&store);
        let status = execute_with(login(&file), &store, &transport, &Flow(&store)).unwrap();
        let source = args(Some("work"), None);
        let resolved = source
            .resolve_with(
                None,
                true,
                |name| Ok(Authenticator::new(&store, &transport).access_token(name)?),
                no_env,
            )
            .unwrap();
        assert_eq!(
            resolved.identity.unwrap().namespace().unwrap(),
            status.account_namespace.unwrap()
        );
        assert_eq!(transport.refreshes.load(SeqCst), 1);
        assert_eq!(transport.identities.load(SeqCst), 2);
        let mismatch = Transport {
            mismatch: true,
            ..Transport::new(&store)
        };
        let result = source.resolve_with(
            None,
            false,
            |name| Ok(Authenticator::new(&store, &mismatch).access_token(name)?),
            no_env,
        );
        let Err(error) = result else {
            panic!("mismatched identity released a client")
        };
        assert_eq!(
            error.downcast_ref::<AuthError>(),
            Some(&AuthError::IdentityMismatch)
        );
        assert_eq!(mismatch.refreshes.load(SeqCst), 1);
    }

    #[test]
    fn missing_saved_connection_never_falls_back() {
        let store = Store::default();
        let transport = Transport::new(&store);
        let result = args(Some("missing"), None).resolve_with(
            None,
            false,
            |name| Ok(Authenticator::new(&store, &transport).access_token(name)?),
            no_env,
        );
        let Err(error) = result else {
            panic!("missing connection released a client")
        };
        assert_eq!(
            error.downcast_ref::<AuthError>(),
            Some(&AuthError::NotConnected)
        );
        assert_eq!(transport.refreshes.load(SeqCst), 0);
    }

    #[test]
    fn callback_failure_remains_typed_and_static() {
        struct InvalidCallback;
        impl AuthorizationFlow for InvalidCallback {
            fn authorize(&self, _: &ClientConfig) -> hive_google_auth::Result<AuthorizationGrant> {
                Err(AuthError::InvalidCallback)
            }
        }
        let file = client_file();
        let store = Store::default();
        let transport = Transport::new(&store);
        let error = execute_with(login(&file), &store, &transport, &InvalidCallback).unwrap_err();
        assert_eq!(
            error.downcast_ref::<AuthError>(),
            Some(&AuthError::InvalidCallback)
        );
        assert_eq!(error.to_string(), "Google callback is invalid");
        assert_eq!(transport.exchanges.load(SeqCst), 0);
        assert!(store.record.lock().unwrap().is_none());
    }
}
