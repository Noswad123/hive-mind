use super::*;
use std::{
    sync::{Arc, Mutex},
    thread,
};

struct FakeBrowser<F>(F);
impl<F: Fn(&str) -> Result<()>> BrowserLauncher for FakeBrowser<F> {
    fn open(&self, url: &str) -> Result<()> {
        (self.0)(url)
    }
}

fn flow<F: Fn(&str) -> Result<()> + 'static>(browser: F) -> LoopbackFlow {
    LoopbackFlow {
        browser: Box::new(FakeBrowser(browser)),
        timeout: Duration::from_secs(5),
        connection_timeout: Duration::from_secs(2),
    }
}

fn client() -> ClientConfig {
    ClientConfig {
        client_id: "123-test.apps.googleusercontent.com".into(),
        client_secret: Some(SecretString::new("SECRET_CANARY").unwrap()),
    }
}

fn parameters(url: &str) -> (String, String, String) {
    let url = reqwest::Url::parse(url).unwrap();
    let fields: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    let redirect = reqwest::Url::parse(&fields["redirect_uri"]).unwrap();
    (
        format!("127.0.0.1:{}", redirect.port().unwrap()),
        fields["state"].clone(),
        fields["code_challenge"].clone(),
    )
}

fn send(host: &str, request: &str) -> String {
    let mut stream = TcpStream::connect(host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response
}

fn request(host: &str, query: &str) -> String {
    format!("GET {CALLBACK_PATH}?{query} HTTP/1.1\r\nHost: {host}\r\n\r\n")
}

#[test]
fn full_flow_binds_before_browser_and_uses_pkce_and_exact_scopes() {
    let observed = Arc::new(Mutex::new(None));
    let copy = observed.clone();
    let worker = Arc::new(Mutex::new(None));
    let worker_copy = worker.clone();
    let flow = flow(move |url| {
        assert!(url.starts_with(AUTHORIZATION_ENDPOINT));
        assert!(!url.contains("SECRET_CANARY"));
        let parsed = reqwest::Url::parse(url).unwrap();
        let fields: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(fields.len(), 10);
        assert_eq!(fields["scope"], REQUIRED_SCOPES.join(" "));
        assert_eq!(fields["prompt"], "consent select_account");
        assert_eq!(fields["response_type"], "code");
        assert_eq!(fields["access_type"], "offline");
        assert_eq!(fields["include_granted_scopes"], "false");
        assert_eq!(fields["code_challenge_method"], "S256");
        let (host, state, challenge) = parameters(url);
        assert_eq!(URL_SAFE_NO_PAD.decode(&state).unwrap().len(), 32);
        *copy.lock().unwrap() = Some((host.clone(), challenge));
        // The connection must already be possible while open() is running.
        let mut stream = TcpStream::connect(&host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        *worker_copy.lock().unwrap() = Some(thread::spawn(move || {
            stream
                .write_all(
                    request(&host, &format!("state={state}&code=CODE_CANARY%2B%26%3D")).as_bytes(),
                )
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200 OK"));
            assert!(response.contains("Authorization response received, return to terminal."));
            assert!(response.contains("Content-Security-Policy: default-src 'none'"));
            assert!(response.contains("Cache-Control: no-store"));
            assert!(response.contains("Referrer-Policy: no-referrer"));
            assert!(!response.contains("CODE_CANARY"));
            assert!(!response.contains(&state));
            assert!(!response.contains("Keychain"));
        }));
        Ok(())
    });
    let grant = flow.authorize(&client()).unwrap();
    worker.lock().unwrap().take().unwrap().join().unwrap();
    assert_eq!(grant.code.expose(), "CODE_CANARY+&=");
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(grant.verifier.expose())
            .unwrap()
            .len(),
        32
    );
    let (host, challenge) = observed.lock().unwrap().clone().unwrap();
    assert_eq!(
        challenge,
        URL_SAFE_NO_PAD.encode(Sha256::digest(grant.verifier.expose().as_bytes()))
    );
    assert_eq!(grant.redirect_uri, format!("http://{host}{CALLBACK_PATH}"));
    assert!(
        TcpStream::connect(host).is_err(),
        "one-shot listener must be closed against replay"
    );
    assert!(!format!("{grant:?}").contains("CODE_CANARY"));
}

#[test]
fn invalid_requests_do_not_consume_a_valid_callback() {
    let worker = Arc::new(Mutex::new(None));
    let copy = worker.clone();
    let flow = flow(move |url| {
        let (host, state, _) = parameters(url);
        *copy.lock().unwrap() = Some(thread::spawn(move || {
            let bad = [
                request(&host, "state=wrong&code=CANARY"),
                request(&host, "code=CANARY"),
                request(&host, &format!("state={state}&code=a&code=b")),
                request(&host, &format!("state={state}&code=a&error=access_denied")),
                request(&host, &format!("state={state}&code=%GG")),
                request(&host, &format!("state={state}&code=%FF")),
                request(&host, &format!("state={state}&code=%00")),
                request(&host, &format!("state={state}&code=%0A")),
                request(&host, &format!("state={state}&code=x")).replace("GET ", "POST "),
                request(&host, &format!("state={state}&code=x"))
                    .replace(CALLBACK_PATH, "/favicon.ico"),
                request(&host, &format!("state={state}&code=x"))
                    .replace("\r\n\r\n", &format!("\r\nHost: {host}\r\n\r\n")),
                request(&host, &format!("state={state}&code=x"))
                    .replace("\r\n\r\n", "\r\nTransfer-Encoding: chunked\r\n\r\n"),
                request(&host, &format!("state={state}&code=x"))
                    .replace("\r\n\r\n", "\r\nContent-Length: 1\r\n\r\nx"),
            ];
            for request in bad {
                let response = send(&host, &request);
                assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
                assert!(!response.contains("CANARY"));
            }
            assert!(
                send(&host, &request(&host, &format!("state={state}&code=valid")))
                    .starts_with("HTTP/1.1 200 OK")
            );
        }));
        Ok(())
    });
    assert_eq!(flow.authorize(&client()).unwrap().code.expose(), "valid");
    worker.lock().unwrap().take().unwrap().join().unwrap();
}

#[test]
fn parser_rejects_ambiguous_headers_state_and_encoding() {
    let host = "127.0.0.1:1234";
    let valid = request(host, "state=expected&code=code");
    for bad in [
        valid.replace("HTTP/1.1", "HTTP/1.0"),
        valid.replace(host, "localhost:1234"),
        valid.replace("Host:", "Host :"),
        valid.replace("Host:", " Host:"),
        valid.replace("code=code", "code=%"),
        valid.replace("code=code", "code=%A"),
        valid.replace("code=code", "code=%7F"),
        valid.replace("code=code", "code=%C2%85"),
        valid.replace("code=code", "code=code#fragment"),
        valid.replace("code=code", "code=code&state=expected"),
        valid.replace("code=code", "code=code&%73tate=expected"),
        valid.replace("code=code", "code=code&error=a&error=b"),
        valid.replace("state=expected", "state=wrong&error=access_denied"),
        valid.replace(
            "\r\n\r\n",
            "\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
        ),
        valid.replace("\r\n\r\n", "\r\nX-Test: nul\0\r\n\r\n"),
        valid.replace("\r\n\r\n", "\r\n folded\r\n\r\n"),
        valid.replace("\r\nHost: 127.0.0.1:1234", ""),
    ] {
        assert!(
            parse_callback(
                bad.trim_end_matches("\r\n\r\n").as_bytes(),
                host,
                "expected"
            )
            .is_none(),
            "accepted {bad:?}"
        );
    }
}

#[test]
fn errors_are_handled_only_after_valid_state() {
    for (error, expected) in [
        ("access_denied", AuthError::Denied),
        ("CANARY", AuthError::InvalidCallback),
    ] {
        let worker = Arc::new(Mutex::new(None));
        let copy = worker.clone();
        let flow = flow(move |url| {
            let (host, state, _) = parameters(url);
            *copy.lock().unwrap() = Some(thread::spawn(move || {
                assert!(
                    send(
                        &host,
                        &request(&host, &format!("state=wrong&error={error}"))
                    )
                    .starts_with("HTTP/1.1 400")
                );
                let response = send(
                    &host,
                    &request(&host, &format!("state={state}&error={error}")),
                );
                assert!(!response.contains("CANARY"));
            }));
            Ok(())
        });
        assert_eq!(flow.authorize(&client()).unwrap_err(), expected);
        worker.lock().unwrap().take().unwrap().join().unwrap();
    }
}

#[test]
fn header_connection_and_time_limits_are_bounded() {
    let worker = Arc::new(Mutex::new(None));
    let copy = worker.clone();
    let mut flow = flow(move |url| {
        let (host, state, _) = parameters(url);
        *copy.lock().unwrap() = Some(thread::spawn(move || {
            let large = format!(
                "GET / HTTP/1.1\r\nHost: {host}\r\nX-Large: {}\r\n\r\n",
                "x".repeat(HEADER_LIMIT)
            );
            let _ = send(&host, &large);
            // Slow clients cannot monopolize the overall deadline.
            let mut slow = TcpStream::connect(&host).unwrap();
            slow.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            slow.write_all(b"GET /?state=").unwrap();
            let mut byte = [0];
            match slow.read(&mut byte) {
                Ok(0) => {}
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                other => panic!("incomplete connection was not closed: {other:?}"),
            }
            assert!(
                send(&host, &request(&host, &format!("state={state}&code=valid")))
                    .starts_with("HTTP/1.1 200")
            );
        }));
        Ok(())
    });
    flow.connection_timeout = Duration::from_millis(100);
    assert_eq!(flow.authorize(&client()).unwrap().code.expose(), "valid");
    worker.lock().unwrap().take().unwrap().join().unwrap();

    let copy = worker.clone();
    let flow = self::flow(move |url| {
        let (host, _, _) = parameters(url);
        *copy.lock().unwrap() = Some(thread::spawn(move || {
            for _ in 0..CONNECTION_LIMIT {
                assert!(
                    send(&host, "GET /favicon.ico HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 400")
                );
            }
        }));
        Ok(())
    });
    assert_eq!(
        flow.authorize(&client()).unwrap_err(),
        AuthError::InvalidCallback
    );
    worker.lock().unwrap().take().unwrap().join().unwrap();

    let mut flow = self::flow(|_| Ok(()));
    flow.timeout = Duration::from_millis(30);
    let start = Instant::now();
    assert_eq!(flow.authorize(&client()).unwrap_err(), AuthError::Timeout);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn overall_deadline_caps_an_incomplete_connection() {
    let connected = Arc::new(Mutex::new(None));
    let copy = connected.clone();
    let mut flow = flow(move |url| {
        let (host, _, _) = parameters(url);
        let mut stream = TcpStream::connect(host).unwrap();
        stream.write_all(b"GET /?state=").unwrap();
        *copy.lock().unwrap() = Some(stream);
        Ok(())
    });
    flow.timeout = Duration::from_millis(40);
    flow.connection_timeout = Duration::from_secs(2);
    let start = Instant::now();
    assert_eq!(flow.authorize(&client()).unwrap_err(), AuthError::Timeout);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn expired_connection_deadlines_do_not_set_zero_timeouts_or_write() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let mut client = TcpStream::connect(&host).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    stream.set_nonblocking(false).unwrap();
    let expired = Instant::now() - Duration::from_secs(1);
    assert!(receive(&mut stream, expired, &host, "expected").is_none());
    respond(&mut stream, true, expired);
    drop(stream);
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();
    assert!(response.is_empty());
}

#[test]
fn failed_browser_closes_listener_and_does_not_write_to_connected_peer() {
    let observed = Arc::new(Mutex::new(None));
    let copy = observed.clone();
    let flow = flow(move |url| {
        let (host, _, _) = parameters(url);
        let stream = TcpStream::connect(&host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        *copy.lock().unwrap() = Some((host, stream));
        Err(AuthError::BrowserUnavailable)
    });
    assert_eq!(
        flow.authorize(&client()).unwrap_err(),
        AuthError::BrowserUnavailable
    );
    let (host, mut stream) = observed.lock().unwrap().take().unwrap();
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    assert!(bytes.is_empty());
    assert!(TcpStream::connect(host).is_err());
}

#[test]
fn system_browser_rejects_nonfixed_urls_without_launching_anything() {
    for url in [
        "file:///tmp/no",
        "http://accounts.google.com/o/oauth2/v2/auth",
        "https://evil.invalid/o/oauth2/v2/auth",
        "https://accounts.google.com/other",
        "https://user@accounts.google.com/o/oauth2/v2/auth",
        "https://accounts.google.com/o/oauth2/v2/auth#fragment",
        "https://accounts.google.com/o/oauth2/v2/auth?code_verifier=CANARY",
        "https://accounts.google.com/o/oauth2/v2/auth?client_secret=CANARY",
    ] {
        assert_eq!(
            SystemBrowser.open(url).unwrap_err(),
            AuthError::BrowserUnavailable
        );
    }
}

#[test]
fn browser_command_has_fixed_program_single_url_and_no_explicit_environment() {
    let url = format!(
        "{AUTHORIZATION_ENDPOINT}?response_type=code&state=public-state&code_challenge=public-challenge"
    );
    let command = browser_command(&url);
    assert_eq!(command.get_program(), "/usr/bin/open");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [std::ffi::OsStr::new(&url)]
    );
    assert_eq!(command.get_envs().count(), 0);
    assert!(command.get_current_dir().is_none());
}

#[test]
#[cfg(unix)]
fn browser_environment_does_not_inherit_credentials() {
    // Only this subprocess receives synthetic secrets; the parallel test process is unchanged.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "loopback::tests::browser_environment_child_probe",
            "--nocapture",
        ])
        .env_clear()
        .env("HIVE_AUTH_BROWSER_ENV_PROBE", "1")
        .env("HIVE_GOOGLE_ACCESS_TOKEN", "SYNTHETIC_ACCESS_TOKEN_CANARY")
        .env("UNRELATED_SECRET", "SYNTHETIC_UNRELATED_SECRET_CANARY")
        .env("PATH", "/synthetic/untrusted/path")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child probe failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn browser_environment_child_probe() {
    if std::env::var_os("HIVE_AUTH_BROWSER_ENV_PROBE").as_deref() != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }
    assert_eq!(
        std::env::var("HIVE_GOOGLE_ACCESS_TOKEN").unwrap(),
        "SYNTHETIC_ACCESS_TOKEN_CANARY"
    );
    assert_eq!(
        std::env::var("UNRELATED_SECRET").unwrap(),
        "SYNTHETIC_UNRELATED_SECRET_CANARY"
    );
    // Exercise the exact launch-environment policy with a harmless program, never open().
    let output = isolated_launch_command("/usr/bin/env")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty(), "launch environment must be empty");
    assert!(output.stderr.is_empty());
}
