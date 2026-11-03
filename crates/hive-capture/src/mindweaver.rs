//! Durable capture delivery through the trusted mw executable, never through a shell.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use crate::{CaptureJob, DeliveryReport, DeliveryState, DestinationBinding, store::CaptureStore};

const OUTPUT_LIMIT: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
const ENV_KEYS: &[&str] = &[
    "HOME",
    "PATH",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "MW_CONFIG",
    "NOTES_DIR",
    "MW_INBOX_PATH",
    "INBOX_PATH",
    "DASHBOARD_PATH",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
];

struct Invocation {
    binary: PathBuf,
    cwd: PathBuf,
    env: BTreeMap<OsString, OsString>,
    args: Vec<String>,
}

trait Runner: Send + Sync {
    fn run(&self, invocation: &Invocation) -> Result<Vec<u8>>;
}

struct ProcessRunner {
    timeout: Duration,
    output_limit: usize,
}

impl Runner for ProcessRunner {
    fn run(&self, invocation: &Invocation) -> Result<Vec<u8>> {
        let mut child = Command::new(&invocation.binary)
            .args(&invocation.args)
            .current_dir(&invocation.cwd)
            .env_clear()
            .envs(&invocation.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| anyhow::anyhow!("mw process could not start"))?;
        let stdout = child.stdout.take().context("mw stdout unavailable")?;
        let stderr = child.stderr.take().context("mw stderr unavailable")?;
        let (tx, rx) = mpsc::sync_channel(2);
        // Drain both pipes concurrently, with bounded buffering even for stderr.
        for (is_stdout, mut pipe) in [
            (true, Box::new(stdout) as Box<dyn Read + Send>),
            (false, Box::new(stderr) as Box<dyn Read + Send>),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                loop {
                    let mut bytes = vec![0; 8192];
                    match pipe.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(n) => {
                            bytes.truncate(n);
                            if tx.send(Ok((is_stdout, bytes))).is_err() {
                                return;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            let _ = tx.send(Err(()));
                            break;
                        }
                    }
                }
            });
        }
        drop(tx);
        let result = (|| {
            let deadline = Instant::now() + self.timeout;
            let mut output = Vec::new();
            let mut total = 0usize;
            let mut drained = false;
            loop {
                ensure!(
                    Instant::now() < deadline,
                    "mw process timed out; outcome unknown"
                );
                if !drained {
                    match rx.recv_timeout(Duration::from_millis(5)) {
                        Ok(Ok((stdout, bytes))) => {
                            total += bytes.len();
                            ensure!(
                                total <= self.output_limit,
                                "mw output limit exceeded; outcome unknown"
                            );
                            if stdout {
                                output.extend(bytes);
                            }
                        }
                        Ok(Err(())) => bail!("mw output could not be read; outcome unknown"),
                        Err(mpsc::RecvTimeoutError::Disconnected) => drained = true,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if let Some(status) = child
                    .try_wait()
                    .map_err(|_| anyhow::anyhow!("mw wait failed"))?
                    && drained
                {
                    ensure!(status.success(), "mw failed; outcome unknown");
                    return Ok(output);
                }
            }
        })();
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }
}

pub struct MindWeaver {
    binary: PathBuf,
    cwd: PathBuf,
    env: BTreeMap<OsString, OsString>,
    binding: Mutex<Option<DestinationBinding>>,
    runner: Box<dyn Runner>,
}

impl MindWeaver {
    /// Freeze executable, working directory and a small noncredential environment.
    /// Does not invoke mw or create any filesystem state.
    pub fn new(binary: impl AsRef<Path>) -> Result<Self> {
        let binary = binary.as_ref();
        ensure!(binary.is_absolute(), "mw executable path must be absolute");
        let metadata = std::fs::metadata(binary).context("mw executable unavailable")?;
        ensure!(metadata.is_file(), "mw executable must be a file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o111 != 0,
                "mw file is not executable"
            );
        }
        Ok(Self {
            binary: binary.canonicalize().context("resolve mw executable")?,
            cwd: std::env::current_dir().context("freeze mw working directory")?,
            env: ENV_KEYS
                .iter()
                .filter_map(|key| std::env::var_os(key).map(|v| (OsString::from(key), v)))
                .collect(),
            binding: Mutex::new(None),
            runner: Box::new(ProcessRunner {
                timeout: TIMEOUT,
                output_limit: OUTPUT_LIMIT,
            }),
        })
    }

    fn invoke(&self, args: &[&str], binding: Option<&DestinationBinding>) -> Result<Vec<u8>> {
        let mut env = self.env.clone();
        if let Some(binding) = binding {
            for (key, value) in [
                ("NOTES_DIR", &binding.notes_dir),
                ("MW_INBOX_PATH", &binding.inbox_path),
                ("DASHBOARD_PATH", &binding.dashboard_path),
            ] {
                env.insert(key.into(), value.into());
            }
        }
        self.runner.run(&Invocation {
            binary: self.binary.clone(),
            cwd: self.cwd.clone(),
            env,
            args: args.iter().map(|s| (*s).into()).collect(),
        })
    }

    fn read_binding(&self) -> Result<DestinationBinding> {
        let bytes = self.invoke(&["config", "json"], None)?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid mw config response"))?;
        ensure!(
            value["schemaVersion"] == "1",
            "unsupported mw config schema"
        );
        // Deliberately retain only the three public destination fields.
        let binding = DestinationBinding {
            notes_dir: string(&value, "notesDir")?.into(),
            inbox_path: string(&value, "inboxPath")?.into(),
            dashboard_path: string(&value, "dashboardPath")?.into(),
        };
        binding.validate()?;
        let root = normalized(Path::new(&binding.notes_dir))?;
        let inbox = normalized(Path::new(&binding.inbox_path))?;
        ensure!(
            inbox.starts_with(&root) && inbox != root,
            "mw Inbox must be beneath notes root"
        );
        ensure!(
            inbox != normalized(Path::new(&binding.dashboard_path))?,
            "mw Inbox and dashboard must differ"
        );
        Ok(binding)
    }

    /// One successful config discovery per adapter. Writes always pin these paths.
    pub fn binding(&self) -> Result<DestinationBinding> {
        let mut cached = self
            .binding
            .lock()
            .map_err(|_| anyhow::anyhow!("mw binding unavailable"))?;
        if let Some(binding) = cached.as_ref() {
            return Ok(binding.clone());
        }
        let binding = self.read_binding()?;
        *cached = Some(binding.clone());
        Ok(binding)
    }

    pub fn deliver_pending(
        &self,
        store: &mut CaptureStore,
        limit: usize,
    ) -> Result<DeliveryReport> {
        self.deliver_pending_guarded(store, limit, || true)
            .map(|(report, _)| report)
    }

    /// Check the remaining cycle budget after read-only preflight and before each claim.
    /// A true flag signals `Deferred cycle_budget`: the next job was not claimed or
    /// attempted. The report includes any earlier deliveries in this call.
    /// This does not impose a timeout on preflight or an already claimed writer.
    pub fn deliver_pending_before(
        &self,
        store: &mut CaptureStore,
        limit: usize,
        deadline: Instant,
        minimum_remaining: Duration,
    ) -> Result<(DeliveryReport, bool)> {
        self.deliver_pending_guarded(store, limit, || {
            deadline.saturating_duration_since(Instant::now()) >= minimum_remaining
        })
    }

    fn deliver_pending_guarded(
        &self,
        store: &mut CaptureStore,
        limit: usize,
        mut can_claim: impl FnMut() -> bool,
    ) -> Result<(DeliveryReport, bool)> {
        let jobs = store.jobs()?;
        if limit == 0 || !jobs.iter().any(|j| j.state == DeliveryState::Pending) {
            return Ok((report(store, 0, 0)?, false));
        }
        let binding = self.binding()?;
        let help = self.invoke(&["todos", "add", "--help"], Some(&binding))?;
        let help = std::str::from_utf8(&help).context("invalid mw capture help")?;
        for flag in ["--capture-id", "--json"] {
            ensure!(
                help.split(|c: char| c.is_whitespace() || matches!(c, ',' | '=' | '[' | ']'))
                    .any(|word| word == flag),
                "mw capture capability unavailable"
            );
        }
        let mut delivered = 0;
        for candidate in jobs
            .iter()
            .filter(|j| j.state == DeliveryState::Pending)
            .take(limit)
        {
            if !can_claim() {
                return Ok((report(store, delivered, 0)?, true));
            }
            // A claim commits both the attempt and destination before any writer launch.
            let job = store.claim(&candidate.capture_id, &binding)?;
            let outcome: Result<Value> = (|| {
                let bytes = self.invoke(
                    &[
                        "todos",
                        "add",
                        "--capture-id",
                        &job.capture_id,
                        "--json",
                        "--",
                        &job.title,
                    ],
                    Some(&binding),
                )?;
                let raw: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow::anyhow!("invalid mw capture receipt"))?;
                ensure!(raw["schemaVersion"] == "1", "unsupported mw receipt schema");
                let already = raw["alreadyCaptured"]
                    .as_bool()
                    .context("missing capture status")?;
                ensure!(
                    raw["projectionUpdated"].is_boolean(),
                    "missing projection status"
                );
                validate_task(&raw["task"], &job, &binding)?;
                if !already {
                    ensure!(
                        raw["task"]["done"] == false && raw["task"]["todoSection"] == "Inbox",
                        "new capture is not open in Inbox"
                    );
                }
                Ok(
                    json!({"schemaVersion": "1", "method": "todos-add", "binding": binding, "result": raw}),
                )
            })();
            match outcome {
                Ok(receipt) => {
                    if store.complete(&job.capture_id, &receipt).is_ok() {
                        delivered += 1;
                    } else {
                        // A failed receipt commit is just as ambiguous as a failed writer.
                        block_unresolved(
                            store,
                            &job.capture_id,
                            &format!(
                                "capture {}: receipt persistence failed; outcome unknown",
                                job.capture_id
                            ),
                        )?;
                    }
                }
                Err(_) => block_unresolved(
                    store,
                    &job.capture_id,
                    &format!(
                        "capture {}: mw delivery failed; outcome unknown; explicit reconciliation required",
                        job.capture_id
                    ),
                )?,
            }
        }
        Ok((report(store, delivered, 0)?, false))
    }

    /// Adopt only an exact saved task after an unknown attempt. Never retries add.
    pub fn reconcile(&self, store: &mut CaptureStore, capture_id: &str) -> Result<DeliveryReport> {
        let job = store
            .jobs()?
            .into_iter()
            .find(|j| j.capture_id == capture_id)
            .context("unknown capture ID")?;
        ensure!(
            matches!(
                job.state,
                DeliveryState::Delivering | DeliveryState::NeedsReview
            ) && job.attempts > 0
                && job.receipt.is_none(),
            "capture is not eligible for reconciliation"
        );
        let binding = job
            .binding
            .as_ref()
            .context("capture has no frozen destination")?;
        let outcome: Result<Value> = (|| {
            // Unlike delivery, explicit reconciliation also detects config-file drift.
            ensure!(self.read_binding()? == *binding, "mw destination changed");
            let bytes = self.invoke(&["todos", "snapshot", "--json"], Some(binding))?;
            let snapshot: Value = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid mw snapshot"))?;
            ensure!(
                snapshot["schemaVersion"] == "1" && snapshot["source"] == "saved-markdown",
                "invalid snapshot provenance"
            );
            ensure!(
                snapshot["notesDir"] == binding.notes_dir
                    && snapshot["dashboardPath"] == binding.dashboard_path,
                "snapshot destination mismatch"
            );
            let items = snapshot["items"]
                .as_array()
                .context("missing snapshot items")?;
            let matches: Vec<_> = items
                .iter()
                .filter(|t| {
                    t["stableId"] == job.capture_id || t["id"] == format!("task:{}", job.capture_id)
                })
                .collect();
            ensure!(matches.len() == 1, "capture missing or ambiguous");
            let task = matches[0];
            validate_task(task, &job, binding)?;
            ensure!(task["text"] == job.title.trim(), "capture text changed");
            Ok(
                json!({"schemaVersion": "1", "method": "snapshot-reconciliation", "binding": binding, "task": task}),
            )
        })();
        if let Ok(receipt) = outcome
            && store.complete(capture_id, &receipt).is_ok()
        {
            return report(store, 0, 1);
        }
        if job.state == DeliveryState::Delivering {
            block_unresolved(
                store,
                capture_id,
                &format!("capture {capture_id}: reconciliation unresolved; writer not retried"),
            )?;
        }
        report(store, 0, 0)
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().context("missing mw response field")
}

fn normalized(path: &Path) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => ensure!(result.pop(), "path escapes root"),
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

fn validate_task(task: &Value, job: &CaptureJob, binding: &DestinationBinding) -> Result<()> {
    ensure!(
        task["id"] == format!("task:{}", job.capture_id)
            && task["stableId"] == job.capture_id
            && task["identityKind"] == "stable",
        "capture identity mismatch"
    );
    let revision = string(task, "sourceRevision")?
        .strip_prefix("sha256:")
        .context("invalid source revision")?;
    ensure!(
        revision.len() == 64
            && revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid source revision"
    );
    ensure!(
        !string(task, "sourceId")?.trim().is_empty() && task["done"].is_boolean(),
        "invalid task provenance"
    );
    let relative = Path::new(string(task, "sourcePath")?);
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "invalid source path"
    );
    // Metadata-only identity checks: never read source Markdown or config files.
    let root = Path::new(&binding.notes_dir)
        .canonicalize()
        .context("notes root unavailable")?;
    let source = root
        .join(relative)
        .canonicalize()
        .context("capture source unavailable")?;
    let inbox = Path::new(&binding.inbox_path)
        .canonicalize()
        .context("capture Inbox unavailable")?;
    ensure!(
        source.starts_with(&root) && source == inbox,
        "capture is not in frozen Inbox"
    );
    Ok(())
}

fn block_unresolved(store: &mut CaptureStore, capture_id: &str, reason: &str) -> Result<()> {
    let job = store
        .jobs()?
        .into_iter()
        .find(|j| j.capture_id == capture_id)
        .context("unknown capture ID")?;
    // Preserve an upstream edit's durable review reason and provider_changed flag.
    if job.state == DeliveryState::NeedsReview {
        return Ok(());
    }
    store.block(capture_id, reason)
}

// State counters describe the whole durable queue; unsupported is a subset of blocked.
fn report(store: &CaptureStore, delivered: usize, reconciled: usize) -> Result<DeliveryReport> {
    let jobs = store.jobs()?;
    Ok(DeliveryReport {
        delivered,
        reconciled,
        blocked: jobs
            .iter()
            .filter(|j| {
                matches!(
                    j.state,
                    DeliveryState::Delivering
                        | DeliveryState::NeedsReview
                        | DeliveryState::Unsupported
                )
            })
            .count(),
        pending: jobs
            .iter()
            .filter(|j| j.state == DeliveryState::Pending)
            .count(),
        unsupported: jobs
            .iter()
            .filter(|j| j.state == DeliveryState::Unsupported)
            .count(),
        calendar_pending: jobs
            .iter()
            .filter(|j| j.state == DeliveryState::CalendarPending)
            .count(),
        baselined: jobs
            .iter()
            .filter(|j| j.state == DeliveryState::Baseline)
            .count(),
        ignored: jobs
            .iter()
            .filter(|j| j.state == DeliveryState::Ignored)
            .count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BootstrapPolicy, GoogleTask};
    use std::sync::Arc;

    struct Fake<F>(Mutex<F>);
    impl<F: FnMut(&Invocation) -> Result<Vec<u8>> + Send> Runner for Fake<F> {
        fn run(&self, invocation: &Invocation) -> Result<Vec<u8>> {
            (self.0.lock().unwrap())(invocation)
        }
    }

    struct Sandbox {
        _dir: tempfile::TempDir,
        root: PathBuf,
        db: PathBuf,
        binding: DestinationBinding,
        store: CaptureStore,
    }

    impl Sandbox {
        fn new(titles: &[(&str, &str)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            std::fs::write(root.join("inbox.md"), "sandbox fixture only").unwrap();
            let binding = DestinationBinding {
                notes_dir: root.to_str().unwrap().into(),
                inbox_path: root.join("inbox.md").to_str().unwrap().into(),
                dashboard_path: root.join("dashboard.md").to_str().unwrap().into(),
            };
            let db = root.join("capture.sqlite");
            let mut store = CaptureStore::open(&db).unwrap();
            let tasks: Vec<_> = titles
                .iter()
                .map(|(id, title)| GoogleTask {
                    id: (*id).into(),
                    title: (*title).into(),
                    status: "needsAction".into(),
                    ..Default::default()
                })
                .collect();
            store
                .ingest("account", "list", &tasks, BootstrapPolicy::ImportOpen)
                .unwrap();
            Self {
                _dir: dir,
                root,
                db,
                binding,
                store,
            }
        }

        fn adapter(
            &self,
            f: impl FnMut(&Invocation) -> Result<Vec<u8>> + Send + 'static,
        ) -> MindWeaver {
            MindWeaver {
                binary: "/sandbox/mw".into(),
                cwd: self.root.clone(),
                env: BTreeMap::new(),
                binding: Mutex::new(None),
                runner: Box::new(Fake(Mutex::new(f))),
            }
        }

        fn claim(&mut self) -> CaptureJob {
            let id = self.store.jobs().unwrap()[0].capture_id.clone();
            self.store.claim(&id, &self.binding).unwrap()
        }
    }

    fn config(binding: &DestinationBinding) -> Vec<u8> {
        let mut value = serde_json::to_value(binding).unwrap();
        value["schemaVersion"] = json!("1");
        serde_json::to_vec(&value).unwrap()
    }

    fn task(job: &CaptureJob) -> Value {
        json!({"id": format!("task:{}", job.capture_id), "stableId": job.capture_id,
            "identityKind": "stable", "sourceRevision": format!("sha256:{}", "a".repeat(64)),
            "sourceId": "inbox-source", "sourcePath": "inbox.md", "done": false,
            "todoSection": "Inbox", "text": job.title.trim()})
    }

    fn receipt(task: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"schemaVersion": "1", "alreadyCaptured": false,
            "projectionUpdated": true, "task": task}))
        .unwrap()
    }

    fn snapshot(binding: &DestinationBinding, items: Vec<Value>) -> Vec<u8> {
        serde_json::to_vec(&json!({"schemaVersion": "1", "source": "saved-markdown",
            "notesDir": binding.notes_dir, "dashboardPath": binding.dashboard_path, "items": items})).unwrap()
    }

    #[test]
    fn claim_is_durable_before_literal_writer_and_repeat_has_no_calls() {
        let mut s = Sandbox::new(&[("google-one", "--help"), ("google-two", "--help")]);
        let binding = s.binding.clone();
        let db = s.db.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let mw = s.adapter(move |i| {
            seen.lock().unwrap().push(i.args.clone());
            match i.args[0].as_str() {
                "config" => Ok(config(&binding)),
                "todos" if i.args == ["todos", "add", "--help"] => {
                    Ok(b"--capture-id <ID> --json".to_vec())
                }
                _ => {
                    assert_eq!(&i.args[..3], &["todos", "add", "--capture-id"]);
                    assert_eq!(&i.args[4..], &["--json", "--", "--help"]);
                    for (key, value) in [
                        ("NOTES_DIR", &binding.notes_dir),
                        ("MW_INBOX_PATH", &binding.inbox_path),
                        ("DASHBOARD_PATH", &binding.dashboard_path),
                    ] {
                        assert_eq!(
                            i.env.get(&OsString::from(key)),
                            Some(&OsString::from(value))
                        );
                    }
                    let job = CaptureStore::inspect(&db)
                        .unwrap()
                        .into_iter()
                        .find(|j| j.capture_id == i.args[3])
                        .unwrap();
                    assert_eq!(job.state, DeliveryState::Delivering);
                    assert_eq!(job.attempts, 1);
                    assert_eq!(job.binding.as_ref(), Some(&binding));
                    Ok(receipt(task(&job)))
                }
            }
        });
        let r = mw.deliver_pending(&mut s.store, 100).unwrap();
        assert_eq!((r.delivered, r.pending, r.blocked), (2, 0, 0));
        let n = calls.lock().unwrap().len();
        std::fs::remove_file(s.root.join("inbox.md")).unwrap();
        mw.deliver_pending(&mut s.store, 100).unwrap();
        assert_eq!(calls.lock().unwrap().len(), n);
        assert_eq!(n, 4); // One config, one help, two distinct Google identities.
        for job in s.store.jobs().unwrap() {
            assert_eq!(job.receipt.unwrap()["method"], "todos-add");
        }
    }

    #[test]
    fn interrupted_delivery_never_retries_and_reconcile_adopts_done_task() {
        let mut s = Sandbox::new(&[("one", "  Buy milk  ")]);
        let job = s.claim();
        let mw = s.adapter(|_| panic!("pending delivery must not invoke mw"));
        assert_eq!(mw.deliver_pending(&mut s.store, 100).unwrap().blocked, 1);
        let binding = s.binding.clone();
        let mut saved = task(&job);
        saved["done"] = json!(true);
        saved["todoSection"] = json!("Do Now");
        let mw = s.adapter(move |i| {
            if i.args == ["config", "json"] {
                return Ok(config(&binding));
            }
            assert_eq!(i.args, ["todos", "snapshot", "--json"]);
            Ok(snapshot(&binding, vec![saved.clone()]))
        });
        assert_eq!(
            mw.reconcile(&mut s.store, &job.capture_id)
                .unwrap()
                .reconciled,
            1
        );
        let saved = s.store.jobs().unwrap().remove(0);
        assert_eq!(saved.attempts, 1);
        let receipt = saved.receipt.unwrap();
        assert_eq!(receipt["method"], "snapshot-reconciliation");
        assert_eq!(receipt["task"]["done"], true);
        assert!(receipt.get("result").is_none());
        assert!(mw.reconcile(&mut s.store, &job.capture_id).is_err());
    }

    #[test]
    fn provider_edit_during_writer_preserves_receipt_and_review_state() {
        for writer_succeeds in [true, false] {
            let mut s = Sandbox::new(&[("one", "Original title")]);
            let job = s.store.jobs().unwrap().remove(0);
            let binding = s.binding.clone();
            let db = s.db.clone();
            let calls = Arc::new(Mutex::new(0));
            let writes = calls.clone();
            let mw = s.adapter(move |i| {
                if i.args == ["config", "json"] {
                    return Ok(config(&binding));
                }
                if i.args == ["todos", "add", "--help"] {
                    return Ok(b"--capture-id --json".to_vec());
                }
                assert_eq!(i.args[3], job.capture_id);
                let mut concurrent = CaptureStore::open(&db)?;
                assert_eq!(concurrent.jobs()?[0].state, DeliveryState::Delivering);
                let mut changed = job.observation.clone();
                changed.title = "Upstream edited title".into();
                concurrent.ingest("account", "list", &[changed], BootstrapPolicy::ImportOpen)?;
                *writes.lock().unwrap() += 1;
                if !writer_succeeds {
                    bail!("simulated ambiguous writer failure");
                }
                Ok(receipt(task(&job)))
            });
            let r = mw.deliver_pending(&mut s.store, 1).unwrap();
            assert_eq!(
                (r.delivered, r.blocked, r.pending),
                (usize::from(writer_succeeds), 1, 0)
            );
            let saved = s.store.jobs().unwrap().remove(0);
            assert_eq!(saved.state, DeliveryState::NeedsReview);
            assert!(saved.provider_changed);
            assert_eq!(saved.receipt.is_some(), writer_succeeds);
            assert_eq!(saved.title, "Original title");
            assert_eq!(saved.observation.title, "Upstream edited title");
            assert_eq!(saved.attempts, 1);
            assert!(saved.last_error.is_some());
            assert_eq!(CaptureStore::inspect(&s.db).unwrap()[0], saved);
            let r = mw.deliver_pending(&mut s.store, 1).unwrap();
            assert_eq!((r.delivered, r.blocked), (0, 1));
            assert_eq!(*calls.lock().unwrap(), 1);
        }
    }

    #[test]
    fn noncapture_states_remain_visible_across_polls_without_mw_calls() {
        let mut s = Sandbox::new(&[]);
        let unsupported = GoogleTask {
            id: "unsupported".into(),
            title: "Multiline\ntitle".into(),
            status: "needsAction".into(),
            ..Default::default()
        };
        let calendar = GoogleTask {
            id: "calendar".into(),
            title: "Dated title".into(),
            due: Some("2026-11-01T00:00:00.000Z".into()),
            status: "needsAction".into(),
            ..Default::default()
        };
        let ignored = GoogleTask {
            id: "ignored".into(),
            title: "Completed title".into(),
            status: "completed".into(),
            ..Default::default()
        };
        let baseline = GoogleTask {
            id: "baseline".into(),
            title: "Baseline title".into(),
            status: "needsAction".into(),
            ..Default::default()
        };
        let tasks = [unsupported, calendar, ignored];
        s.store
            .ingest("account", "list", &tasks, BootstrapPolicy::ImportOpen)
            .unwrap();
        s.store
            .ingest(
                "account",
                "baseline-list",
                std::slice::from_ref(&baseline),
                BootstrapPolicy::Baseline,
            )
            .unwrap();
        let mw = s.adapter(|_| panic!("noncapture states must not invoke mw"));
        let before = s.store.jobs().unwrap();
        for _ in 0..2 {
            s.store
                .ingest("account", "list", &tasks, BootstrapPolicy::ImportOpen)
                .unwrap();
            s.store
                .ingest(
                    "account",
                    "baseline-list",
                    std::slice::from_ref(&baseline),
                    BootstrapPolicy::Baseline,
                )
                .unwrap();
            let r = mw.deliver_pending(&mut s.store, 100).unwrap();
            assert_eq!((r.delivered, r.reconciled, r.pending), (0, 0, 0));
            assert_eq!(
                (
                    r.blocked,
                    r.unsupported,
                    r.calendar_pending,
                    r.baselined,
                    r.ignored
                ),
                (1, 1, 1, 1, 1)
            );
            for job in &before {
                assert!(s.store.claim(&job.capture_id, &s.binding).is_err());
                assert!(mw.reconcile(&mut s.store, &job.capture_id).is_err());
            }
            assert_eq!(s.store.jobs().unwrap(), before);
            assert!(
                before
                    .iter()
                    .all(|j| j.attempts == 0 && j.binding.is_none() && j.receipt.is_none())
            );
        }
    }

    #[test]
    fn missing_moved_edited_archived_duplicate_or_invalid_provenance_stays_blocked() {
        for scenario in [
            "missing",
            "moved",
            "edited",
            "archived",
            "duplicate",
            "provenance",
            "revision",
            "identity",
            "traversal",
        ] {
            let mut s = Sandbox::new(&[("one", "Buy milk")]);
            let job = s.claim();
            std::fs::write(s.root.join("moved.md"), "fixture").unwrap();
            let mut t = task(&job);
            match scenario {
                "moved" => t["sourcePath"] = json!("moved.md"),
                "edited" => t["text"] = json!("Edited"),
                "revision" => t["sourceRevision"] = json!(format!("sha256:{}", "A".repeat(64))),
                "identity" => t["identityKind"] = json!("positional"),
                "traversal" => t["sourcePath"] = json!("../inbox.md"),
                _ => {}
            }
            let items = match scenario {
                "missing" | "archived" => vec![],
                "duplicate" => vec![t.clone(), t],
                _ => vec![t],
            };
            let binding = s.binding.clone();
            let mw = s.adapter(move |i| {
                if i.args == ["config", "json"] {
                    return Ok(config(&binding));
                }
                assert_eq!(i.args, ["todos", "snapshot", "--json"]);
                let mut v: Value =
                    serde_json::from_slice(&snapshot(&binding, items.clone())).unwrap();
                if scenario == "provenance" {
                    v["source"] = json!("database");
                }
                Ok(serde_json::to_vec(&v).unwrap())
            });
            for _ in 0..2 {
                assert_eq!(
                    mw.reconcile(&mut s.store, &job.capture_id).unwrap().blocked,
                    1,
                    "{scenario}"
                );
                let saved = s.store.jobs().unwrap().remove(0);
                assert_eq!(saved.state, DeliveryState::NeedsReview);
                assert_eq!(saved.attempts, 1);
                assert!(saved.receipt.is_none());
            }
        }
    }

    #[test]
    fn config_change_cannot_retarget_unknown_attempt() {
        let mut s = Sandbox::new(&[("one", "Buy milk")]);
        let job = s.claim();
        let mut binding = s.binding.clone();
        binding.inbox_path = s.root.join("other.md").to_str().unwrap().into();
        let mw = s.adapter(move |i| {
            assert_eq!(i.args, ["config", "json"]);
            Ok(config(&binding))
        });
        assert_eq!(
            mw.reconcile(&mut s.store, &job.capture_id).unwrap().blocked,
            1
        );
        assert_eq!(s.store.jobs().unwrap()[0].binding, Some(s.binding.clone()));
    }

    #[test]
    fn invalid_outputs_fail_closed_and_next_job_can_proceed() {
        for scenario in [
            "nonzero",
            "partial",
            "schema",
            "stableId",
            "sourceId",
            "done",
            "section",
            "projection",
        ] {
            let mut s = Sandbox::new(&[
                ("one", "private Google title"),
                ("two", "private Google title"),
            ]);
            let binding = s.binding.clone();
            let jobs = s.store.jobs().unwrap();
            let mut writes = 0;
            let mw = s.adapter(move |i| {
                if i.args == ["config", "json"] {
                    return Ok(config(&binding));
                }
                if i.args == ["todos", "add", "--help"] {
                    return Ok(b"--capture-id --json".to_vec());
                }
                let job = jobs.iter().find(|j| j.capture_id == i.args[3]).unwrap();
                writes += 1;
                if writes > 1 {
                    return Ok(receipt(task(job)));
                }
                if scenario == "nonzero" {
                    bail!("secret stderr and Google private title");
                }
                if scenario == "partial" {
                    return Ok(b"{\"schemaVersion\":".to_vec());
                }
                let mut v: Value = serde_json::from_slice(&receipt(task(job))).unwrap();
                match scenario {
                    "schema" => v["schemaVersion"] = json!(2),
                    "stableId" => v["task"]["stableId"] = json!("wrong"),
                    "sourceId" => v["task"]["sourceId"] = json!(""),
                    "done" => v["task"]["done"] = json!(true),
                    "section" => v["task"]["todoSection"] = json!("Do Now"),
                    "projection" => v["projectionUpdated"] = Value::Null,
                    _ => unreachable!(),
                }
                Ok(serde_json::to_vec(&v).unwrap())
            });
            let r = mw.deliver_pending(&mut s.store, 100).unwrap();
            assert_eq!((r.delivered, r.blocked, r.pending), (1, 1, 0), "{scenario}");
            let blocked = s
                .store
                .jobs()
                .unwrap()
                .into_iter()
                .find(|j| j.state == DeliveryState::NeedsReview)
                .unwrap();
            let error = blocked.last_error.unwrap();
            assert!(!error.contains("secret") && !error.contains("private"));
            assert!(error.contains(&blocked.capture_id));
            assert_eq!(mw.deliver_pending(&mut s.store, 100).unwrap().delivered, 0);
        }
    }

    #[test]
    fn preflights_consume_budget_before_claim_guard() {
        let mut s = Sandbox::new(&[("one", "title")]);
        let before = s.store.jobs().unwrap();
        let binding = s.binding.clone();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(10);
        let clock = Arc::new(Mutex::new(now));
        let elapsed = clock.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let mw = s.adapter(move |i| {
            seen.lock().unwrap().push(i.args.clone());
            *elapsed.lock().unwrap() += Duration::from_secs(3);
            if i.args == ["config", "json"] {
                return Ok(config(&binding));
            }
            assert_eq!(i.args, ["todos", "add", "--help"]);
            Ok(b"--capture-id --json".to_vec())
        });
        let mut checks = 0;
        let (r, deferred) = mw
            .deliver_pending_guarded(&mut s.store, 1, || {
                checks += 1;
                assert_eq!(calls.lock().unwrap().len(), 2);
                deadline.saturating_duration_since(*clock.lock().unwrap()) >= Duration::from_secs(5)
            })
            .unwrap();
        assert!(deferred);
        assert_eq!(checks, 1);
        assert_eq!((r.delivered, r.pending, r.blocked), (0, 1, 0));
        assert_eq!(s.store.jobs().unwrap(), before);
        assert_eq!(before[0].state, DeliveryState::Pending);
        assert_eq!(before[0].attempts, 0);
    }

    #[test]
    fn expired_deadline_allows_only_read_only_preflight() {
        let mut s = Sandbox::new(&[("one", "title")]);
        let before = s.store.jobs().unwrap();
        let binding = s.binding.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let mw = s.adapter(move |i| {
            seen.lock().unwrap().push(i.args.clone());
            if i.args == ["config", "json"] {
                return Ok(config(&binding));
            }
            assert_eq!(i.args, ["todos", "add", "--help"]);
            Ok(b"--capture-id --json".to_vec())
        });
        let (r, deferred) = mw
            .deliver_pending_before(&mut s.store, 1, Instant::now(), Duration::from_secs(5))
            .unwrap();
        assert!(deferred);
        assert_eq!((r.delivered, r.pending, r.blocked), (0, 1, 0));
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert_eq!(s.store.jobs().unwrap(), before);
        assert_eq!(before[0].attempts, 0);
    }

    #[test]
    fn claim_guard_rechecks_each_job_and_preserves_partial_report() {
        let mut s = Sandbox::new(&[("one", "first"), ("two", "second")]);
        let jobs = s.store.jobs().unwrap();
        let pending = jobs[1].clone();
        let binding = s.binding.clone();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(5);
        let clock = Arc::new(Mutex::new(now));
        let elapsed = clock.clone();
        let writes = Arc::new(Mutex::new(0));
        let count = writes.clone();
        let mw = s.adapter(move |i| {
            if i.args == ["config", "json"] {
                return Ok(config(&binding));
            }
            if i.args == ["todos", "add", "--help"] {
                return Ok(b"--capture-id --json".to_vec());
            }
            assert_eq!(i.args[3], jobs[0].capture_id);
            *count.lock().unwrap() += 1;
            *elapsed.lock().unwrap() += Duration::from_secs(1);
            Ok(receipt(task(&jobs[0])))
        });
        let (r, deferred) = mw
            .deliver_pending_guarded(&mut s.store, 2, || {
                deadline.saturating_duration_since(*clock.lock().unwrap()) >= Duration::from_secs(5)
            })
            .unwrap();
        assert!(deferred);
        assert_eq!((r.delivered, r.pending, r.blocked), (1, 1, 0));
        assert_eq!(*writes.lock().unwrap(), 1);
        assert_eq!(s.store.jobs().unwrap()[1], pending);
        assert_eq!(pending.attempts, 0);
    }

    #[test]
    fn normal_budget_delivers_with_original_stable_id() {
        let mut s = Sandbox::new(&[("one", "title")]);
        let job = s.store.jobs().unwrap().remove(0);
        let expected_id = job.capture_id.clone();
        let binding = s.binding.clone();
        let mw = s.adapter(move |i| {
            if i.args == ["config", "json"] {
                return Ok(config(&binding));
            }
            if i.args == ["todos", "add", "--help"] {
                return Ok(b"--capture-id --json".to_vec());
            }
            assert_eq!(i.args[3], job.capture_id);
            Ok(receipt(task(&job)))
        });
        let (r, deferred) = mw
            .deliver_pending_before(
                &mut s.store,
                1,
                Instant::now() + Duration::from_secs(3600),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(!deferred);
        assert_eq!((r.delivered, r.pending, r.blocked), (1, 0, 0));
        let saved = s.store.jobs().unwrap().remove(0);
        assert_eq!(saved.capture_id, expected_id);
        assert_eq!(saved.attempts, 1);
        assert_eq!(
            saved.receipt.unwrap()["result"]["task"]["stableId"],
            expected_id
        );
    }

    #[test]
    fn claim_guard_only_runs_after_successful_preflight_with_pending_work() {
        let mut s = Sandbox::new(&[("one", "title")]);
        let binding = s.binding.clone();
        let mw = s.adapter(move |i| {
            if i.args == ["config", "json"] {
                return Ok(config(&binding));
            }
            assert_eq!(i.args, ["todos", "add", "--help"]);
            Ok(b"--json".to_vec())
        });
        let (r, deferred) = mw
            .deliver_pending_guarded(&mut s.store, 0, || panic!("zero limit"))
            .unwrap();
        assert!(!deferred);
        assert_eq!(r.pending, 1);
        assert!(
            mw.deliver_pending_guarded(&mut s.store, 1, || panic!("failed preflight"))
                .is_err()
        );
        assert_eq!(s.store.jobs().unwrap()[0].attempts, 0);

        let mut empty = Sandbox::new(&[]);
        let mw = empty.adapter(|_| panic!("empty queue"));
        let (_, deferred) = mw
            .deliver_pending_guarded(&mut empty.store, 1, || panic!("empty queue"))
            .unwrap();
        assert!(!deferred);
    }

    #[test]
    fn preflight_and_zero_limit_do_not_claim() {
        let mut s = Sandbox::new(&[("one", "title")]);
        let binding = s.binding.clone();
        let mw = s.adapter(move |i| {
            if i.args == ["config", "json"] {
                return Ok(config(&binding));
            }
            assert_eq!(i.args, ["todos", "add", "--help"]);
            Ok(b"--json".to_vec())
        });
        mw.deliver_pending(&mut s.store, 0).unwrap();
        assert!(mw.deliver_pending(&mut s.store, 1).is_err());
        assert_eq!(s.store.jobs().unwrap()[0].attempts, 0);
        assert_eq!(s.store.jobs().unwrap()[0].state, DeliveryState::Pending);
    }

    #[test]
    fn failed_receipt_commit_blocks_without_retrying_writer() {
        let mut s = Sandbox::new(&[("one", "title")]);
        let conn = rusqlite::Connection::open(&s.db).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_receipt BEFORE UPDATE ON jobs
             WHEN json_extract(NEW.job, '$.receipt_recorded') = 1
             BEGIN SELECT RAISE(ABORT, 'test receipt commit failure'); END;",
        )
        .unwrap();
        let binding = s.binding.clone();
        let job = s.store.jobs().unwrap().remove(0);
        let writes = Arc::new(Mutex::new(0));
        let count = writes.clone();
        let mw = s.adapter(move |i| {
            if i.args == ["config", "json"] {
                return Ok(config(&binding));
            }
            if i.args == ["todos", "add", "--help"] {
                return Ok(b"--capture-id --json".to_vec());
            }
            *count.lock().unwrap() += 1;
            Ok(receipt(task(&job)))
        });
        assert_eq!(mw.deliver_pending(&mut s.store, 1).unwrap().blocked, 1);
        mw.deliver_pending(&mut s.store, 1).unwrap();
        assert_eq!(*writes.lock().unwrap(), 1);
        let job = s.store.jobs().unwrap().remove(0);
        assert_eq!(job.state, DeliveryState::NeedsReview);
        assert!(job.receipt.is_none());
        assert_eq!(job.attempts, 1);
    }

    #[test]
    fn cached_binding_pins_writer_even_if_config_file_changes() {
        let mut s = Sandbox::new(&[("one", "title")]);
        let binding = s.binding.clone();
        let job = s.store.jobs().unwrap().remove(0);
        let calls = Arc::new(Mutex::new(0));
        let count = calls.clone();
        let mw = s.adapter(move |i| {
            if i.args == ["config", "json"] {
                *count.lock().unwrap() += 1;
                ensure!(
                    *count.lock().unwrap() == 1,
                    "config drift must not replace cached binding"
                );
                return Ok(config(&binding));
            }
            assert_eq!(
                i.env[&OsString::from("MW_INBOX_PATH")],
                OsString::from(&binding.inbox_path)
            );
            if i.args == ["todos", "add", "--help"] {
                return Ok(b"--capture-id --json".to_vec());
            }
            // An existing captured task may already be completed; do not reopen it.
            let mut v: Value = serde_json::from_slice(&receipt(task(&job))).unwrap();
            v["alreadyCaptured"] = json!(true);
            v["task"]["done"] = json!(true);
            v["task"]["todoSection"] = json!("Do Now");
            Ok(serde_json::to_vec(&v).unwrap())
        });
        assert_eq!(mw.binding().unwrap(), s.binding);
        assert_eq!(mw.deliver_pending(&mut s.store, 1).unwrap().delivered, 1);
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn constructor_environment_is_allowlisted_and_discovery_is_cached() {
        assert!(MindWeaver::new("relative/mw").is_err());
        let mw = MindWeaver::new("/usr/bin/true").unwrap();
        assert!(mw.env.keys().all(|k| {
            ENV_KEYS
                .iter()
                .any(|key| k.as_os_str() == std::ffi::OsStr::new(key))
        }));
        for key in [
            "GOOGLE_ACCESS_TOKEN",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "HIVE_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "OPENAI_API_KEY",
            "DYLD_INSERT_LIBRARIES",
        ] {
            assert!(!mw.env.contains_key(&OsString::from(key)));
        }
        let s = Sandbox::new(&[]);
        let binding = s.binding.clone();
        let count = Arc::new(Mutex::new(0));
        let calls = count.clone();
        let mw = s.adapter(move |_| {
            *calls.lock().unwrap() += 1;
            Ok(config(&binding))
        });
        assert_eq!(mw.binding().unwrap(), s.binding);
        assert_eq!(mw.binding().unwrap(), s.binding);
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn real_process_runner_bounds_output_timeout_nonzero_and_spawn_failure() {
        let runner = ProcessRunner {
            // Isolate the output guard from host scheduling latency. The short
            // timeout is exercised separately below, not raced against output.
            timeout: Duration::from_secs(2),
            output_limit: 1024,
        };
        let mut invocation = Invocation {
            binary: "/usr/bin/yes".into(),
            cwd: std::env::current_dir().unwrap(),
            env: BTreeMap::new(),
            args: vec![],
        };
        assert!(
            runner
                .run(&invocation)
                .unwrap_err()
                .to_string()
                .contains("output limit")
        );
        invocation.binary = "/bin/sleep".into();
        invocation.args = vec!["5".into()];
        let timeout_runner = ProcessRunner {
            timeout: Duration::from_millis(100),
            output_limit: 1024,
        };
        let start = Instant::now();
        assert!(
            timeout_runner
                .run(&invocation)
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        invocation.binary = "/usr/bin/false".into();
        invocation.args.clear();
        assert!(runner.run(&invocation).is_err());
        invocation.binary = "/nonexistent/hive-test-mw".into();
        assert!(runner.run(&invocation).is_err());
    }
}
