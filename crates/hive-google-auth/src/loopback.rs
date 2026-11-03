//! One-shot loopback authorization with PKCE and no credential-bearing browser arguments.
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::google::{AUTHORIZATION_ENDPOINT, validate_client_id};
use crate::{
    AuthError, AuthProfile, AuthorizationFlow, AuthorizationGrant, ClientConfig, Result,
    SecretString,
};

// Use the root loopback form supported by Google's Desktop client examples.
const CALLBACK_PATH: &str = "/";
const HEADER_LIMIT: usize = 8 * 1024;
const CONNECTION_LIMIT: usize = 16;

pub trait BrowserLauncher {
    fn open(&self, url: &str) -> Result<()>;
}

pub struct SystemBrowser;

impl BrowserLauncher for SystemBrowser {
    fn open(&self, url: &str) -> Result<()> {
        let parsed = reqwest::Url::parse(url).map_err(|_| AuthError::BrowserUnavailable)?;
        if parsed.scheme() != "https"
            || parsed.host_str() != Some("accounts.google.com")
            || parsed.path() != "/o/oauth2/v2/auth"
            || parsed.port().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
            || parsed.query_pairs().any(|(key, _)| {
                matches!(
                    key.as_ref(),
                    "code"
                        | "code_verifier"
                        | "client_secret"
                        | "access_token"
                        | "refresh_token"
                        | "id_token"
                )
            })
        {
            return Err(AuthError::BrowserUnavailable);
        }
        #[cfg(target_os = "macos")]
        {
            let status = browser_command(url)
                .status()
                .map_err(|_| AuthError::BrowserUnavailable)?;
            if status.success() {
                Ok(())
            } else {
                Err(AuthError::BrowserUnavailable)
            }
        }
        #[cfg(not(target_os = "macos"))]
        Err(AuthError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "macos", test))]
fn browser_command(url: &str) -> std::process::Command {
    let mut command = isolated_launch_command("/usr/bin/open");
    command.arg(url);
    command
}

#[cfg(any(target_os = "macos", test))]
fn isolated_launch_command(program: &str) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    // LaunchServices needs no inherited shell environment, which may contain credentials.
    command
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

pub struct LoopbackFlow {
    profile: AuthProfile,
    browser: Box<dyn BrowserLauncher>,
    timeout: Duration,
    connection_timeout: Duration,
}

impl Default for LoopbackFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopbackFlow {
    pub fn new() -> Self {
        Self::for_profile(AuthProfile::TasksReadonly)
    }

    pub fn for_profile(profile: AuthProfile) -> Self {
        Self {
            profile,
            browser: Box::new(SystemBrowser),
            timeout: Duration::from_secs(300),
            connection_timeout: Duration::from_secs(2),
        }
    }
}

impl AuthorizationFlow for LoopbackFlow {
    fn profile(&self) -> AuthProfile {
        self.profile
    }

    fn authorize(&self, client: &ClientConfig) -> Result<AuthorizationGrant> {
        validate_client_id(&client.client_id)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| AuthError::ListenerUnavailable)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| AuthError::ListenerUnavailable)?;
        let address = listener
            .local_addr()
            .map_err(|_| AuthError::ListenerUnavailable)?;
        let redirect_uri = format!("http://{address}{CALLBACK_PATH}");
        let mut random_state = [0u8; 32];
        let mut random_verifier = [0u8; 32];
        getrandom::fill(&mut random_state).map_err(|_| AuthError::ListenerUnavailable)?;
        getrandom::fill(&mut random_verifier).map_err(|_| AuthError::ListenerUnavailable)?;
        let state = URL_SAFE_NO_PAD.encode(random_state);
        let verifier = SecretString::new(URL_SAFE_NO_PAD.encode(random_verifier))?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.expose().as_bytes()));
        let mut url =
            reqwest::Url::parse(AUTHORIZATION_ENDPOINT).map_err(|_| AuthError::InvalidClient)?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", client.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", &self.profile.scopes().join(" ")),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent select_account"),
            ("include_granted_scopes", "false"),
        ]);
        let deadline = Instant::now() + self.timeout;
        self.browser.open(url.as_str())?;
        let mut accepted = 0;
        loop {
            if Instant::now() >= deadline {
                return Err(AuthError::Timeout);
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    accepted += 1;
                    // Accepted sockets need explicit blocking mode on every platform.
                    let connection_deadline =
                        deadline.min(Instant::now() + self.connection_timeout);
                    let callback = if stream.set_nonblocking(false).is_ok() {
                        receive(
                            &mut stream,
                            connection_deadline,
                            &address.to_string(),
                            &state,
                        )
                    } else {
                        None
                    };
                    respond(
                        &mut stream,
                        callback.as_ref().is_some_and(|result| result.is_ok()),
                        connection_deadline,
                    );
                    if let Some(result) = callback {
                        return result.map(|code| AuthorizationGrant {
                            code,
                            verifier,
                            redirect_uri,
                        });
                    }
                    if accepted >= CONNECTION_LIMIT {
                        return Err(AuthError::InvalidCallback);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(
                        Duration::from_millis(5)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(_) => return Err(AuthError::ListenerUnavailable),
            }
        }
    }
}

fn receive(
    stream: &mut TcpStream,
    deadline: Instant,
    host: &str,
    state: &str,
) -> Option<Result<SecretString>> {
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        stream.set_read_timeout(Some(remaining)).ok()?;
        let mut chunk = [0u8; 1024];
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > HEADER_LIMIT {
            return None;
        }
        if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            if end + 4 != bytes.len() {
                return None;
            }
            return parse_callback(&bytes[..end], host, state);
        }
    }
}

// None means an unrelated/invalid request, which must not consume the pending grant.
fn parse_callback(bytes: &[u8], host: &str, expected_state: &str) -> Option<Result<SecretString>> {
    let request = std::str::from_utf8(bytes).ok()?;
    let mut lines = request.split("\r\n");
    let first = lines.next()?;
    let parts: Vec<_> = first.split(' ').collect();
    if parts.len() != 3 || parts[0] != "GET" || parts[2] != "HTTP/1.1" {
        return None;
    }
    let mut seen_host = false;
    let mut seen_length = false;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.bytes().any(|b| b < 0x20 || b == 0x7f)
        {
            return None;
        }
        let value = value.trim_matches(' ');
        if name.eq_ignore_ascii_case("host") {
            if seen_host || value != host {
                return None;
            }
            seen_host = true;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return None;
        } else if name.eq_ignore_ascii_case("content-length") {
            if seen_length || value != "0" {
                return None;
            }
            seen_length = true;
        }
    }
    if !seen_host {
        return None;
    }
    let (path, query) = parts[1].split_once('?')?;
    if path != CALLBACK_PATH || query.contains('#') {
        return None;
    }
    let mut parameters = std::collections::HashMap::new();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        let key = decode(key)?;
        let value = decode(value)?;
        if parameters.insert(key, value).is_some() {
            return None;
        }
    }
    let state = parameters.get("state")?;
    if !bool::from(state.as_bytes().ct_eq(expected_state.as_bytes())) {
        return None;
    }
    match (parameters.get("code"), parameters.get("error")) {
        (Some(code), None) => SecretString::new(code.clone()).ok().map(Ok),
        (None, Some(error)) => Some(Err(if error == "access_denied" {
            AuthError::Denied
        } else {
            AuthError::InvalidCallback
        })),
        _ => None,
    }
}

fn decode(value: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(match byte {
            b'%' => {
                let high = (bytes.next()? as char).to_digit(16)?;
                let low = (bytes.next()? as char).to_digit(16)?;
                ((high << 4) | low) as u8
            }
            b'+' => b' ',
            other => other,
        });
    }
    let decoded = String::from_utf8(decoded).ok()?;
    if decoded.chars().any(char::is_control) {
        return None;
    }
    Some(decoded)
}

fn respond(stream: &mut TcpStream, success: bool, deadline: Instant) {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return;
    };
    if stream.set_write_timeout(Some(remaining)).is_err() {
        return;
    }
    let (status, body) = if success {
        (
            "200 OK",
            "Authorization response received, return to terminal.",
        )
    } else {
        (
            "400 Bad Request",
            "Invalid authorization response. Return to terminal.",
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nContent-Security-Policy: default-src 'none'\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

#[cfg(test)]
mod tests;
