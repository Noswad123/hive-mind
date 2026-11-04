//! Desktop Google credentials: no plaintext fallback, implicit login, or secret CLI output.
pub mod google;
pub mod loopback;
pub mod provider;
pub mod storage;

pub use hive_core::GoogleIdentity;
use serde::Serialize;
use std::{fmt, path::Path};
use zeroize::Zeroizing;

pub const TASKS_SCOPE: &str = "https://www.googleapis.com/auth/tasks.readonly";
pub const TASKS_WRITE_SCOPE: &str = "https://www.googleapis.com/auth/tasks";
pub const OPENID_SCOPE: &str = "openid";
pub const REQUIRED_SCOPES: &[&str] = &[TASKS_SCOPE, OPENID_SCOPE];
pub const CALENDAR_EVENTS_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AuthProfile {
    #[default]
    TasksReadonly,
    TasksWrite,
    CalendarEvents,
}

impl AuthProfile {
    pub fn scopes(self) -> &'static [&'static str] {
        match self {
            Self::TasksReadonly => REQUIRED_SCOPES,
            Self::TasksWrite => &[TASKS_WRITE_SCOPE, OPENID_SCOPE],
            Self::CalendarEvents => &[CALENDAR_EVENTS_SCOPE, OPENID_SCOPE],
        }
    }

    pub(crate) fn accepts_scopes(self, scopes: &[String]) -> bool {
        scopes.len() == self.scopes().len()
            && self.scopes().iter().all(|required| {
                scopes
                    .iter()
                    .filter(|scope| scope.as_str() == *required)
                    .count()
                    == 1
            })
    }

    pub(crate) fn service(self) -> &'static str {
        match self {
            Self::TasksReadonly => "hive/google-captures/v1",
            Self::TasksWrite => "hive/google-tasks-write/v1",
            Self::CalendarEvents => "hive/google-calendar/v1",
        }
    }

    pub(crate) fn lock_namespace(self) -> &'static str {
        match self {
            Self::TasksReadonly => "google-auth",
            Self::TasksWrite => "google-tasks-write-auth",
            Self::CalendarEvents => "google-calendar-auth",
        }
    }
}
pub type Result<T> = std::result::Result<T, AuthError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("Google authorization flow and credential store profiles do not match")]
    ProfileMismatch,
    #[error("invalid connection name; use 1..64 lowercase letters, digits, '-' or '_'")]
    InvalidConnection,
    #[error("invalid Google Desktop OAuth client file")]
    InvalidClient,
    #[error("Google authorization was denied")]
    Denied,
    #[error("Google callback is invalid")]
    InvalidCallback,
    #[error("Google sign-in timed out")]
    Timeout,
    #[error("could not start the loopback callback listener")]
    ListenerUnavailable,
    #[error("could not open the system browser")]
    BrowserUnavailable,
    #[error("Google authorization transport failed; no automatic retry")]
    Transport,
    #[error("Google response is invalid")]
    InvalidResponse,
    #[error("Google authorization expired or was revoked; explicitly sign in again")]
    ReauthenticationRequired,
    #[error("Google OAuth client was rejected")]
    InvalidClientGrant,
    #[error("Google did not grant the required narrow scopes")]
    InvalidScopes,
    #[error(
        "Google scope mismatch: tasks_readonly={tasks_readonly}, tasks_write={tasks_write}, calendar_events={calendar_events}, openid={openid}, userinfo_email={userinfo_email}, userinfo_profile={userinfo_profile}, unknown_count={unknown_count}, duplicates={duplicates}, response_missing={response_missing}"
    )]
    ScopeMismatch {
        tasks_readonly: bool,
        tasks_write: bool,
        calendar_events: bool,
        openid: bool,
        userinfo_email: bool,
        userinfo_profile: bool,
        unknown_count: usize,
        duplicates: bool,
        response_missing: bool,
    },
    #[error("Google account identity does not match this connection")]
    IdentityMismatch,
    #[error("Google connection is not signed in; no credential fallback")]
    NotConnected,
    #[error("secure credential storage is unavailable on this platform; no plaintext fallback")]
    UnsupportedPlatform,
    #[error("secure credential access was denied or cancelled")]
    StorageDenied,
    #[error("secure credential storage is locked or interaction is unavailable")]
    StorageLocked,
    #[error("secure credential storage failed")]
    StorageUnavailable,
    #[error("stored Google connection is corrupt or unsupported")]
    StorageCorrupt,
    #[error("another Google credential operation is active; try again after it finishes")]
    Busy,
}

#[derive(Clone)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 16 * 1024 || value.chars().any(char::is_control)
        {
            return Err(AuthError::InvalidResponse);
        }
        Ok(Self(Zeroizing::new(value)))
    }
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: Option<SecretString>,
}

impl ClientConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        google::read_client_file(path.as_ref())
    }
}

#[derive(Debug)]
pub struct AuthorizationGrant {
    pub code: SecretString,
    pub verifier: SecretString,
    pub redirect_uri: String,
}

#[derive(Debug)]
pub struct TokenSet {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub expires_in: u64,
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct GrantRecord {
    pub client: ClientConfig,
    pub refresh_token: SecretString,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StoredConnection {
    pub version: u32,
    pub connection: String,
    pub identity: GoogleIdentity,
    pub generation: u64,
    // Logout clears the grant but retains the account binding as a tombstone.
    pub grant: Option<GrantRecord>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatus {
    pub schema_version: &'static str,
    pub connection: String,
    pub credential_stored: bool,
    pub account_namespace: Option<String>,
    pub scopes: Vec<String>,
}

#[derive(Debug)]
pub struct BoundAccessToken {
    pub token: SecretString,
    pub identity: GoogleIdentity,
}

/// An access-token outcome attributed to the validated, persisted credential generation.
#[derive(Debug)]
pub struct AccessTokenAttempt {
    pub outcome: Result<BoundAccessToken>,
    pub generation: Option<u64>,
}

pub trait CredentialGuard {}
pub trait CredentialStore: Send + Sync {
    fn profile(&self) -> AuthProfile {
        AuthProfile::TasksReadonly
    }
    fn lock(&self, connection: &str) -> Result<Box<dyn CredentialGuard + '_>>;
    fn load(&self, connection: &str) -> Result<Option<StoredConnection>>;
    fn save(&self, connection: &str, record: &StoredConnection) -> Result<()>;
}

pub trait OAuthTransport: Send + Sync {
    fn exchange(&self, client: &ClientConfig, grant: &AuthorizationGrant) -> Result<TokenSet>;
    fn refresh(&self, client: &ClientConfig, refresh_token: &SecretString) -> Result<TokenSet>;
    fn identity(&self, access_token: &SecretString) -> Result<GoogleIdentity>;
}

pub trait AuthorizationFlow {
    fn profile(&self) -> AuthProfile {
        AuthProfile::TasksReadonly
    }
    fn authorize(&self, client: &ClientConfig) -> Result<AuthorizationGrant>;
}

pub fn validate_connection(connection: &str) -> Result<()> {
    if connection.is_empty()
        || connection.len() > 64
        || !connection
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
    {
        return Err(AuthError::InvalidConnection);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_accept_only_their_exact_scope_set() {
        assert_eq!(AuthProfile::default(), AuthProfile::TasksReadonly);
        assert_eq!(REQUIRED_SCOPES, &[TASKS_SCOPE, OPENID_SCOPE]);
        assert_eq!(
            AuthProfile::TasksWrite.scopes(),
            &[TASKS_WRITE_SCOPE, OPENID_SCOPE]
        );
        assert_eq!(
            AuthProfile::TasksWrite.service(),
            "hive/google-tasks-write/v1"
        );
        assert_eq!(
            AuthProfile::TasksWrite.lock_namespace(),
            "google-tasks-write-auth"
        );
        let profiles = [
            AuthProfile::TasksReadonly,
            AuthProfile::TasksWrite,
            AuthProfile::CalendarEvents,
        ];
        for profile in profiles {
            let exact: Vec<String> = profile.scopes().iter().map(|s| (*s).into()).collect();
            assert!(profile.accepts_scopes(&exact));
            assert!(profile.accepts_scopes(&exact.iter().rev().cloned().collect::<Vec<_>>()));
            for invalid in [
                vec![],
                vec![exact[0].clone()],
                vec![OPENID_SCOPE.into()],
                vec![exact[0].clone(), exact[0].clone()],
                [exact.clone(), vec![OPENID_SCOPE.into()]].concat(),
                [
                    exact.clone(),
                    vec!["https://www.googleapis.com/auth/calendar".into()],
                ]
                .concat(),
            ] {
                assert!(!profile.accepts_scopes(&invalid));
            }
            for other in profiles.into_iter().filter(|other| *other != profile) {
                let wrong: Vec<String> = other.scopes().iter().map(|s| (*s).into()).collect();
                assert!(!profile.accepts_scopes(&wrong));
                assert!(!profile.accepts_scopes(&[exact.clone(), vec![wrong[0].clone()]].concat()));
                assert_ne!(profile.service(), other.service());
                assert_ne!(profile.lock_namespace(), other.lock_namespace());
            }
        }
    }
}
