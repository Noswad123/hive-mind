//! Read-only Google Tasks snapshots. OAuth, environment selection, refresh-token
//! storage, retries, and persistence belong to the caller, not this client.

use std::{collections::HashSet, io::Read, time::Duration};

use anyhow::{Result, anyhow, ensure};
use reqwest::{Url, blocking::Client, header, redirect::Policy};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};

use crate::GoogleTask;

const BASE_URL: &str = "https://tasks.googleapis.com/tasks/v1/";
const MAX_PAGE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PAGES: usize = 100;
const MAX_ITEMS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GoogleTaskList {
    pub id: String,
    pub title: String,
}

// Deliberately no Debug implementation: the HTTP client contains authorization.
pub struct GoogleTasksClient {
    client: Client,
    base_url: Url,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", bound(deserialize = "T: Deserialize<'de>"))]
struct Page<T> {
    #[serde(default)]
    items: Vec<T>,
    #[serde(default, deserialize_with = "page_token")]
    next_page_token: Option<String>,
}

fn page_token<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn valid_identifier(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

impl GoogleTasksClient {
    /// Uses only the fixed Google HTTPS endpoint. The caller supplies an access
    /// token explicitly; this client never discovers credentials or refreshes it.
    pub fn new(access_token: &str) -> Result<Self> {
        Self::build(access_token, BASE_URL, false)
    }

    #[cfg(test)]
    fn with_base_url(access_token: &str, endpoint: &str) -> Result<Self> {
        Self::build(access_token, endpoint, true)
    }

    fn build(access_token: &str, endpoint: &str, allow_loopback: bool) -> Result<Self> {
        ensure!(
            valid_identifier(access_token),
            "invalid Google access token"
        );
        let base_url = Url::parse(endpoint).map_err(|_| anyhow!("invalid Google endpoint"))?;
        let authority = endpoint
            .split("://")
            .nth(1)
            .unwrap_or("")
            .split('/')
            .next()
            .unwrap_or("");
        let loopback = matches!(base_url.host_str(), Some("127.0.0.1" | "[::1]"));
        ensure!(
            endpoint == base_url.as_str()
                && !authority.contains('@')
                && base_url.username().is_empty()
                && base_url.password().is_none()
                && base_url.query().is_none()
                && base_url.fragment().is_none()
                && base_url.path() == "/tasks/v1/"
                && (endpoint == BASE_URL
                    || (allow_loopback && loopback && base_url.scheme() == "http")),
            "invalid Google endpoint"
        );
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {access_token}"))
            .map_err(|_| anyhow!("invalid Google access token"))?;
        authorization.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        let client = Client::builder()
            .default_headers(headers)
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .map_err(|_| anyhow!("could not initialize Google HTTP client"))?;
        Ok(Self { client, base_url })
    }

    pub fn list_task_lists(&self) -> Result<Vec<GoogleTaskList>> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow!("invalid Google endpoint"))?
            .pop_if_empty()
            .extend(["users", "@me", "lists"]);
        self.snapshot(url, |list: &GoogleTaskList| valid_identifier(&list.id))
    }

    /// Fetches every page, including lifecycle records. No partial result is
    /// returned on failure, and no cursor or other state is retained.
    pub fn list_tasks(&self, list_id: &str) -> Result<Vec<GoogleTask>> {
        ensure!(
            valid_identifier(list_id) && !matches!(list_id, "." | ".."),
            "invalid Google task list ID"
        );
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow!("invalid Google endpoint"))?
            .pop_if_empty()
            .extend(["lists", list_id, "tasks"]);
        url.query_pairs_mut().extend_pairs([
            ("showCompleted", "true"),
            ("showDeleted", "true"),
            ("showHidden", "true"),
        ]);
        self.snapshot(url, |task: &GoogleTask| valid_identifier(&task.id))
    }

    fn snapshot<T: DeserializeOwned>(
        &self,
        mut url: Url,
        valid: impl Fn(&T) -> bool,
    ) -> Result<Vec<T>> {
        url.query_pairs_mut().append_pair("maxResults", "100");
        let mut items = Vec::new();
        let mut seen_tokens = HashSet::new();
        let mut next_token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut page_url = url.clone();
            if let Some(token) = &next_token {
                page_url.query_pairs_mut().append_pair("pageToken", token);
            }
            // Never propagate reqwest/serde errors: they may contain URLs or
            // provider-controlled text. Retry hints are static, not headers.
            let response = self
                .client
                .get(page_url)
                .send()
                .map_err(|_| anyhow!("Google Tasks request failed; retry later"))?;
            let status = response.status();
            if !status.is_success() {
                let hint = match status.as_u16() {
                    401 => "provide a valid access token; no automatic refresh",
                    403 => "check account permissions; do not retry unchanged",
                    429 | 500..=599 => "retry later; no automatic retry",
                    300..=399 => "redirect refused; no automatic retry",
                    _ => "check request; no automatic retry",
                };
                return Err(anyhow!("Google Tasks HTTP {}: {hint}", status.as_u16()));
            }
            ensure!(
                response
                    .content_length()
                    .is_none_or(|size| size <= MAX_PAGE_BYTES),
                "Google Tasks page exceeds size limit"
            );
            let mut bytes = Vec::new();
            response
                .take(MAX_PAGE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| anyhow!("could not read Google Tasks page; retry later"))?;
            ensure!(
                bytes.len() as u64 <= MAX_PAGE_BYTES,
                "Google Tasks page exceeds size limit"
            );
            ensure!(
                bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{'),
                "invalid Google Tasks page"
            );
            let page: Page<T> =
                serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid Google Tasks page"))?;
            ensure!(
                page.items.iter().all(&valid),
                "invalid Google Tasks item ID"
            );
            ensure!(
                items.len() + page.items.len() <= MAX_ITEMS,
                "Google Tasks snapshot exceeds item limit"
            );
            items.extend(page.items);
            match page.next_page_token {
                None => return Ok(items),
                Some(token) => {
                    ensure!(valid_identifier(&token), "invalid Google Tasks page token");
                    ensure!(
                        seen_tokens.insert(token.clone()),
                        "repeated Google Tasks page token"
                    );
                    next_token = Some(token);
                }
            }
        }
        Err(anyhow!("Google Tasks snapshot exceeds page limit"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
        time::Instant,
    };

    const SECRET: &str = "test-bearer-DO-NOT-LEAK";

    struct Server {
        base: String,
        requests: Arc<Mutex<Vec<String>>>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl Server {
        fn new(responses: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}/tasks/v1/", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = requests.clone();
            let worker = thread::spawn(move || {
                for response in responses {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(Instant::now() < deadline, "mock accept timed out");
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => panic!("mock accept failed: {error}"),
                        }
                    };
                    // macOS inherits the listener's nonblocking mode. Restore
                    // blocking I/O so large writes cannot truncate on WouldBlock.
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        assert_eq!(stream.read(&mut byte).unwrap(), 1);
                        request.push(byte[0]);
                        assert!(request.len() < 16_384);
                    }
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(request).unwrap());
                    // A size-limit rejection can close the connection early.
                    if let Err(error) = stream.write_all(response.as_bytes()) {
                        assert!(
                            matches!(
                                error.kind(),
                                std::io::ErrorKind::BrokenPipe
                                    | std::io::ErrorKind::ConnectionReset
                            ),
                            "mock response write failed: {:?}",
                            error.kind()
                        );
                    }
                }
            });
            Self {
                base,
                requests,
                worker: Some(worker),
            }
        }

        fn client(&self) -> GoogleTasksClient {
            GoogleTasksClient::with_base_url(SECRET, &self.base).unwrap()
        }

        fn finish(mut self) -> Vec<String> {
            self.worker.take().unwrap().join().unwrap();
            Arc::try_unwrap(self.requests)
                .unwrap()
                .into_inner()
                .unwrap()
        }
    }

    fn response(status: u16, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} Mock\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn ok(body: &str) -> String {
        response(200, body)
    }

    fn chunked(body: &str, chunk_size: usize) -> String {
        use std::fmt::Write as _;

        let mut wire = String::from(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        );
        for chunk in body.as_bytes().chunks(chunk_size) {
            write!(
                wire,
                "{:x}\r\n{}\r\n",
                chunk.len(),
                std::str::from_utf8(chunk).unwrap()
            )
            .unwrap();
        }
        wire.push_str("0\r\n\r\n");
        wire
    }

    fn safe_error<T>(result: Result<T>) -> String {
        let error = match result {
            Ok(_) => panic!("expected failure"),
            Err(error) => error,
        };
        let message = format!("{error:#} {error:?}");
        for secret in [SECRET, "private-page-token", "private-body", "http://"] {
            assert!(!message.contains(secret));
        }
        message
    }

    fn request_url(request: &str) -> Url {
        let mut line = request.lines().next().unwrap().split_whitespace();
        assert_eq!(line.next(), Some("GET"));
        let url = Url::parse(&format!("http://mock{}", line.next().unwrap())).unwrap();
        assert!(request.to_ascii_lowercase().contains(&format!(
            "authorization: bearer {}",
            SECRET.to_ascii_lowercase()
        )));
        url
    }

    #[test]
    fn task_snapshot_encodes_paths_tokens_and_preserves_lifecycle_and_unknown_fields() {
        let server = Server::new(vec![
            ok(r#"{"items":[{"id":"a","future":{"x":1}}],"nextPageToken":"a&b?/# +%"}"#),
            ok(r#"{"items":[{"id":"b","deleted":true,"hidden":true,"status":"completed"}]}"#),
        ]);
        let tasks = server.client().list_tasks("list/with?x=#% +").unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].extra["future"]["x"], 1);
        assert!(tasks[1].deleted && tasks[1].hidden);
        assert_eq!(tasks[1].status, "completed");
        let requests = server.finish();
        for (index, request) in requests.iter().enumerate() {
            let url = request_url(request);
            assert_eq!(
                url.path(),
                "/tasks/v1/lists/list%2Fwith%3Fx=%23%25%20+/tasks"
            );
            let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            for key in ["showCompleted", "showDeleted", "showHidden"] {
                assert_eq!(query[key], "true");
            }
            assert_eq!(query["maxResults"], "100");
            assert_eq!(
                query.get("pageToken").map(String::as_str),
                if index == 0 { None } else { Some("a&b?/# +%") }
            );
        }
    }

    #[test]
    fn lists_paginate_and_absent_items_are_empty() {
        let server = Server::new(vec![
            ok(r#"{"items":[{"id":"one","title":"Inbox"}],"nextPageToken":"next"}"#),
            ok("{}"),
        ]);
        let lists = server.client().list_task_lists().unwrap();
        assert_eq!(
            lists,
            vec![GoogleTaskList {
                id: "one".into(),
                title: "Inbox".into()
            }]
        );
        for request in server.finish() {
            let url = request_url(&request);
            assert_eq!(url.path(), "/tasks/v1/users/@me/lists");
            assert!(
                url.query_pairs()
                    .any(|(k, v)| k == "maxResults" && v == "100")
            );
        }
    }

    #[test]
    fn rejects_invalid_tokens_ids_and_endpoints_without_network() {
        for token in ["", "  ", "a\r\nb", "a\0b", "a\tb"] {
            safe_error(GoogleTasksClient::new(token));
        }
        let client = GoogleTasksClient::new(SECRET).unwrap();
        assert_eq!(client.base_url.as_str(), BASE_URL);
        for id in ["", "  ", ".", "..", "a\nb"] {
            safe_error(client.list_tasks(id));
        }
        for endpoint in [
            "http://localhost/tasks/v1/",
            "http://127.0.0.2/tasks/v1/",
            "http://127.1/tasks/v1/",
            "http://2130706433/tasks/v1/",
            "http://example.com/tasks/v1/",
            "https://example.com/tasks/v1/",
            "http://user:pass@127.0.0.1/tasks/v1/",
            "http://@127.0.0.1/tasks/v1/",
            "http://127.0.0.1/tasks/v1/?secret=x",
            "http://127.0.0.1/tasks/v1/#fragment",
            "http://127.0.0.1/",
            "http://127.0.0.1/tasks/v1",
        ] {
            safe_error(GoogleTasksClient::with_base_url(SECRET, endpoint));
        }
        assert!(GoogleTasksClient::with_base_url(SECRET, "http://[::1]:1234/tasks/v1/").is_ok());
    }

    #[test]
    fn rejects_malformed_shapes_and_missing_or_blank_task_ids() {
        for body in [
            "private-body",
            "[]",
            r#"[[],"next"]"#,
            "null",
            r#"{"items":null}"#,
            r#"{"items":{}}"#,
            r#"{"items":[{}]}"#,
            r#"{"items":[{"id":null}]}"#,
            r#"{"items":[{"id":"  "}]}"#,
            r#"{"items":[{"id":42}]}"#,
            r#"{"items":[{"id":"a","deleted":"true"}]}"#,
            r#"{"nextPageToken":42}"#,
            r#"{"nextPageToken":null}"#,
            r#"{"nextPageToken":" "}"#,
            r#"{"nextPageToken":"a\nb"}"#,
        ] {
            let server = Server::new(vec![ok(body)]);
            safe_error(server.client().list_tasks("list"));
            server.finish();
        }
        for body in [
            r#"{"items":[{"id":"a"}]}"#,
            r#"{"items":[{"id":"","title":"x"}]}"#,
            r#"{"items":[{"id":"a","title":4}]}"#,
        ] {
            let server = Server::new(vec![ok(body)]);
            safe_error(server.client().list_task_lists());
            server.finish();
        }
    }

    #[test]
    fn errors_are_redacted_and_later_page_failure_returns_no_snapshot() {
        for status in [401, 403, 429, 500, 503] {
            let server = Server::new(vec![
                ok(r#"{"items":[{"id":"first"}],"nextPageToken":"private-page-token"}"#),
                response(status, &format!("private-body {SECRET}")),
            ]);
            let error = safe_error(server.client().list_tasks("list"));
            assert!(error.contains(&format!("HTTP {status}")));
            assert!(error.contains("retry") || error.contains("refresh"));
            assert_eq!(server.finish().len(), 2);
        }
        let server = Server::new(vec![
            ok(r#"{"nextPageToken":"private-page-token"}"#),
            ok("private-body"),
        ]);
        safe_error(server.client().list_tasks("list"));
        assert_eq!(server.finish().len(), 2);
    }

    #[test]
    fn repeated_tokens_and_page_limit_terminate() {
        let page = ok(r#"{"nextPageToken":"private-page-token"}"#);
        let server = Server::new(vec![page.clone(), page]);
        assert!(safe_error(server.client().list_tasks("list")).contains("repeated"));
        assert_eq!(server.finish().len(), 2);
        let server = Server::new(
            (0..MAX_PAGES)
                .map(|i| ok(&format!(r#"{{"nextPageToken":"page-{i}"}}"#)))
                .collect(),
        );
        assert!(safe_error(server.client().list_task_lists()).contains("page limit"));
        assert_eq!(server.finish().len(), MAX_PAGES);
    }

    #[test]
    fn valid_chunked_pages_are_decoded() {
        let body = r#"{"items":[{"id":"chunked"}]}"#;
        for chunk_size in [1, 8192] {
            let server = Server::new(vec![chunked(body, chunk_size)]);
            let tasks = server.client().list_tasks("list").unwrap();
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0].id, "chunked");
            server.finish();
        }
        let mut body = body.to_owned();
        body.extend(std::iter::repeat_n(
            ' ',
            MAX_PAGE_BYTES as usize - body.len(),
        ));
        for chunk_size in [8192, body.len()] {
            let server = Server::new(vec![chunked(&body, chunk_size)]);
            let tasks = server.client().list_tasks("list").unwrap();
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0].id, "chunked");
            server.finish();
        }
    }

    #[test]
    fn item_and_body_limits_are_enforced() {
        let body = serde_json::json!({"items": (0..MAX_ITEMS + 1).map(|i| serde_json::json!({"id": i.to_string()})).collect::<Vec<_>>()}).to_string();
        let server = Server::new(vec![ok(&body)]);
        assert!(safe_error(server.client().list_tasks("list")).contains("item limit"));
        server.finish();
        let body = "x".repeat(MAX_PAGE_BYTES as usize + 1);
        for chunk_size in [None, Some(8192), Some(body.len())] {
            let wire = match chunk_size {
                Some(size) => chunked(&body, size),
                None => ok(&body),
            };
            let server = Server::new(vec![wire]);
            let message = safe_error(server.client().list_tasks("list"));
            assert!(
                message.contains("size limit"),
                "chunk_size={chunk_size:?}: {message}"
            );
            server.finish();
        }
    }

    #[test]
    fn redirects_never_forward_authorization_even_to_loopback() {
        let destination = TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let location = format!(
            "http://{}/private-page-token",
            destination.local_addr().unwrap()
        );
        for status in [301, 302, 307, 308] {
            let server = Server::new(vec![format!(
                "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )]);
            assert!(safe_error(server.client().list_tasks("list")).contains("redirect refused"));
            assert_eq!(server.finish().len(), 1);
            assert!(
                matches!(destination.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        }
    }

    #[test]
    #[ignore = "requires explicit HIVE_TEST_BIN and MW_TEST_BIN pointing to capture-capable binaries"]
    fn fake_google_to_real_hive_and_mw_durable_capture() {
        use crate::{BootstrapPolicy, DeliveryState, store::CaptureStore};
        use std::{
            collections::BTreeSet,
            fs,
            path::PathBuf,
            process::{Command, Stdio},
        };

        let binary = |key: &str| {
            let path = PathBuf::from(std::env::var_os(key).unwrap_or_else(|| {
                panic!("set {key} explicitly before running this ignored integration test")
            }));
            assert!(path.is_absolute(), "{key} must be an absolute binary path");
            let path = path
                .canonicalize()
                .unwrap_or_else(|_| panic!("{key} binary is unavailable"));
            assert!(path.is_file(), "{key} must point to a binary file");
            path
        };
        let hive = binary("HIVE_TEST_BIN");
        let mw = binary("MW_TEST_BIN");
        let temp = tempfile::tempdir().unwrap();
        // CaptureStore rejects symlinked ancestors, including macOS /var.
        let root = temp.path().canonicalize().unwrap();
        for directory in [
            "home", "notes", "config", "data", "state", "cache", "tmp", "runtime",
        ] {
            fs::create_dir(root.join(directory)).unwrap();
        }
        let notes = root.join("notes");
        let inbox = notes.join("capture.md");
        let dashboard = notes.join("dashboard.md");
        let config = root.join("config/config.toml");
        fs::write(&config, "notes_dir = notes\ninbox_path = notes/capture.md\ndashboard_path = notes/dashboard.md\ndb_path = unavailable/notes.sqlite\nnotes_schema_path = unavailable/schema.sql\n").unwrap();
        let queue = root.join("capture-queue.sqlite");
        let command = |program: &std::path::Path| {
            let mut command = Command::new(program);
            command
                .env_clear()
                .current_dir(&root)
                .env("HOME", root.join("home"))
                .env("PATH", "/usr/bin:/bin")
                .env("MW_CONFIG", &config)
                .env("NOTES_DIR", &notes)
                .env("MW_INBOX_PATH", &inbox)
                .env("INBOX_PATH", &inbox)
                .env("DASHBOARD_PATH", &dashboard)
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_STATE_HOME", root.join("state"))
                .env("XDG_CACHE_HOME", root.join("cache"))
                .env("XDG_RUNTIME_DIR", root.join("runtime"))
                .env("TMPDIR", root.join("tmp"))
                .env("TMP", root.join("tmp"))
                .env("TEMP", root.join("tmp"))
                .env("LANG", "C")
                .env("LC_ALL", "C")
                .stdin(Stdio::null());
            command
        };
        let deliver = || {
            let output = command(&hive)
                .args(["captures", "deliver", "--db-path"])
                .arg(&queue)
                .arg("--mw-bin")
                .arg(&mw)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "sandbox hive delivery failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
        };
        let snapshot = || {
            let output = command(&mw)
                .args(["todos", "snapshot", "--json"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "sandbox mw snapshot failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
        };
        let pages = [
            ok(
                r#"{"items":[{"id":"phone-a","title":"Same phone capture","status":"needsAction"}],"nextPageToken":"page-two"}"#,
            ),
            ok(
                r#"{"items":[{"id":"phone-b","title":"Same phone capture","status":"needsAction"}]}"#,
            ),
        ];
        let server = Server::new(pages.iter().cycle().take(4).cloned().collect());
        let client = server.client();
        let mut store = CaptureStore::open(&queue).unwrap();
        for expected_staged in [2, 0] {
            let tasks: Vec<GoogleTask> = client.list_tasks("phone-inbox").unwrap();
            assert_eq!(tasks.len(), 2);
            let report = store
                .ingest(
                    "fake-account",
                    "phone-inbox",
                    &tasks,
                    BootstrapPolicy::ImportOpen,
                )
                .unwrap();
            assert_eq!(report.staged, expected_staged);
            let jobs = store.jobs().unwrap();
            assert_eq!(jobs.len(), 2);
            assert!(jobs.iter().all(|job| job.state == DeliveryState::Pending));
        }
        let mut requests = server.finish();
        // Simulate the desktop going offline after durable staging, before any mw write.
        drop(store);
        assert!(!inbox.exists());
        let store = CaptureStore::open(&queue).unwrap();
        let jobs = store.jobs().unwrap();
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().all(|job| job.state == DeliveryState::Pending));
        let expected_ids: BTreeSet<_> = jobs.iter().map(|job| job.capture_id.clone()).collect();
        assert_eq!(expected_ids.len(), 2);
        drop(store);

        assert_eq!(deliver()["report"]["delivered"], 2);
        let source_bytes = fs::read(&inbox).unwrap();
        let dashboard_bytes = fs::read(&dashboard).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&source_bytes)
                .lines()
                .filter(|line| line.trim_start().starts_with("- [ ]"))
                .count(),
            2
        );
        let saved = snapshot();
        assert_eq!(saved["source"], "saved-markdown");
        let items = saved["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        let actual_ids: BTreeSet<_> = items
            .iter()
            .map(|item| {
                assert_eq!(item["text"], "Same phone capture");
                assert_eq!(item["sourcePath"], "capture.md");
                assert_eq!(item["identityKind"], "stable");
                let id = item["stableId"].as_str().unwrap().to_owned();
                assert_eq!(item["id"], format!("task:{id}"));
                id
            })
            .collect();
        assert_eq!(actual_ids, expected_ids);

        let server = Server::new(pages.to_vec());
        let tasks = server.client().list_tasks("phone-inbox").unwrap();
        requests.extend(server.finish());
        let mut store = CaptureStore::open(&queue).unwrap();
        assert_eq!(
            store
                .ingest(
                    "fake-account",
                    "phone-inbox",
                    &tasks,
                    BootstrapPolicy::ImportOpen
                )
                .unwrap()
                .staged,
            0
        );
        drop(store);
        assert_eq!(deliver()["report"]["delivered"], 0);
        let jobs = CaptureStore::inspect(&queue).unwrap();
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().all(|job| job.state == DeliveryState::Delivered));
        assert_eq!(snapshot(), saved);
        assert_eq!(fs::read(&inbox).unwrap(), source_bytes);
        assert_eq!(fs::read(&dashboard).unwrap(), dashboard_bytes);
        assert!(!root.join("unavailable").exists());
        assert_eq!(requests.len(), 6);
        for request in requests {
            assert_eq!(
                request_url(&request).path(),
                "/tasks/v1/lists/phone-inbox/tasks"
            );
        }
    }

    #[test]
    fn transport_errors_do_not_expose_request_url() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/tasks/v1/", listener.local_addr().unwrap());
        drop(listener);
        safe_error(
            GoogleTasksClient::with_base_url(SECRET, &base)
                .unwrap()
                .list_tasks("private-page-token"),
        );
    }
}
