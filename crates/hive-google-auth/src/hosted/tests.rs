use super::*;
use crate::{
    AuthorizationFlow, AuthorizationGrant, ClientConfig, GoogleIdentity, OAuthTransport,
    SecretString, TokenSet, provider::Authenticator,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

const SUBJECT: &str = "hosted-subject-canary";
const REFRESH: &str = "hosted-refresh-canary";
const CLIENT: &str = "hosted-client-secret-canary";
const ACCESS: &str = "hosted-access-canary";
const ROTATED: &str = "hosted-rotated-canary";

fn identity() -> GoogleIdentity {
    GoogleIdentity::new(SUBJECT).unwrap()
}

fn wire() -> Value {
    json!({
        "version": 1,
        "connection": "work",
        "identity": identity(),
        "generation": 7,
        "grant": {
            "client": {
                "client_id": "synthetic.apps.googleusercontent.com",
                "client_secret": CLIENT
            },
            "refresh_token": REFRESH,
            "scopes": AuthProfile::TasksWrite.scopes()
        }
    })
}

fn decode(value: &Value) -> Result<ImmutableTasksWriteStore> {
    ImmutableTasksWriteStore::from_record_bytes(
        "work",
        &identity().namespace().unwrap(),
        &serde_json::to_vec(value).unwrap(),
    )
}

fn sanitized(error: AuthError) {
    let rendered = format!("{error} {error:?}");
    for canary in [SUBJECT, REFRESH, CLIENT, ACCESS, ROTATED] {
        assert!(!rendered.contains(canary));
    }
}

#[test]
fn valid_store_is_send_sync_bound_tasks_write_and_loads_independent_clones() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<ImmutableTasksWriteStore>();
    let store = decode(&wire()).unwrap();
    assert_eq!(store.profile(), AuthProfile::TasksWrite);
    let guard = store.lock("work").unwrap();
    let mut record = store.load("work").unwrap().unwrap();
    assert_eq!(record.identity, identity());
    assert_eq!(record.generation, 7);
    assert!(record.grant.as_ref().unwrap().refresh_token.expose() == REFRESH);
    let debug = format!("{store:?} {record:?}");
    for canary in [REFRESH, CLIENT, ACCESS, ROTATED] {
        assert!(!debug.contains(canary));
    }
    assert!(!format!("{store:?}").contains(SUBJECT));
    record.grant = None;
    assert!(store.load("work").unwrap().unwrap().grant.is_some());
    assert!(matches!(
        store.lock("other"),
        Err(AuthError::IdentityMismatch)
    ));
    assert!(matches!(
        store.load("other"),
        Err(AuthError::IdentityMismatch)
    ));
    assert!(matches!(
        store.load("../work"),
        Err(AuthError::InvalidConnection)
    ));
    assert!(matches!(store.lock("work"), Err(AuthError::Busy)));
    drop(guard);
    assert!(store.lock("work").is_ok());
    // No successful in-memory update, even for a tombstone or a wrong alias.
    for alias in ["work", "other", "../work"] {
        let error = store.save(alias, &record).unwrap_err();
        assert_eq!(error, AuthError::CredentialPersistenceRequired);
        sanitized(error);
    }
    assert!(store.load("work").unwrap().unwrap().grant.is_some());
}

#[test]
fn strict_decoder_rejects_corrupt_records_profiles_scopes_and_tombstones() {
    for (pointer, replacement) in [
        ("/version", json!(2)),
        ("/generation", json!(0)),
        ("/generation", json!(-1)),
        ("/connection", json!("other")),
        ("/identity/issuer", json!("https://evil.invalid")),
        ("/identity/subject", json!("")),
        ("/grant/client/client_id", json!("invalid")),
        ("/grant/client/client_secret", json!("")),
        ("/grant/refresh_token", json!("")),
        ("/grant/scopes", json!(AuthProfile::TasksReadonly.scopes())),
        ("/grant/scopes", json!(AuthProfile::CalendarEvents.scopes())),
        ("/grant/scopes", json!([])),
        ("/grant/scopes", json!([crate::TASKS_WRITE_SCOPE])),
        ("/grant/scopes", json!([crate::OPENID_SCOPE])),
        (
            "/grant/scopes",
            json!([crate::TASKS_WRITE_SCOPE, crate::OPENID_SCOPE, "extra"]),
        ),
        (
            "/grant/scopes",
            json!([
                crate::TASKS_WRITE_SCOPE,
                crate::OPENID_SCOPE,
                crate::TASKS_SCOPE
            ]),
        ),
        (
            "/grant/scopes",
            json!([
                crate::TASKS_WRITE_SCOPE,
                crate::OPENID_SCOPE,
                crate::OPENID_SCOPE
            ]),
        ),
    ] {
        let mut value = wire();
        *value.pointer_mut(pointer).unwrap() = replacement;
        let error = decode(&value).unwrap_err();
        assert_eq!(error, AuthError::StorageCorrupt);
        sanitized(error);
    }
    let mut tombstone = wire();
    tombstone["grant"] = Value::Null;
    assert_eq!(decode(&tombstone).unwrap_err(), AuthError::NotConnected);
    let text = serde_json::to_string(&wire()).unwrap();
    let pin = identity().namespace().unwrap();
    for invalid in [
        text.replacen('{', "{\"version\":1,", 1),
        text.replacen('{', "{\"unknown\":null,", 1),
        text.replace("\"client_id\":", "\"unknown\":null,\"client_id\":"),
        text.replace(
            "\"refresh_token\":",
            "\"refresh_token\":\"duplicate\",\"refresh_token\":",
        ),
        text.replace("\"issuer\":", "\"issuer\":\"duplicate\",\"issuer\":"),
        text.replace("\"subject\":", "\"unknown\":null,\"subject\":"),
        format!("{text}{{}}"),
    ] {
        let error = ImmutableTasksWriteStore::from_record_bytes("work", &pin, invalid.as_bytes())
            .unwrap_err();
        assert_eq!(error, AuthError::StorageCorrupt);
        sanitized(error);
    }
    let mut bounded = text.into_bytes();
    bounded.resize(64 * 1024, b' ');
    assert!(ImmutableTasksWriteStore::from_record_bytes("work", &pin, &bounded).is_ok());
    bounded.push(b' ');
    for bytes in [bounded.as_slice(), b"", b"\xff"] {
        assert_eq!(
            ImmutableTasksWriteStore::from_record_bytes("work", &pin, bytes).unwrap_err(),
            AuthError::StorageCorrupt
        );
    }
}

#[test]
fn independently_pinned_canonical_namespace_and_alias_are_required_before_oauth() {
    let bytes = serde_json::to_vec(&wire()).unwrap();
    let pin = identity().namespace().unwrap();
    for wrong in [
        String::new(),
        SUBJECT.into(),
        pin.to_uppercase(),
        format!("{pin}0"),
        format!("google-sub-v1:{}", "a".repeat(63)),
        format!("google-sub-v1:{}", "z".repeat(64)),
        GoogleIdentity::new("other").unwrap().namespace().unwrap(),
    ] {
        let error =
            ImmutableTasksWriteStore::from_record_bytes("work", &wrong, &bytes).unwrap_err();
        assert_eq!(error, AuthError::IdentityMismatch);
        sanitized(error);
    }
    assert_eq!(
        ImmutableTasksWriteStore::from_record_bytes("other", &pin, &bytes).unwrap_err(),
        AuthError::StorageCorrupt
    );
    assert_eq!(
        ImmutableTasksWriteStore::from_record_bytes("../work", &pin, &bytes).unwrap_err(),
        AuthError::InvalidConnection
    );
    let mut swapped = wire();
    swapped["identity"] = serde_json::to_value(GoogleIdentity::new("other").unwrap()).unwrap();
    assert_eq!(decode(&swapped).unwrap_err(), AuthError::IdentityMismatch);
}

struct Fake<'a> {
    store: &'a ImmutableTasksWriteStore,
    rotation: Option<&'static str>,
    scopes: Option<Vec<String>>,
    identity: Result<GoogleIdentity>,
    refreshes: AtomicUsize,
    identities: AtomicUsize,
    exchanges: AtomicUsize,
}

impl<'a> Fake<'a> {
    fn new(store: &'a ImmutableTasksWriteStore, rotation: Option<&'static str>) -> Self {
        Self {
            store,
            rotation,
            scopes: None,
            identity: Ok(identity()),
            refreshes: AtomicUsize::new(0),
            identities: AtomicUsize::new(0),
            exchanges: AtomicUsize::new(0),
        }
    }

    fn tokens(&self) -> TokenSet {
        TokenSet {
            access_token: SecretString::new(ACCESS).unwrap(),
            refresh_token: self.rotation.map(|r| SecretString::new(r).unwrap()),
            expires_in: 3600,
            scopes: self.scopes.clone(),
        }
    }
}

impl OAuthTransport for Fake<'_> {
    fn exchange(&self, _: &ClientConfig, _: &AuthorizationGrant) -> Result<TokenSet> {
        assert!(matches!(self.store.lock("work"), Err(AuthError::Busy)));
        self.exchanges.fetch_add(1, Ordering::SeqCst);
        Ok(self.tokens())
    }

    fn refresh(&self, _: &ClientConfig, refresh: &SecretString) -> Result<TokenSet> {
        assert!(matches!(self.store.lock("work"), Err(AuthError::Busy)));
        assert!(refresh.expose() == REFRESH);
        assert_eq!(self.refreshes.fetch_add(1, Ordering::SeqCst), 0, "no retry");
        Ok(self.tokens())
    }

    fn identity(&self, access: &SecretString) -> Result<GoogleIdentity> {
        assert!(matches!(self.store.lock("work"), Err(AuthError::Busy)));
        assert!(access.expose() == ACCESS);
        self.identities.fetch_add(1, Ordering::SeqCst);
        self.identity.clone()
    }
}

#[test]
fn refresh_omitted_or_identical_succeeds_but_rotation_denies_before_userinfo() {
    for rotation in [None, Some(REFRESH), Some(ROTATED)] {
        for explicit_scopes in [false, true] {
            let store = decode(&wire()).unwrap();
            let mut fake = Fake::new(&store, rotation);
            if explicit_scopes {
                fake.scopes = Some(
                    AuthProfile::TasksWrite
                        .scopes()
                        .iter()
                        .map(|s| (*s).into())
                        .collect(),
                );
            }
            let attempt = Authenticator::new(&store, &fake).access_token_attempt("work");
            assert_eq!(attempt.generation, Some(7));
            if rotation == Some(ROTATED) {
                let error = attempt.outcome.unwrap_err();
                assert_eq!(error, AuthError::CredentialPersistenceRequired);
                sanitized(error);
                assert_eq!(fake.identities.load(Ordering::SeqCst), 0);
            } else {
                let bound = attempt.outcome.unwrap();
                assert_eq!(bound.identity, identity());
                assert!(bound.token.expose() == ACCESS);
                assert_eq!(fake.identities.load(Ordering::SeqCst), 1);
            }
            assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);
            assert_eq!(fake.exchanges.load(Ordering::SeqCst), 0);
            let _guard = store.lock("work").unwrap();
            let record = store.load("work").unwrap().unwrap();
            assert_eq!(record.generation, 7);
            assert_eq!(record.identity, identity());
            assert!(record.grant.unwrap().refresh_token.expose() == REFRESH);
        }
    }
}

#[test]
fn fresh_identity_failure_and_wrong_refresh_scopes_never_release_bearer() {
    for failure in [
        AuthError::IdentityMismatch,
        AuthError::Transport,
        AuthError::InvalidScopes,
    ] {
        let store = decode(&wire()).unwrap();
        let mut fake = Fake::new(&store, None);
        match failure {
            AuthError::IdentityMismatch => {
                fake.identity = Ok(GoogleIdentity::new("other").unwrap())
            }
            AuthError::Transport => fake.identity = Err(AuthError::Transport),
            _ => fake.scopes = Some(vec![crate::TASKS_SCOPE.into(), crate::OPENID_SCOPE.into()]),
        }
        let error = Authenticator::new(&store, &fake)
            .access_token("work")
            .unwrap_err();
        if failure == AuthError::InvalidScopes {
            assert!(matches!(error, AuthError::ScopeMismatch { .. }));
            assert_eq!(fake.identities.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(error, failure);
            assert_eq!(fake.identities.load(Ordering::SeqCst), 1);
        }
        sanitized(error);
        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(fake.exchanges.load(Ordering::SeqCst), 0);
        assert!(store.lock("work").is_ok());
    }
}

struct Flow(AuthProfile);
impl AuthorizationFlow for Flow {
    fn profile(&self) -> AuthProfile {
        self.0
    }
    fn authorize(&self, _: &ClientConfig) -> Result<AuthorizationGrant> {
        assert_eq!(self.0, AuthProfile::TasksWrite);
        Ok(AuthorizationGrant {
            code: SecretString::new("synthetic-code").unwrap(),
            verifier: SecretString::new("synthetic-verifier").unwrap(),
            redirect_uri: "http://127.0.0.1:1234/callback".into(),
        })
    }
}

#[test]
fn lifecycle_updates_cannot_succeed_and_cross_profile_flows_never_authorize() {
    let store = decode(&wire()).unwrap();
    let mut fake = Fake::new(&store, Some(REFRESH));
    fake.scopes = Some(
        AuthProfile::TasksWrite
            .scopes()
            .iter()
            .map(|s| (*s).into())
            .collect(),
    );
    let auth = Authenticator::new(&store, &fake);
    let client = store.load("work").unwrap().unwrap().grant.unwrap().client;
    for profile in [AuthProfile::TasksReadonly, AuthProfile::CalendarEvents] {
        assert_eq!(
            auth.login("work", &client, &Flow(profile)).unwrap_err(),
            AuthError::ProfileMismatch
        );
    }
    assert_eq!(fake.exchanges.load(Ordering::SeqCst), 0);
    assert_eq!(
        auth.logout("work").unwrap_err(),
        AuthError::CredentialPersistenceRequired
    );
    assert_eq!(fake.identities.load(Ordering::SeqCst), 0);
    assert_eq!(
        auth.login("work", &client, &Flow(AuthProfile::TasksWrite))
            .unwrap_err(),
        AuthError::CredentialPersistenceRequired
    );
    assert_eq!(fake.exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(fake.identities.load(Ordering::SeqCst), 1);
    assert_eq!(fake.refreshes.load(Ordering::SeqCst), 0);
    let _guard = store.lock("work").unwrap();
    let record = store.load("work").unwrap().unwrap();
    assert_eq!(record.generation, 7);
    assert!(record.grant.unwrap().refresh_token.expose() == REFRESH);
}

#[test]
fn concurrent_authentication_is_busy_until_guard_release_without_oauth() {
    let store = decode(&wire()).unwrap();
    let fake = Fake::new(&store, None);
    let wrong_alias = Authenticator::new(&store, &fake).access_token_attempt("other");
    assert_eq!(wrong_alias.generation, None);
    assert_eq!(
        wrong_alias.outcome.unwrap_err(),
        AuthError::IdentityMismatch
    );
    let guard = store.lock("work").unwrap();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let auth = Authenticator::new(&store, &fake);
                let attempt = auth.access_token_attempt("work");
                assert_eq!(attempt.generation, None);
                assert_eq!(attempt.outcome.unwrap_err(), AuthError::Busy);
                assert_eq!(auth.logout("work").unwrap_err(), AuthError::Busy);
            })
            .join()
            .unwrap();
    });
    assert_eq!(fake.refreshes.load(Ordering::SeqCst), 0);
    assert_eq!(fake.identities.load(Ordering::SeqCst), 0);
    drop(guard);
    Authenticator::new(&store, &fake)
        .access_token("work")
        .unwrap();
    assert!(store.lock("work").is_ok());
}
