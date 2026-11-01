//! Explicit, account-bound credential operations. No access-token cache or implicit sign-in.
use crate::{
    AccessTokenAttempt, AuthError, AuthorizationFlow, BoundAccessToken, ClientConfig,
    ConnectionStatus, CredentialStore, GrantRecord, OAuthTransport, REQUIRED_SCOPES, Result,
    SecretString, StoredConnection, TokenSet, validate_connection,
};

pub struct Authenticator<'a> {
    store: &'a dyn CredentialStore,
    transport: &'a dyn OAuthTransport,
}

impl<'a> Authenticator<'a> {
    pub fn new(store: &'a dyn CredentialStore, transport: &'a dyn OAuthTransport) -> Self {
        Self { store, transport }
    }

    pub fn login(
        &self,
        connection: &str,
        client: &ClientConfig,
        flow: &dyn AuthorizationFlow,
    ) -> Result<ConnectionStatus> {
        validate_connection(connection)?;
        let _guard = self.store.lock(connection)?;
        validate_client(client)?;
        let old = self.store.load(connection)?;
        if let Some(record) = &old {
            validate_record(connection, record)?;
        }
        // Keep the lock through authorization, so logout cannot race a browser session.
        let grant = flow.authorize(client)?;
        let tokens = self.transport.exchange(client, &grant)?;
        validate_tokens(&tokens)?;
        let scopes = tokens.scopes.as_ref().ok_or_else(|| scope_mismatch(None))?;
        validate_scopes(scopes)?;
        let refresh_token = tokens.refresh_token.ok_or(AuthError::InvalidResponse)?;
        let identity = self.transport.identity(&tokens.access_token)?;
        identity
            .namespace()
            .map_err(|_| AuthError::StorageCorrupt)?;
        if old
            .as_ref()
            .is_some_and(|record| record.identity != identity)
        {
            return Err(AuthError::IdentityMismatch);
        }
        let record = StoredConnection {
            version: 1,
            connection: connection.to_owned(),
            identity,
            generation: next_generation(old.as_ref())?,
            grant: Some(GrantRecord {
                client: client.clone(),
                refresh_token,
                scopes: scopes.clone(),
            }),
        };
        self.store.save(connection, &record)?;
        connection_status(connection, Some(&record))
    }

    /// Refresh exactly once and verify the account with fresh UserInfo.
    /// If Google rotates a refresh token and saving fails, reauthentication may be
    /// necessary: the previous stored token is retained, but may already be spent.
    pub fn access_token(&self, connection: &str) -> Result<BoundAccessToken> {
        self.access_token_attempt(connection).outcome
    }

    /// Freeze outcome and generation under the credential lock, including any
    /// successfully persisted rotation, even when subsequent UserInfo fails.
    pub fn access_token_attempt(&self, connection: &str) -> AccessTokenAttempt {
        let mut guard = None;
        let mut generation = None;
        let outcome = (|| {
            validate_connection(connection)?;
            guard = Some(self.store.lock(connection)?);
            let mut record = self
                .store
                .load(connection)?
                .ok_or(AuthError::NotConnected)?;
            validate_record(connection, &record)?;
            generation = Some(record.generation);
            let grant = record.grant.as_ref().ok_or(AuthError::NotConnected)?;
            let tokens = self
                .transport
                .refresh(&grant.client, &grant.refresh_token)?;
            validate_tokens(&tokens)?;
            if let Some(scopes) = &tokens.scopes {
                validate_scopes(scopes)?;
            }
            if let Some(refresh_token) = tokens.refresh_token
                && refresh_token.expose() != grant.refresh_token.expose()
            {
                record.generation = next_generation(Some(&record))?;
                record.grant = Some(GrantRecord {
                    client: grant.client.clone(),
                    refresh_token,
                    scopes: tokens.scopes.unwrap_or_else(|| grant.scopes.clone()),
                });
                // Retain rotation even if UserInfo fails: the old token may be spent.
                // This updates credentials, never the existing account/client binding.
                self.store.save(connection, &record)?;
                generation = Some(record.generation);
            }
            let identity = self.transport.identity(&tokens.access_token)?;
            identity
                .namespace()
                .map_err(|_| AuthError::StorageCorrupt)?;
            if identity != record.identity {
                return Err(AuthError::IdentityMismatch);
            }
            Ok(BoundAccessToken {
                token: tokens.access_token,
                identity,
            })
        })();
        let attempt = AccessTokenAttempt {
            outcome,
            generation,
        };
        drop(guard);
        attempt
    }

    /// Local credential presence only; this does not prove Google's grant is valid.
    pub fn status(&self, connection: &str) -> Result<ConnectionStatus> {
        validate_connection(connection)?;
        let _guard = self.store.lock(connection)?;
        let record = self.store.load(connection)?;
        if let Some(record) = &record {
            validate_record(connection, record)?;
        }
        connection_status(connection, record.as_ref())
    }

    /// Clear local credentials without revocation, preserving the account binding.
    pub fn logout(&self, connection: &str) -> Result<ConnectionStatus> {
        validate_connection(connection)?;
        let _guard = self.store.lock(connection)?;
        let record = self.store.load(connection)?;
        let Some(mut record) = record else {
            return connection_status(connection, None);
        };
        validate_record(connection, &record)?;
        if record.grant.is_some() {
            record.generation = next_generation(Some(&record))?;
            record.grant = None;
            self.store.save(connection, &record)?;
        }
        connection_status(connection, Some(&record))
    }
}

fn validate_secret(secret: &SecretString) -> Result<()> {
    let value = secret.expose();
    if value.trim().is_empty() || value.len() > 16 * 1024 || value.chars().any(char::is_control) {
        return Err(AuthError::InvalidResponse);
    }
    Ok(())
}

fn validate_client(client: &ClientConfig) -> Result<()> {
    let prefix = client
        .client_id
        .strip_suffix(".apps.googleusercontent.com")
        .ok_or(AuthError::InvalidClient)?;
    if prefix.is_empty()
        || client.client_id.len() > 512
        || !prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(AuthError::InvalidClient);
    }
    if let Some(secret) = &client.client_secret {
        validate_secret(secret).map_err(|_| AuthError::InvalidClient)?;
    }
    Ok(())
}

fn validate_scopes(scopes: &[String]) -> Result<()> {
    if scopes.len() != REQUIRED_SCOPES.len()
        || !REQUIRED_SCOPES
            .iter()
            .all(|required| scopes.iter().filter(|s| s.as_str() == *required).count() == 1)
    {
        return Err(scope_mismatch(Some(scopes)));
    }
    Ok(())
}

fn scope_mismatch(scopes: Option<&[String]>) -> AuthError {
    let response_missing = scopes.is_none();
    let scopes = scopes.unwrap_or_default();
    let mut tasks_readonly = false;
    let mut openid = false;
    let mut userinfo_email = false;
    let mut userinfo_profile = false;
    let mut unknown_count = 0;
    let mut duplicates = false;
    for (index, scope) in scopes.iter().enumerate() {
        duplicates |= scopes[..index].contains(scope);
        match scope.as_str() {
            crate::TASKS_SCOPE => tasks_readonly = true,
            crate::OPENID_SCOPE => openid = true,
            "email" | "https://www.googleapis.com/auth/userinfo.email" => userinfo_email = true,
            "profile" | "https://www.googleapis.com/auth/userinfo.profile" => {
                userinfo_profile = true
            }
            _ => unknown_count += 1,
        }
    }
    AuthError::ScopeMismatch {
        tasks_readonly,
        openid,
        userinfo_email,
        userinfo_profile,
        unknown_count,
        duplicates,
        response_missing,
    }
}

fn validate_tokens(tokens: &TokenSet) -> Result<()> {
    if !(1..=86400).contains(&tokens.expires_in) {
        return Err(AuthError::InvalidResponse);
    }
    validate_secret(&tokens.access_token)?;
    if let Some(refresh) = &tokens.refresh_token {
        validate_secret(refresh)?;
    }
    Ok(())
}

fn validate_record(connection: &str, record: &StoredConnection) -> Result<()> {
    if record.version != 1 || record.connection != connection || record.generation == 0 {
        return Err(AuthError::StorageCorrupt);
    }
    record
        .identity
        .namespace()
        .map_err(|_| AuthError::StorageCorrupt)?;
    if let Some(grant) = &record.grant {
        validate_client(&grant.client).map_err(|_| AuthError::StorageCorrupt)?;
        validate_scopes(&grant.scopes).map_err(|_| AuthError::StorageCorrupt)?;
        validate_secret(&grant.refresh_token).map_err(|_| AuthError::StorageCorrupt)?;
    }
    Ok(())
}

fn next_generation(record: Option<&StoredConnection>) -> Result<u64> {
    record
        .map_or(0, |r| r.generation)
        .checked_add(1)
        .ok_or(AuthError::StorageCorrupt)
}

fn connection_status(
    connection: &str,
    record: Option<&StoredConnection>,
) -> Result<ConnectionStatus> {
    Ok(ConnectionStatus {
        schema_version: "1",
        connection: connection.to_owned(),
        credential_stored: record.is_some_and(|r| r.grant.is_some()),
        account_namespace: record
            .map(|r| r.identity.namespace())
            .transpose()
            .map_err(|_| AuthError::StorageCorrupt)?,
        scopes: record
            .and_then(|r| r.grant.as_ref())
            .map_or_else(Vec::new, |g| g.scopes.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthorizationGrant, CredentialGuard, GoogleIdentity};
    use std::{
        collections::HashMap,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    const ACCESS: &str = "canary-access-secret";
    const REFRESH: &str = "canary-refresh-secret";
    const CLIENT_SECRET: &str = "canary-client-secret";
    const SUBJECT: &str = "canary-raw-subject";

    #[derive(Default)]
    struct Store {
        records: Mutex<HashMap<String, StoredConnection>>,
        held: AtomicBool,
        locks: AtomicUsize,
        saves: AtomicUsize,
        fail_save: AtomicBool,
    }

    struct Guard<'a>(&'a AtomicBool);
    impl CredentialGuard for Guard<'_> {}
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }

    impl CredentialStore for Store {
        fn lock(&self, _: &str) -> Result<Box<dyn CredentialGuard + '_>> {
            self.locks.fetch_add(1, Ordering::SeqCst);
            self.held
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .map_err(|_| AuthError::Busy)?;
            Ok(Box::new(Guard(&self.held)))
        }
        fn load(&self, connection: &str) -> Result<Option<StoredConnection>> {
            assert!(self.held.load(Ordering::SeqCst));
            Ok(self.records.lock().unwrap().get(connection).cloned())
        }
        fn save(&self, connection: &str, record: &StoredConnection) -> Result<()> {
            assert!(self.held.load(Ordering::SeqCst));
            self.saves.fetch_add(1, Ordering::SeqCst);
            if self.fail_save.load(Ordering::SeqCst) {
                return Err(AuthError::StorageUnavailable);
            }
            self.records
                .lock()
                .unwrap()
                .insert(connection.into(), record.clone());
            Ok(())
        }
    }

    struct Transport {
        exchange: Mutex<Option<Result<TokenSet>>>,
        refresh: Mutex<Option<Result<TokenSet>>>,
        valid_refresh: Mutex<SecretString>,
        identity: Mutex<Result<GoogleIdentity>>,
        exchanges: AtomicUsize,
        refreshes: AtomicUsize,
        identities: AtomicUsize,
    }

    impl Default for Transport {
        fn default() -> Self {
            Self {
                exchange: Mutex::new(Some(Ok(tokens(Some(REFRESH))))),
                refresh: Mutex::new(Some(Ok(tokens(None)))),
                valid_refresh: Mutex::new(secret(REFRESH)),
                identity: Mutex::new(Ok(GoogleIdentity::new(SUBJECT).unwrap())),
                exchanges: AtomicUsize::new(0),
                refreshes: AtomicUsize::new(0),
                identities: AtomicUsize::new(0),
            }
        }
    }

    impl OAuthTransport for Transport {
        fn exchange(&self, _: &ClientConfig, _: &AuthorizationGrant) -> Result<TokenSet> {
            self.exchanges.fetch_add(1, Ordering::SeqCst);
            self.exchange
                .lock()
                .unwrap()
                .take()
                .expect("unexpected exchange/retry")
        }
        fn refresh(&self, _: &ClientConfig, refresh_token: &SecretString) -> Result<TokenSet> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            let mut valid_refresh = self.valid_refresh.lock().unwrap();
            if refresh_token.expose() != valid_refresh.expose() {
                return Err(AuthError::ReauthenticationRequired);
            }
            let response = self
                .refresh
                .lock()
                .unwrap()
                .take()
                .expect("unexpected refresh/retry");
            if let Ok(tokens) = &response
                && let Some(replacement) = &tokens.refresh_token
            {
                *valid_refresh = replacement.clone();
            }
            response
        }
        fn identity(&self, access_token: &SecretString) -> Result<GoogleIdentity> {
            assert_eq!(access_token.expose(), ACCESS);
            self.identities.fetch_add(1, Ordering::SeqCst);
            self.identity.lock().unwrap().clone()
        }
    }

    #[derive(Default)]
    struct Flow {
        calls: AtomicUsize,
        error: Option<AuthError>,
    }
    impl AuthorizationFlow for Flow {
        fn authorize(&self, _: &ClientConfig) -> Result<AuthorizationGrant> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self.error {
                return Err(error);
            }
            Ok(AuthorizationGrant {
                code: secret("canary-authorization-code"),
                verifier: secret("canary-pkce-verifier"),
                redirect_uri: "http://127.0.0.1:1234/callback".into(),
            })
        }
    }

    fn secret(value: &str) -> SecretString {
        SecretString::new(value).unwrap()
    }
    fn client() -> ClientConfig {
        ClientConfig {
            client_id: "123-test.apps.googleusercontent.com".into(),
            client_secret: Some(secret(CLIENT_SECRET)),
        }
    }
    fn scopes() -> Vec<String> {
        REQUIRED_SCOPES.iter().map(|s| (*s).into()).collect()
    }
    fn tokens(refresh: Option<&str>) -> TokenSet {
        TokenSet {
            access_token: secret(ACCESS),
            refresh_token: refresh.map(secret),
            expires_in: 3600,
            scopes: Some(scopes()),
        }
    }
    fn connected() -> Store {
        let store = Store::default();
        store.records.lock().unwrap().insert(
            "work".into(),
            StoredConnection {
                version: 1,
                connection: "work".into(),
                identity: GoogleIdentity::new(SUBJECT).unwrap(),
                generation: 7,
                grant: Some(GrantRecord {
                    client: client(),
                    refresh_token: secret(REFRESH),
                    scopes: scopes(),
                }),
            },
        );
        store
    }
    fn snapshot(store: &Store) -> String {
        format!("{:?}", store.records.lock().unwrap())
    }
    fn no_network(transport: &Transport) {
        assert_eq!(transport.exchanges.load(Ordering::SeqCst), 0);
        assert_eq!(transport.refreshes.load(Ordering::SeqCst), 0);
        assert_eq!(transport.identities.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn connection_validation_precedes_all_store_and_flow_operations() {
        let store = Store::default();
        let transport = Transport::default();
        let auth = Authenticator::new(&store, &transport);
        let flow = Flow::default();
        for name in ["", "Work", "../work", "work\n", &"a".repeat(65)] {
            assert_eq!(
                auth.login(name, &client(), &flow).unwrap_err(),
                AuthError::InvalidConnection
            );
            assert_eq!(
                auth.access_token(name).unwrap_err(),
                AuthError::InvalidConnection
            );
            assert_eq!(auth.status(name).unwrap_err(), AuthError::InvalidConnection);
            assert_eq!(auth.logout(name).unwrap_err(), AuthError::InvalidConnection);
        }
        assert_eq!(store.locks.load(Ordering::SeqCst), 0);
        assert_eq!(flow.calls.load(Ordering::SeqCst), 0);
        no_network(&transport);
    }

    #[test]
    fn login_requires_fresh_refresh_token_exact_scopes_and_valid_expiry() {
        let mut cases = vec![(tokens(None), AuthError::InvalidResponse)];
        for (scope, tasks_readonly, openid, userinfo_email, unknown_count, duplicates) in [
            (None, false, false, false, 0, false),
            (Some(vec![]), false, false, false, 0, false),
            (Some(vec!["openid".into()]), false, true, false, 0, false),
            (
                Some(vec!["openid".into(), "openid".into()]),
                false,
                true,
                false,
                0,
                true,
            ),
            (
                Some(vec![
                    "openid".into(),
                    "https://www.googleapis.com/auth/tasks".into(),
                ]),
                false,
                true,
                false,
                1,
                false,
            ),
            (
                Some([scopes(), vec!["email".into()]].concat()),
                true,
                true,
                true,
                0,
                false,
            ),
        ] {
            let expected = AuthError::ScopeMismatch {
                tasks_readonly,
                openid,
                userinfo_email,
                userinfo_profile: false,
                unknown_count,
                duplicates,
                response_missing: scope.is_none(),
            };
            let mut value = tokens(Some(REFRESH));
            value.scopes = scope;
            cases.push((value, expected));
        }
        for expiry in [0, 86401, u64::MAX] {
            let mut value = tokens(Some(REFRESH));
            value.expires_in = expiry;
            cases.push((value, AuthError::InvalidResponse));
        }
        for (value, expected) in cases {
            let store = connected();
            let before = snapshot(&store);
            let transport = Transport::default();
            *transport.exchange.lock().unwrap() = Some(Ok(value));
            assert_eq!(
                Authenticator::new(&store, &transport)
                    .login("work", &client(), &Flow::default())
                    .unwrap_err(),
                expected
            );
            assert_eq!(snapshot(&store), before);
            assert_eq!(store.saves.load(Ordering::SeqCst), 0);
            assert_eq!(transport.exchanges.load(Ordering::SeqCst), 1);
            assert_eq!(transport.identities.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn denial_and_transport_errors_preserve_old_grant_without_retry() {
        let store = connected();
        let before = snapshot(&store);
        let transport = Transport::default();
        let auth = Authenticator::new(&store, &transport);
        let flow = Flow {
            error: Some(AuthError::Denied),
            ..Flow::default()
        };
        assert_eq!(
            auth.login("work", &client(), &flow).unwrap_err(),
            AuthError::Denied
        );
        no_network(&transport);
        for error in [
            AuthError::Transport,
            AuthError::InvalidClientGrant,
            AuthError::ReauthenticationRequired,
        ] {
            *transport.exchange.lock().unwrap() = Some(Err(error));
            assert_eq!(
                auth.login("work", &client(), &Flow::default()).unwrap_err(),
                error
            );
            assert_eq!(snapshot(&store), before);
        }
        assert_eq!(transport.exchanges.load(Ordering::SeqCst), 3);
        assert_eq!(store.saves.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn status_and_logout_are_local_and_keep_binding_but_clear_all_credentials() {
        let store = connected();
        let transport = Transport::default();
        let auth = Authenticator::new(&store, &transport);
        let before = auth.status("work").unwrap();
        assert!(before.credential_stored);
        assert_eq!(before.scopes, scopes());
        assert_eq!(store.saves.load(Ordering::SeqCst), 0);
        let after = auth.logout("work").unwrap();
        assert!(!after.credential_stored);
        assert_eq!(before.account_namespace, after.account_namespace);
        assert!(after.scopes.is_empty());
        let record = store.records.lock().unwrap()["work"].clone();
        assert_eq!(record.generation, 8);
        assert!(record.grant.is_none());
        auth.logout("work").unwrap();
        assert_eq!(store.saves.load(Ordering::SeqCst), 1);
        assert_eq!(
            auth.access_token("work").unwrap_err(),
            AuthError::NotConnected
        );
        assert_eq!(
            auth.access_token("missing").unwrap_err(),
            AuthError::NotConnected
        );
        assert!(!auth.status("missing").unwrap().credential_stored);
        assert!(auth.logout("missing").unwrap().account_namespace.is_none());
        assert_eq!(store.saves.load(Ordering::SeqCst), 1);
        no_network(&transport);
    }

    #[test]
    fn relogin_cannot_swap_accounts_even_after_logout() {
        for logged_out in [false, true] {
            let store = connected();
            let transport = Transport::default();
            let auth = Authenticator::new(&store, &transport);
            if logged_out {
                auth.logout("work").unwrap();
            }
            let before = snapshot(&store);
            *transport.identity.lock().unwrap() = Ok(GoogleIdentity::new("other-account").unwrap());
            assert_eq!(
                auth.login("work", &client(), &Flow::default()).unwrap_err(),
                AuthError::IdentityMismatch
            );
            assert_eq!(snapshot(&store), before);
        }
    }

    #[test]
    fn same_account_can_change_client_and_alias_is_not_identity() {
        let store = connected();
        let transport = Transport::default();
        let auth = Authenticator::new(&store, &transport);
        let namespace = auth.status("work").unwrap().account_namespace;
        let mut replacement = client();
        replacement.client_id = "456-replacement.apps.googleusercontent.com".into();
        assert_eq!(
            auth.login("work", &replacement, &Flow::default())
                .unwrap()
                .account_namespace,
            namespace
        );
        let record = store.records.lock().unwrap()["work"].clone();
        assert_eq!(record.generation, 8);
        assert_eq!(
            record.grant.unwrap().client.client_id,
            replacement.client_id
        );
        *transport.exchange.lock().unwrap() = Some(Ok(tokens(Some(REFRESH))));
        assert_eq!(
            auth.login("personal", &client(), &Flow::default())
                .unwrap()
                .account_namespace,
            namespace
        );
    }

    #[test]
    fn refresh_rotation_is_saved_before_return_and_absent_rotation_does_not_write() {
        for rotation in [None, Some(REFRESH), Some("rotated-refresh-secret")] {
            let store = connected();
            let transport = Transport::default();
            let mut response = tokens(rotation);
            response.scopes = None;
            *transport.refresh.lock().unwrap() = Some(Ok(response));
            let attempt = Authenticator::new(&store, &transport).access_token_attempt("work");
            let rotated = rotation.is_some_and(|r| r != REFRESH);
            assert_eq!(attempt.generation, Some(if rotated { 8 } else { 7 }));
            let token = attempt.outcome.unwrap();
            assert_eq!(token.token.expose(), ACCESS);
            assert_eq!(token.identity, GoogleIdentity::new(SUBJECT).unwrap());
            assert_eq!(store.saves.load(Ordering::SeqCst), usize::from(rotated));
            let record = store.records.lock().unwrap()["work"].clone();
            assert_eq!(record.generation, if rotated { 8 } else { 7 });
            let grant = record.grant.unwrap();
            assert_eq!(grant.refresh_token.expose(), rotation.unwrap_or(REFRESH));
            assert_eq!(grant.scopes, scopes());
            assert_eq!(transport.refreshes.load(Ordering::SeqCst), 1);
            assert_eq!(transport.identities.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn attempt_generation_requires_a_valid_record_and_includes_tombstones() {
        let store = connected();
        let transport = Transport::default();
        let auth = Authenticator::new(&store, &transport);
        for (connection, error) in [
            ("../work", AuthError::InvalidConnection),
            ("missing", AuthError::NotConnected),
        ] {
            let attempt = auth.access_token_attempt(connection);
            assert_eq!(attempt.generation, None);
            assert_eq!(attempt.outcome.unwrap_err(), error);
        }
        let guard = store.lock("work").unwrap();
        let attempt = auth.access_token_attempt("work");
        assert_eq!(attempt.generation, None);
        assert_eq!(attempt.outcome.unwrap_err(), AuthError::Busy);
        drop(guard);
        store
            .records
            .lock()
            .unwrap()
            .get_mut("work")
            .unwrap()
            .version = 2;
        let attempt = auth.access_token_attempt("work");
        assert_eq!(attempt.generation, None);
        assert_eq!(attempt.outcome.unwrap_err(), AuthError::StorageCorrupt);
        store
            .records
            .lock()
            .unwrap()
            .get_mut("work")
            .unwrap()
            .version = 1;
        auth.logout("work").unwrap();
        let attempt = auth.access_token_attempt("work");
        assert_eq!(attempt.generation, Some(8));
        assert_eq!(attempt.outcome.unwrap_err(), AuthError::NotConnected);
        no_network(&transport);
    }

    #[test]
    fn each_access_operation_refreshes_once_without_cache() {
        let store = connected();
        let transport = Transport::default();
        let auth = Authenticator::new(&store, &transport);
        auth.access_token("work").unwrap();
        *transport.refresh.lock().unwrap() = Some(Ok(tokens(None)));
        auth.access_token("work").unwrap();
        assert_eq!(transport.refreshes.load(Ordering::SeqCst), 2);
        assert_eq!(transport.identities.load(Ordering::SeqCst), 2);
        assert_eq!(store.saves.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn save_failures_never_return_token_or_replace_old_record() {
        let store = connected();
        let before = snapshot(&store);
        store.fail_save.store(true, Ordering::SeqCst);
        let transport = Transport::default();
        *transport.refresh.lock().unwrap() = Some(Ok(tokens(Some("rotated-refresh-secret"))));
        let auth = Authenticator::new(&store, &transport);
        let attempt = auth.access_token_attempt("work");
        assert_eq!(attempt.generation, Some(7));
        assert_eq!(attempt.outcome.unwrap_err(), AuthError::StorageUnavailable);
        assert_eq!(transport.identities.load(Ordering::SeqCst), 0);
        assert_eq!(snapshot(&store), before);
        assert_eq!(
            auth.login("work", &client(), &Flow::default()).unwrap_err(),
            AuthError::StorageUnavailable
        );
        assert_eq!(snapshot(&store), before);
        assert_eq!(
            auth.logout("work").unwrap_err(),
            AuthError::StorageUnavailable
        );
        assert_eq!(snapshot(&store), before);
        assert_eq!(
            store.records.lock().unwrap()["work"]
                .grant
                .as_ref()
                .unwrap()
                .refresh_token
                .expose(),
            REFRESH
        );
    }

    #[test]
    fn userinfo_failure_preserves_login_grant_but_retains_bound_refresh_rotation() {
        for identity in [
            Err(AuthError::Transport),
            Ok(serde_json::from_str(r#"{"issuer":"https://evil.invalid","subject":"s"}"#).unwrap()),
        ] {
            let expected = if identity.is_err() {
                AuthError::Transport
            } else {
                AuthError::StorageCorrupt
            };
            let store = connected();
            let before = snapshot(&store);
            let transport = Transport::default();
            *transport.identity.lock().unwrap() = identity;
            *transport.refresh.lock().unwrap() = Some(Ok(tokens(Some("rotated-refresh-secret"))));
            let auth = Authenticator::new(&store, &transport);
            assert_eq!(
                auth.login("work", &client(), &Flow::default()).unwrap_err(),
                expected
            );
            assert_eq!(snapshot(&store), before);
            assert_eq!(store.saves.load(Ordering::SeqCst), 0);
            let attempt = auth.access_token_attempt("work");
            assert_eq!(attempt.generation, Some(8));
            assert_eq!(attempt.outcome.unwrap_err(), expected);
            assert_eq!(store.saves.load(Ordering::SeqCst), 1);
            let record = store.records.lock().unwrap()["work"].clone();
            assert_eq!(record.identity, GoogleIdentity::new(SUBJECT).unwrap());
            assert_eq!(record.generation, 8);
            let grant = record.grant.unwrap();
            assert_eq!(grant.refresh_token.expose(), "rotated-refresh-secret");
            assert_eq!(grant.client.client_id, client().client_id);
            assert_eq!(grant.client.client_secret.unwrap().expose(), CLIENT_SECRET);
        }
    }

    #[test]
    fn spent_refresh_recovers_after_one_userinfo_failure_without_sign_in() {
        struct FailsUserInfoOnce<'a> {
            inner: Transport,
            store: &'a Store,
        }
        impl OAuthTransport for FailsUserInfoOnce<'_> {
            fn exchange(&self, _: &ClientConfig, _: &AuthorizationGrant) -> Result<TokenSet> {
                panic!("refresh must not start a new authorization");
            }
            fn refresh(&self, client: &ClientConfig, refresh: &SecretString) -> Result<TokenSet> {
                self.inner.refresh(client, refresh)
            }
            fn identity(&self, access: &SecretString) -> Result<GoogleIdentity> {
                // Rotation must already be durable, with the lock and binding intact.
                assert!(self.store.held.load(Ordering::SeqCst));
                let record = self.store.records.lock().unwrap()["work"].clone();
                assert_eq!(record.identity, GoogleIdentity::new(SUBJECT).unwrap());
                assert_eq!(record.generation, 8);
                let grant = record.grant.unwrap();
                assert_eq!(grant.refresh_token.expose(), "rotated-refresh-secret");
                assert_eq!(grant.client.client_id, client().client_id);
                let identity = self.inner.identity(access)?;
                if self.inner.identities.load(Ordering::SeqCst) == 1 {
                    return Err(AuthError::Transport);
                }
                Ok(identity)
            }
        }
        let store = connected();
        let transport = FailsUserInfoOnce {
            inner: Transport::default(),
            store: &store,
        };
        *transport.inner.refresh.lock().unwrap() = Some(Ok(tokens(Some("rotated-refresh-secret"))));
        let auth = Authenticator::new(&store, &transport);
        assert_eq!(auth.access_token("work").unwrap_err(), AuthError::Transport);
        // The fake provider invalidated R1 when issuing R2; reusing R1 now fails.
        assert_eq!(
            transport.inner.valid_refresh.lock().unwrap().expose(),
            "rotated-refresh-secret"
        );
        let mut response = tokens(None);
        response.scopes = None;
        *transport.inner.refresh.lock().unwrap() = Some(Ok(response));
        let access = auth.access_token("work").unwrap();
        assert_eq!(access.token.expose(), ACCESS);
        assert_eq!(access.identity, GoogleIdentity::new(SUBJECT).unwrap());
        assert_eq!(transport.inner.refreshes.load(Ordering::SeqCst), 2);
        assert_eq!(transport.inner.identities.load(Ordering::SeqCst), 2);
        assert_eq!(transport.inner.exchanges.load(Ordering::SeqCst), 0);
        assert_eq!(store.saves.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn expiry_boundaries_and_scope_order_are_accepted() {
        for expires_in in [1, 86400] {
            let store = Store::default();
            let transport = Transport::default();
            let mut response = tokens(Some(REFRESH));
            response.expires_in = expires_in;
            response.scopes.as_mut().unwrap().reverse();
            *transport.exchange.lock().unwrap() = Some(Ok(response));
            let status = Authenticator::new(&store, &transport)
                .login("work", &client(), &Flow::default())
                .unwrap();
            assert!(status.credential_stored);
            assert_eq!(
                status.account_namespace,
                Some(GoogleIdentity::new(SUBJECT).unwrap().namespace().unwrap())
            );
            assert_eq!(store.records.lock().unwrap()["work"].generation, 1);
        }
    }

    #[test]
    fn refresh_errors_preserve_grant_and_subject_mismatch_retains_rotation_not_identity() {
        let store = connected();
        let before = snapshot(&store);
        let transport = Transport::default();
        let auth = Authenticator::new(&store, &transport);
        for error in [
            AuthError::ReauthenticationRequired,
            AuthError::InvalidClientGrant,
            AuthError::Transport,
        ] {
            *transport.refresh.lock().unwrap() = Some(Err(error));
            assert_eq!(auth.access_token("work").unwrap_err(), error);
            assert_eq!(snapshot(&store), before);
        }
        assert_eq!(transport.refreshes.load(Ordering::SeqCst), 3);
        assert_eq!(transport.identities.load(Ordering::SeqCst), 0);
        assert_eq!(store.saves.load(Ordering::SeqCst), 0);
        *transport.refresh.lock().unwrap() = Some(Ok(tokens(Some("rotated-refresh-secret"))));
        *transport.identity.lock().unwrap() = Ok(GoogleIdentity::new("wrong-subject").unwrap());
        assert_eq!(
            auth.access_token("work").unwrap_err(),
            AuthError::IdentityMismatch
        );
        let record = store.records.lock().unwrap()["work"].clone();
        assert_eq!(record.identity, GoogleIdentity::new(SUBJECT).unwrap());
        assert_eq!(record.generation, 8);
        let grant = record.grant.unwrap();
        assert_eq!(grant.refresh_token.expose(), "rotated-refresh-secret");
        assert_eq!(grant.client.client_id, client().client_id);
        assert_eq!(grant.client.client_secret.unwrap().expose(), CLIENT_SECRET);
        assert_eq!(store.saves.load(Ordering::SeqCst), 1);
        assert_eq!(
            auth.status("work").unwrap().account_namespace,
            Some(GoogleIdentity::new(SUBJECT).unwrap().namespace().unwrap())
        );
        *transport.refresh.lock().unwrap() = Some(Ok(tokens(None)));
        assert_eq!(
            auth.access_token("work").unwrap_err(),
            AuthError::IdentityMismatch
        );
        assert_eq!(
            store.records.lock().unwrap()["work"].identity,
            GoogleIdentity::new(SUBJECT).unwrap()
        );
        assert_eq!(transport.identities.load(Ordering::SeqCst), 2);
        assert_eq!(store.saves.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn refresh_rejects_explicit_reduced_broad_or_duplicate_scopes() {
        for (scopes, tasks_readonly, openid, userinfo_email, duplicates) in [
            (vec![], false, false, false, false),
            (vec!["openid".into()], false, true, false, false),
            (
                vec!["openid".into(), "openid".into()],
                false,
                true,
                false,
                true,
            ),
            (
                [scopes(), vec!["email".into()]].concat(),
                true,
                true,
                true,
                false,
            ),
        ] {
            let store = connected();
            let before = snapshot(&store);
            let transport = Transport::default();
            let mut response = tokens(Some("rotated-refresh-secret"));
            response.scopes = Some(scopes);
            *transport.refresh.lock().unwrap() = Some(Ok(response));
            assert_eq!(
                Authenticator::new(&store, &transport)
                    .access_token("work")
                    .unwrap_err(),
                AuthError::ScopeMismatch {
                    tasks_readonly,
                    openid,
                    userinfo_email,
                    userinfo_profile: false,
                    unknown_count: 0,
                    duplicates,
                    response_missing: false,
                }
            );
            assert_eq!(snapshot(&store), before);
            assert_eq!(transport.identities.load(Ordering::SeqCst), 0);
            assert_eq!(store.saves.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn scope_mismatch_reports_only_allowlisted_metadata_without_returning_credentials() {
        const UNKNOWN: &str = "canary-unknown-scope-secret";
        for (email, profile) in [
            ("email", "profile"),
            (
                "https://www.googleapis.com/auth/userinfo.email",
                "https://www.googleapis.com/auth/userinfo.profile",
            ),
        ] {
            for (existing, refresh) in [(false, false), (true, false), (true, true)] {
                let store = if existing {
                    connected()
                } else {
                    Store::default()
                };
                let before = snapshot(&store);
                let transport = Transport::default();
                let mut response = tokens(Some("canary-rotated-refresh-secret"));
                response.scopes = Some(
                    [
                        scopes(),
                        vec![email.into(), profile.into(), UNKNOWN.into(), UNKNOWN.into()],
                    ]
                    .concat(),
                );
                let auth = Authenticator::new(&store, &transport);
                let error = if refresh {
                    *transport.refresh.lock().unwrap() = Some(Ok(response));
                    auth.access_token("work").unwrap_err()
                } else {
                    *transport.exchange.lock().unwrap() = Some(Ok(response));
                    auth.login("work", &client(), &Flow::default()).unwrap_err()
                };
                assert_eq!(
                    error,
                    AuthError::ScopeMismatch {
                        tasks_readonly: true,
                        openid: true,
                        userinfo_email: true,
                        userinfo_profile: true,
                        unknown_count: 2,
                        duplicates: true,
                        response_missing: false,
                    }
                );
                assert_eq!(
                    error.to_string(),
                    "Google scope mismatch: tasks_readonly=true, openid=true, userinfo_email=true, userinfo_profile=true, unknown_count=2, duplicates=true, response_missing=false"
                );
                assert_eq!(
                    format!("{error:?}"),
                    "ScopeMismatch { tasks_readonly: true, openid: true, userinfo_email: true, userinfo_profile: true, unknown_count: 2, duplicates: true, response_missing: false }"
                );
                let status = auth.status("work").unwrap();
                let public = format!(
                    "{error}{error:?}{status:?}{}",
                    serde_json::to_string(&status).unwrap()
                );
                for canary in [
                    UNKNOWN,
                    ACCESS,
                    REFRESH,
                    CLIENT_SECRET,
                    SUBJECT,
                    "canary-rotated-refresh-secret",
                    "canary-authorization-code",
                    "canary-pkce-verifier",
                ] {
                    assert!(!public.contains(canary));
                }
                assert_eq!(status.scopes, if existing { scopes() } else { vec![] });
                assert_eq!(snapshot(&store), before);
                assert_eq!(store.saves.load(Ordering::SeqCst), 0);
                assert_eq!(transport.identities.load(Ordering::SeqCst), 0);
                assert_eq!(
                    transport.exchanges.load(Ordering::SeqCst),
                    usize::from(!refresh)
                );
                assert_eq!(
                    transport.refreshes.load(Ordering::SeqCst),
                    usize::from(refresh)
                );
            }
        }
    }

    #[test]
    fn oauth_invalid_scopes_remains_static_and_is_propagated_unchanged() {
        let store = connected();
        let before = snapshot(&store);
        let transport = Transport::default();
        let auth = Authenticator::new(&store, &transport);
        *transport.exchange.lock().unwrap() = Some(Err(AuthError::InvalidScopes));
        *transport.refresh.lock().unwrap() = Some(Err(AuthError::InvalidScopes));
        assert_eq!(
            auth.login("work", &client(), &Flow::default()).unwrap_err(),
            AuthError::InvalidScopes
        );
        assert_eq!(
            auth.access_token("work").unwrap_err(),
            AuthError::InvalidScopes
        );
        assert_eq!(
            AuthError::InvalidScopes.to_string(),
            "Google did not grant the required narrow scopes"
        );
        assert_eq!(snapshot(&store), before);
        assert_eq!(store.saves.load(Ordering::SeqCst), 0);
        assert_eq!(transport.identities.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn public_client_fields_are_validated_before_authorization() {
        let store = Store::default();
        let transport = Transport::default();
        let flow = Flow::default();
        for id in [
            "",
            "fake",
            ".apps.googleusercontent.com",
            "bad\n.apps.googleusercontent.com",
            "a.apps.googleusercontent.com.evil",
            "a/b.apps.googleusercontent.com",
        ] {
            let invalid = ClientConfig {
                client_id: id.into(),
                client_secret: None,
            };
            assert_eq!(
                Authenticator::new(&store, &transport)
                    .login("work", &invalid, &flow)
                    .unwrap_err(),
                AuthError::InvalidClient
            );
        }
        assert_eq!(flow.calls.load(Ordering::SeqCst), 0);
        no_network(&transport);
    }

    #[test]
    fn corrupt_records_fail_closed_before_network() {
        for case in 0..9 {
            let store = connected();
            {
                let mut records = store.records.lock().unwrap();
                let record = records.get_mut("work").unwrap();
                match case {
                    0 => record.version = 2,
                    1 => record.connection = "other".into(),
                    2 => record.generation = 0,
                    3 => record.grant.as_mut().unwrap().client.client_id = "fake".into(),
                    4 => record.grant.as_mut().unwrap().scopes = vec!["openid".into()],
                    5 => {
                        record.identity = serde_json::from_str(
                            r#"{"issuer":"https://evil.invalid","subject":"s"}"#,
                        )
                        .unwrap()
                    }
                    6 => {
                        record.identity = serde_json::from_str(
                            r#"{"issuer":"https://accounts.google.com","subject":""}"#,
                        )
                        .unwrap()
                    }
                    7 => record.grant.as_mut().unwrap().scopes = vec!["openid".into(); 2],
                    _ => {
                        record.grant.as_mut().unwrap().scopes =
                            vec!["canary-unknown-scope-secret".into()]
                    }
                }
            }
            let transport = Transport::default();
            let auth = Authenticator::new(&store, &transport);
            assert_eq!(auth.status("work").unwrap_err(), AuthError::StorageCorrupt);
            assert_eq!(
                auth.access_token("work").unwrap_err(),
                AuthError::StorageCorrupt
            );
            assert_eq!(
                auth.login("work", &client(), &Flow::default()).unwrap_err(),
                AuthError::StorageCorrupt
            );
            assert_eq!(auth.logout("work").unwrap_err(), AuthError::StorageCorrupt);
            no_network(&transport);
        }
    }

    #[test]
    fn generation_overflow_does_not_overwrite() {
        let store = connected();
        store
            .records
            .lock()
            .unwrap()
            .get_mut("work")
            .unwrap()
            .generation = u64::MAX;
        let before = snapshot(&store);
        let transport = Transport::default();
        *transport.refresh.lock().unwrap() = Some(Ok(tokens(Some("rotated-refresh-secret"))));
        let auth = Authenticator::new(&store, &transport);
        assert_eq!(auth.logout("work").unwrap_err(), AuthError::StorageCorrupt);
        assert_eq!(
            auth.login("work", &client(), &Flow::default()).unwrap_err(),
            AuthError::StorageCorrupt
        );
        assert_eq!(
            auth.access_token("work").unwrap_err(),
            AuthError::StorageCorrupt
        );
        assert_eq!(snapshot(&store), before);
        assert_eq!(store.saves.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn refresh_lock_excludes_logout_and_prevents_resurrection() {
        use std::sync::Barrier;
        struct BlockingTransport<'a> {
            inner: Transport,
            entered: &'a Barrier,
            release: &'a Barrier,
        }
        impl OAuthTransport for BlockingTransport<'_> {
            fn exchange(&self, c: &ClientConfig, g: &AuthorizationGrant) -> Result<TokenSet> {
                self.inner.exchange(c, g)
            }
            fn refresh(&self, c: &ClientConfig, r: &SecretString) -> Result<TokenSet> {
                self.entered.wait();
                self.release.wait();
                self.inner.refresh(c, r)
            }
            fn identity(&self, a: &SecretString) -> Result<GoogleIdentity> {
                self.inner.identity(a)
            }
        }
        let store = connected();
        let entered = Barrier::new(2);
        let release = Barrier::new(2);
        let transport = BlockingTransport {
            inner: Transport::default(),
            entered: &entered,
            release: &release,
        };
        *transport.inner.refresh.lock().unwrap() = Some(Ok(tokens(Some("rotated-refresh-secret"))));
        let auth = Authenticator::new(&store, &transport);
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| auth.access_token("work"));
            entered.wait();
            assert_eq!(auth.logout("work").unwrap_err(), AuthError::Busy);
            assert_eq!(auth.access_token("work").unwrap_err(), AuthError::Busy);
            release.wait();
            handle.join().unwrap().unwrap();
        });
        assert!(!auth.logout("work").unwrap().credential_stored);
        assert_eq!(
            auth.access_token("work").unwrap_err(),
            AuthError::NotConnected
        );
        assert!(store.records.lock().unwrap()["work"].grant.is_none());
        assert_eq!(transport.inner.refreshes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn login_holds_lock_during_authorization() {
        struct CheckingFlow<'a>(&'a Store);
        impl AuthorizationFlow for CheckingFlow<'_> {
            fn authorize(&self, client: &ClientConfig) -> Result<AuthorizationGrant> {
                let transport = Transport::default();
                let auth = Authenticator::new(self.0, &transport);
                assert_eq!(auth.logout("work").unwrap_err(), AuthError::Busy);
                assert_eq!(auth.status("work").unwrap_err(), AuthError::Busy);
                Flow::default().authorize(client)
            }
        }
        let store = connected();
        let transport = Transport::default();
        Authenticator::new(&store, &transport)
            .login("work", &client(), &CheckingFlow(&store))
            .unwrap();
        assert!(!store.held.load(Ordering::SeqCst));
    }

    #[test]
    fn secrets_are_redacted_and_status_has_only_allowlisted_fields() {
        let store = connected();
        let transport = Transport::default();
        let auth = Authenticator::new(&store, &transport);
        let status = auth.status("work").unwrap();
        let json = serde_json::to_value(&status).unwrap();
        let mut keys: Vec<_> = json
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
        let attempt = auth.access_token_attempt("work");
        assert!(attempt.outcome.is_ok());
        let grant = Flow::default().authorize(&client()).unwrap();
        let debug = format!(
            "{:?}{:?}{:?}{:?}{:?}",
            client(),
            tokens(Some(REFRESH)),
            grant,
            attempt,
            store.records.lock().unwrap()
        );
        for canary in [
            ACCESS,
            REFRESH,
            CLIENT_SECRET,
            "canary-authorization-code",
            "canary-pkce-verifier",
        ] {
            assert!(!debug.contains(canary));
        }
        let public = format!(
            "{status:?}{json}{}{:?}",
            AuthError::IdentityMismatch,
            AuthError::StorageCorrupt
        );
        for canary in [ACCESS, REFRESH, CLIENT_SECRET, SUBJECT] {
            assert!(!public.contains(canary));
        }
    }
}
