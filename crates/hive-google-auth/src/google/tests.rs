use super::*;
use std::{io::Write, net::TcpListener, sync::mpsc, thread};

const GOOD: &str = r#"{"access_token":"access-canary","token_type":"Bearer","expires_in":3600,"refresh_token":"refresh-canary","scope":"openid https://www.googleapis.com/auth/tasks.readonly","id_token":"unverified.not.identity"}"#;

#[test]
fn reduced_request_budget_preserves_defaults_fixed_endpoints_and_never_extends_deadline() {
    let original = GoogleOAuth::new().unwrap();
    assert_eq!(original.timeout, Duration::from_secs(30));
    assert!(original.request_deadline.is_none());
    assert!(matches!(
        original.with_request_timeout(Duration::ZERO),
        Err(AuthError::Timeout)
    ));
    let reduced = original
        .with_request_timeout(Duration::from_secs(2))
        .unwrap();
    assert_eq!(reduced.token_endpoint, TOKEN_ENDPOINT);
    assert_eq!(reduced.identity_endpoint, IDENTITY_ENDPOINT);
    assert_eq!(reduced.timeout, Duration::from_secs(2));
    let extended = reduced
        .with_request_timeout(Duration::from_secs(60))
        .unwrap();
    assert_eq!(extended.request_deadline, reduced.request_deadline);
    assert!(extended.request_timeout().unwrap() <= Duration::from_secs(2));
    assert_eq!(original.request_timeout().unwrap(), Duration::from_secs(30));
}

#[test]
fn expired_reduced_request_budget_starts_no_refresh_or_userinfo_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/endpoint", listener.local_addr().unwrap());
    let mut oauth = GoogleOAuth::local(&endpoint, &endpoint, Duration::from_secs(2));
    oauth.request_deadline = Some(Instant::now() - Duration::from_secs(1));
    let token = SecretString::new("access-canary").unwrap();
    assert!(matches!(
        oauth.refresh(&client(false), &token),
        Err(AuthError::Timeout)
    ));
    assert!(matches!(oauth.identity(&token), Err(AuthError::Timeout)));
    assert!(listener.accept().is_err());
}

#[test]
fn reduced_timeout_applies_to_refresh_and_userinfo_over_real_loopback_http() {
    for refresh in [false, true] {
        let body = if refresh {
            GOOD
        } else {
            r#"{"sub":"fixture"}"#
        };
        let (original, request, handle) = mock("200 OK", body, "", Duration::from_millis(120));
        let reduced = original
            .with_request_timeout(Duration::from_millis(50))
            .unwrap();
        let token = SecretString::new("access-canary").unwrap();
        let result = if refresh {
            reduced.refresh(&client(false), &token).map(|_| ())
        } else {
            reduced.identity(&token).map(|_| ())
        };
        assert!(matches!(result, Err(AuthError::Timeout)));
        assert!(
            request
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .contains("/endpoint")
        );
        handle.join().unwrap();
        assert!(original.request_deadline.is_none());
    }
}

fn client(secret: bool) -> ClientConfig {
    ClientConfig {
        client_id: "123-desktop.apps.googleusercontent.com".into(),
        client_secret: secret.then(|| SecretString::new("secret&=+canary").unwrap()),
    }
}

fn grant() -> AuthorizationGrant {
    AuthorizationGrant {
        code: SecretString::new("code&=+canary").unwrap(),
        verifier: SecretString::new("verifier&=+canary").unwrap(),
        redirect_uri: "http://127.0.0.1:45231/".into(),
    }
}

fn mock(
    status: &str,
    body: &str,
    headers: &str,
    delay: Duration,
) -> (GoogleOAuth, mpsc::Receiver<String>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/endpoint", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    );
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0u8; 4096];
            let count = stream.read(&mut buffer).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length = header
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .map(|v| v.parse::<usize>().unwrap())
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let _ = sender.send(String::from_utf8(request).unwrap());
        thread::sleep(delay);
        let _ = stream.write_all(response.as_bytes());
    });
    (
        GoogleOAuth::local(&endpoint, &endpoint, Duration::from_millis(150)),
        receiver,
        handle,
    )
}

fn form(request: &str) -> std::collections::HashMap<String, String> {
    let body = request.split_once("\r\n\r\n").unwrap().1;
    reqwest::Url::parse(&format!("http://127.0.0.1/?{body}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[test]
fn exchange_and_refresh_use_encoded_post_forms() {
    for secret in [false, true] {
        let (oauth, request, handle) = mock("200 OK", GOOD, "", Duration::ZERO);
        let tokens = oauth.exchange(&client(secret), &grant()).unwrap();
        assert_eq!(tokens.access_token.expose(), "access-canary");
        assert_eq!(tokens.refresh_token.unwrap().expose(), "refresh-canary");
        assert_eq!(tokens.scopes.unwrap(), ["openid", crate::TASKS_SCOPE]);
        let request = request.recv().unwrap();
        assert!(request.starts_with("POST /endpoint HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/x-www-form-urlencoded")
        );
        let fields = form(&request);
        assert_eq!(fields.len(), if secret { 6 } else { 5 });
        assert_eq!(fields["grant_type"], "authorization_code");
        assert_eq!(fields["client_id"], client(secret).client_id);
        assert_eq!(fields["code"], grant().code.expose());
        assert_eq!(fields["redirect_uri"], grant().redirect_uri);
        assert_eq!(fields["code_verifier"], grant().verifier.expose());
        if secret {
            assert_eq!(fields["client_secret"], "secret&=+canary");
        }
        handle.join().unwrap();

        let (oauth, request, handle) = mock("200 OK", GOOD, "", Duration::ZERO);
        oauth
            .refresh(
                &client(secret),
                &SecretString::new("refresh&=+canary").unwrap(),
            )
            .unwrap();
        let request = request.recv().unwrap();
        assert!(request.starts_with("POST "));
        let fields = form(&request);
        assert_eq!(fields.len(), if secret { 4 } else { 3 });
        assert_eq!(fields["grant_type"], "refresh_token");
        assert_eq!(fields["refresh_token"], "refresh&=+canary");
        assert_eq!(fields["client_id"], client(secret).client_id);
        if secret {
            assert_eq!(fields["client_secret"], "secret&=+canary");
        }
        handle.join().unwrap();
    }
}

#[test]
fn userinfo_is_bearer_get_and_only_subject_is_trusted() {
    let (oauth, request, handle) = mock(
        "200 OK",
        r#"{"sub":"verified-subject","email":"ignore","id_token":"not-trusted"}"#,
        "",
        Duration::ZERO,
    );
    let identity = oauth
        .identity(&SecretString::new("access-canary").unwrap())
        .unwrap();
    assert_eq!(identity.subject(), "verified-subject");
    let request = request.recv().unwrap().to_ascii_lowercase();
    assert!(request.starts_with("get /endpoint http/1.1\r\n"));
    assert!(request.contains("authorization: bearer access-canary\r\n"));
    assert!(!request.contains("scope="));
    handle.join().unwrap();
}

#[test]
fn provider_errors_are_static_and_redacted() {
    for (code, expected) in [
        ("invalid_grant", AuthError::ReauthenticationRequired),
        ("invalid_client", AuthError::InvalidClientGrant),
        ("unauthorized_client", AuthError::InvalidClientGrant),
        ("invalid_scope", AuthError::InvalidScopes),
        ("insufficient_scope", AuthError::InvalidScopes),
        ("access_denied", AuthError::Denied),
        ("CANARY", AuthError::Transport),
    ] {
        let body = format!(
            r#"{{"error":"{code}","error_description":"CANARY https://evil.invalid/?access_token=CANARY"}}"#
        );
        let (oauth, _, handle) = mock("400 Bad Request", &body, "", Duration::ZERO);
        let error = oauth.exchange(&client(true), &grant()).unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error} {error:?}").contains("CANARY"));
        handle.join().unwrap();
    }
    assert!(!format!("{:?}", grant()).contains("canary"));
    assert!(!format!("{:?}", client(true)).contains("secret&"));
}

#[test]
fn malformed_duplicate_and_unbounded_responses_are_rejected() {
    let bodies = [
        "not-json".to_owned(),
        r#"{"access_token":"a","access_token":"b","token_type":"Bearer","expires_in":1}"#.into(),
        r#"{"access_token":"a","token_type":"Bearer","expires_in":1,"expires_in":2}"#.into(),
        r#"{"access_token":"a","token_type":"MAC","expires_in":1}"#.into(),
        r#"{"access_token":"a","token_type":"Bearer","expires_in":0}"#.into(),
        r#"{"access_token":"a","token_type":"Bearer","expires_in":86401}"#.into(),
        r#"{"access_token":"a","token_type":"Bearer","expires_in":1.0}"#.into(),
        r#"{"access_token":"a b","token_type":"Bearer","expires_in":1}"#.into(),
        r#"{"access_token":"a\nCANARY","token_type":"Bearer","expires_in":1}"#.into(),
        "x".repeat(MAX_BYTES as usize + 1),
    ];
    for body in bodies {
        let (oauth, _, handle) = mock("200 OK", &body, "", Duration::ZERO);
        assert_eq!(
            oauth.exchange(&client(false), &grant()).unwrap_err(),
            AuthError::InvalidResponse
        );
        handle.join().unwrap();
    }
    for body in [
        r#"{"sub":"a","sub":"b"}"#,
        r#"{"sub":""}"#,
        r#"{"id_token":"fake"}"#,
        r#"{"sub":"a\u0000"}"#,
    ] {
        let (oauth, _, handle) = mock("200 OK", body, "", Duration::ZERO);
        assert_eq!(
            oauth
                .identity(&SecretString::new("token").unwrap())
                .unwrap_err(),
            AuthError::InvalidResponse
        );
        handle.join().unwrap();
    }
    for scope in ["", "openid\tother", "openid\nother", "\"bad"] {
        let body = serde_json::json!({"access_token":"a", "token_type":"Bearer", "expires_in":1, "scope":scope}).to_string();
        let (oauth, _, handle) = mock("200 OK", &body, "", Duration::ZERO);
        assert_eq!(
            oauth.exchange(&client(false), &grant()).unwrap_err(),
            AuthError::InvalidScopes
        );
        handle.join().unwrap();
    }
}

#[test]
fn redirects_are_not_followed_and_timeout_is_bounded() {
    for identity in [false, true] {
        let (oauth, _, handle) = mock(
            "302 Found",
            "",
            "Location: http://127.0.0.1:1/CANARY\r\n",
            Duration::ZERO,
        );
        let error = if identity {
            oauth
                .identity(&SecretString::new("token").unwrap())
                .unwrap_err()
        } else {
            oauth.exchange(&client(false), &grant()).unwrap_err()
        };
        assert_eq!(error, AuthError::Transport);
        handle.join().unwrap();
    }
    let (oauth, _, handle) = mock("200 OK", GOOD, "", Duration::from_millis(350));
    let start = std::time::Instant::now();
    assert_eq!(
        oauth.exchange(&client(false), &grant()).unwrap_err(),
        AuthError::Timeout
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    handle.join().unwrap();
}

#[test]
fn streaming_responses_are_size_and_total_time_bounded() {
    for slow in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let oauth = GoogleOAuth::local(&endpoint, &endpoint, Duration::from_millis(150));
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&chunk[..count]);
            }
            // No Content-Length, so the streaming bound must apply independently.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .unwrap();
            if slow {
                for _ in 0..20 {
                    if stream.write_all(b" ").is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(35));
                }
            } else {
                let _ = stream.write_all(&vec![b' '; MAX_BYTES as usize + 1]);
            }
        });
        let start = std::time::Instant::now();
        assert_eq!(
            oauth.exchange(&client(false), &grant()).unwrap_err(),
            if slow {
                AuthError::Timeout
            } else {
                AuthError::InvalidResponse
            }
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        handle.join().unwrap();
    }
}

#[test]
fn optional_fields_expiry_boundaries_and_userinfo_statuses() {
    for expires in [1, 86400] {
        let body = serde_json::json!({"access_token":"a+/=", "token_type":"bearer", "expires_in":expires, "id_token":"never.decode.me"}).to_string();
        let (oauth, _, handle) = mock("200 OK", &body, "", Duration::ZERO);
        let tokens = oauth
            .refresh(&client(false), &SecretString::new("refresh").unwrap())
            .unwrap();
        assert!(tokens.scopes.is_none());
        assert!(tokens.refresh_token.is_none());
        assert_eq!(tokens.expires_in, expires);
        handle.join().unwrap();
    }
    for (status, expected) in [
        ("401 Unauthorized", AuthError::ReauthenticationRequired),
        ("403 Forbidden", AuthError::InvalidScopes),
        ("500 Internal Server Error", AuthError::Transport),
    ] {
        let (oauth, _, handle) = mock(status, "CANARY", "", Duration::ZERO);
        assert_eq!(
            oauth
                .identity(&SecretString::new("token").unwrap())
                .unwrap_err(),
            expected
        );
        handle.join().unwrap();
    }
    let (oauth, _, handle) = mock(
        "400 Bad Request",
        r#"{"error":"invalid_grant","error":"invalid_client"}"#,
        "",
        Duration::ZERO,
    );
    assert_eq!(
        oauth
            .refresh(&client(false), &SecretString::new("refresh").unwrap())
            .unwrap_err(),
        AuthError::InvalidResponse
    );
    handle.join().unwrap();
}

#[test]
fn credential_files_are_explicit_desktop_json_and_never_supply_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("client.json");
    for secret in [false, true] {
        let mut installed = serde_json::json!({"client_id":client(false).client_id, "auth_uri":"https://evil.invalid/", "token_uri":"file:///CANARY"});
        if secret {
            installed["client_secret"] = "private-canary".into();
        }
        std::fs::write(
            &path,
            serde_json::json!({"installed":installed}).to_string(),
        )
        .unwrap();
        let parsed = read_client_file(&path).unwrap();
        assert_eq!(parsed.client_id, client(false).client_id);
        assert_eq!(parsed.client_secret.is_some(), secret);
    }
    for body in [
        r#"{"web":{"client_id":"123.apps.googleusercontent.com"}}"#,
        r#"{"installed":{"client_id":"123.apps.googleusercontent.com"},"web":{}}"#,
        r#"{"installed":{"client_id":"123.apps.googleusercontent.com"},"web":null}"#,
        r#"{"installed":{"client_id":"123.apps.googleusercontent.com","client_id":"x"}}"#,
        r#"{"installed":{"client_id":"evil.invalid"}}"#,
        r#"{"installed":{"client_id":".apps.googleusercontent.com"}}"#,
        r#"{"installed":{"client_id":"123\n.apps.googleusercontent.com"}}"#,
        r#"{"installed":{"client_id":"123.apps.googleusercontent.com","client_secret":"\u0000"}}"#,
        "installed:\n client_id: no",
        "{}",
    ] {
        std::fs::write(&path, body).unwrap();
        assert_eq!(
            read_client_file(&path).unwrap_err(),
            AuthError::InvalidClient
        );
    }
    std::fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
    assert_eq!(
        read_client_file(&path).unwrap_err(),
        AuthError::InvalidClient
    );
    assert_eq!(
        read_client_file(dir.path()).unwrap_err(),
        AuthError::InvalidClient
    );
    assert_eq!(
        read_client_file(&dir.path().join("absent")).unwrap_err(),
        AuthError::InvalidClient
    );
    #[cfg(unix)]
    {
        // Valid targets ensure these assertions exercise link rejection, not JSON rejection.
        std::fs::write(
            &path,
            serde_json::json!({"installed":{"client_id":client(false).client_id}}).to_string(),
        )
        .unwrap();
        let symlink = dir.path().join("symlink");
        std::os::unix::fs::symlink(&path, &symlink).unwrap();
        assert_eq!(
            read_client_file(&symlink).unwrap_err(),
            AuthError::InvalidClient
        );
        let hardlink = dir.path().join("hardlink");
        std::fs::hard_link(&path, &hardlink).unwrap();
        assert_eq!(
            read_client_file(&hardlink).unwrap_err(),
            AuthError::InvalidClient
        );
        assert_eq!(
            read_client_file(&path).unwrap_err(),
            AuthError::InvalidClient
        );
        let fifo = dir.path().join("fifo");
        use std::os::unix::ffi::OsStrExt;
        let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the CString is NUL-terminated and lives through the call.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert_eq!(
            read_client_file(&fifo).unwrap_err(),
            AuthError::InvalidClient
        );
    }
}

#[test]
fn local_override_rejects_nonliteral_or_credential_bearing_endpoints() {
    for endpoint in [
        "https://127.0.0.1/token",
        "http://localhost/token",
        "http://2130706433/token",
        "http://example.com/token",
        "http://user@127.0.0.1/token",
        "http://127.0.0.1/token?q=1",
        "http://127.0.0.1/token#f",
    ] {
        assert!(
            std::panic::catch_unwind(|| GoogleOAuth::local(
                endpoint,
                endpoint,
                Duration::from_secs(1)
            ))
            .is_err()
        );
    }
}
