//! Fixed-endpoint Google OAuth transport. Provider responses never become error messages.
use std::{fs::OpenOptions, io::Read, path::Path, time::Duration};

use reqwest::{blocking::Client, redirect::Policy};
use serde::Deserialize;

use crate::{
    AuthError, AuthorizationGrant, ClientConfig, GoogleIdentity, OAuthTransport, Result,
    SecretString, TokenSet,
};

pub(crate) const AUTHORIZATION_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const IDENTITY_ENDPOINT: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const MAX_BYTES: u64 = 64 * 1024;

pub struct GoogleOAuth {
    http: Client,
    timeout: Duration,
    token_endpoint: String,
    identity_endpoint: String,
}

impl GoogleOAuth {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: Self::http(true, Duration::from_secs(30))?,
            timeout: Duration::from_secs(30),
            token_endpoint: TOKEN_ENDPOINT.into(),
            identity_endpoint: IDENTITY_ENDPOINT.into(),
        })
    }

    fn http(https_only: bool, timeout: Duration) -> Result<Client> {
        Client::builder()
            .https_only(https_only)
            .no_proxy()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| AuthError::Transport)
    }

    fn tokens<'a>(
        &self,
        client: &'a ClientConfig,
        mut form: Vec<(&str, &'a str)>,
    ) -> Result<TokenSet> {
        validate_client_id(&client.client_id)?;
        form.push(("client_id", &client.client_id));
        if let Some(secret) = &client.client_secret {
            form.push(("client_secret", secret.expose()));
        }
        let response = self
            .http
            .post(&self.token_endpoint)
            .timeout(self.timeout)
            .form(&form)
            .send()
            .map_err(transport_error)?;
        let status = response.status();
        let bytes = bounded(response)?;
        if !status.is_success() {
            if status.is_redirection() {
                return Err(AuthError::Transport);
            }
            #[derive(Deserialize)]
            struct Failure {
                error: String,
            }
            let failure: Failure =
                serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidResponse)?;
            return Err(match failure.error.as_str() {
                "invalid_grant" => AuthError::ReauthenticationRequired,
                "invalid_client" | "unauthorized_client" => AuthError::InvalidClientGrant,
                "invalid_scope" | "insufficient_scope" => AuthError::InvalidScopes,
                "access_denied" => AuthError::Denied,
                _ => AuthError::Transport,
            });
        }
        #[derive(Deserialize)]
        struct Tokens {
            access_token: String,
            token_type: String,
            expires_in: u64,
            refresh_token: Option<String>,
            scope: Option<String>,
        }
        let tokens: Tokens =
            serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidResponse)?;
        if !tokens.token_type.eq_ignore_ascii_case("Bearer")
            || !(1..=86400).contains(&tokens.expires_in)
        {
            return Err(AuthError::InvalidResponse);
        }
        validate_bearer(&tokens.access_token)?;
        let scopes = tokens
            .scope
            .map(|scope| {
                if scope.is_empty()
                    || !scope.bytes().all(|b| {
                        b == b' '
                            || b == 0x21
                            || (0x23..=0x5b).contains(&b)
                            || (0x5d..=0x7e).contains(&b)
                    })
                {
                    return Err(AuthError::InvalidScopes);
                }
                let values: Vec<String> = scope
                    .split(' ')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                if values.is_empty() {
                    return Err(AuthError::InvalidScopes);
                }
                Ok(values)
            })
            .transpose()?;
        Ok(TokenSet {
            access_token: SecretString::new(tokens.access_token)?,
            refresh_token: tokens.refresh_token.map(SecretString::new).transpose()?,
            expires_in: tokens.expires_in,
            scopes,
        })
    }

    #[cfg(test)]
    fn local(token: &str, identity: &str, timeout: Duration) -> Self {
        for endpoint in [token, identity] {
            let url = reqwest::Url::parse(endpoint).unwrap();
            assert_eq!(url.scheme(), "http");
            assert_eq!(url.host_str(), Some("127.0.0.1"));
            assert!(
                endpoint.starts_with("http://127.0.0.1:")
                    || endpoint.starts_with("http://127.0.0.1/")
            );
            assert!(url.username().is_empty() && url.password().is_none());
            assert!(url.query().is_none() && url.fragment().is_none());
        }
        Self {
            http: Self::http(false, timeout).unwrap(),
            timeout,
            token_endpoint: token.into(),
            identity_endpoint: identity.into(),
        }
    }
}

impl OAuthTransport for GoogleOAuth {
    fn exchange(&self, client: &ClientConfig, grant: &AuthorizationGrant) -> Result<TokenSet> {
        self.tokens(
            client,
            vec![
                ("grant_type", "authorization_code"),
                ("code", grant.code.expose()),
                ("redirect_uri", &grant.redirect_uri),
                ("code_verifier", grant.verifier.expose()),
            ],
        )
    }

    fn refresh(&self, client: &ClientConfig, refresh_token: &SecretString) -> Result<TokenSet> {
        self.tokens(
            client,
            vec![
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.expose()),
            ],
        )
    }

    fn identity(&self, access_token: &SecretString) -> Result<GoogleIdentity> {
        validate_bearer(access_token.expose())?;
        let response = self
            .http
            .get(&self.identity_endpoint)
            .timeout(self.timeout)
            .bearer_auth(access_token.expose())
            .send()
            .map_err(transport_error)?;
        let status = response.status();
        let bytes = bounded(response)?;
        if !status.is_success() {
            return Err(if status.as_u16() == 401 {
                AuthError::ReauthenticationRequired
            } else if status.as_u16() == 403 {
                AuthError::InvalidScopes
            } else {
                AuthError::Transport
            });
        }
        #[derive(Deserialize)]
        struct UserInfo {
            sub: String,
        }
        let info: UserInfo =
            serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidResponse)?;
        GoogleIdentity::new(info.sub).map_err(|_| AuthError::InvalidResponse)
    }
}

fn transport_error(error: reqwest::Error) -> AuthError {
    if error.is_timeout() {
        AuthError::Timeout
    } else {
        AuthError::Transport
    }
}

fn bounded(response: reqwest::blocking::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_BYTES)
    {
        return Err(AuthError::InvalidResponse);
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) || error
                .get_ref()
                .and_then(|source| source.downcast_ref::<reqwest::Error>())
                .is_some_and(reqwest::Error::is_timeout)
            {
                AuthError::Timeout
            } else {
                AuthError::Transport
            }
        })?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(AuthError::InvalidResponse);
    }
    Ok(bytes)
}

fn validate_bearer(token: &str) -> Result<()> {
    let core = token.trim_end_matches('=');
    if core.is_empty()
        || !core
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
    {
        return Err(AuthError::InvalidResponse);
    }
    Ok(())
}

pub(crate) fn validate_client_id(id: &str) -> Result<()> {
    let prefix = id
        .strip_suffix(".apps.googleusercontent.com")
        .ok_or(AuthError::InvalidClient)?;
    if id.len() > 512
        || prefix.is_empty()
        || !prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(AuthError::InvalidClient);
    }
    Ok(())
}

/// Read only the explicitly selected official Desktop client JSON, never discovered credentials.
pub fn read_client_file(path: &Path) -> Result<ClientConfig> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // NONBLOCK avoids hanging on a FIFO before its type can be checked.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    return Err(AuthError::UnsupportedPlatform);
    let file = options.open(path).map_err(|_| AuthError::InvalidClient)?;
    let metadata = file.metadata().map_err(|_| AuthError::InvalidClient)?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(AuthError::InvalidClient);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(AuthError::InvalidClient);
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AuthError::InvalidClient)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(AuthError::InvalidClient);
    }
    #[derive(Deserialize)]
    struct File {
        installed: Installed,
        #[serde(default, deserialize_with = "present")]
        web: bool,
    }
    fn present<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<bool, D::Error> {
        let _ = serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(true)
    }
    #[derive(Deserialize)]
    struct Installed {
        client_id: String,
        client_secret: Option<String>,
    }
    // Endpoints from the file are deliberately ignored; all requests use fixed endpoints.
    let parsed: File = serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidClient)?;
    if parsed.web {
        return Err(AuthError::InvalidClient);
    }
    validate_client_id(&parsed.installed.client_id)?;
    Ok(ClientConfig {
        client_id: parsed.installed.client_id,
        client_secret: parsed
            .installed
            .client_secret
            .map(SecretString::new)
            .transpose()
            .map_err(|_| AuthError::InvalidClient)?,
    })
}

#[cfg(test)]
mod tests;
