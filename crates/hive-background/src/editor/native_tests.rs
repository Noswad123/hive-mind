//! Explicit opt-in only: real Neovim, real runtime, exclusively fixture-owned I/O.
use super::*;
use std::{
    collections::BTreeMap,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    process::{Child, Command, Stdio},
};

// The helper has no provider/auth imports or passthrough command execution. All
// paths come from its own physical directory, never HOME or application state.
const HELPER: &str = r#"#!/usr/bin/python3
import fcntl, json, os, pathlib, sys, time
root = pathlib.Path(__file__).resolve().parent
os.umask(0o077)
def emit(value):
    print(json.dumps(dict(schemaVersion='1', **value)), flush=True)
def lock(name, operation):
    fd = os.open(str(root / 'state' / name), os.O_RDWR | os.O_CREAT, 0o600)
    fcntl.flock(fd, operation)
    return fd
if sys.argv[1:] == ['--hold-delivery']:
    fd = lock('delivery.lock', fcntl.LOCK_EX)
    (root / 'holder-ready').write_text('ready')
    time.sleep(2)
    (root / 'holder-done').write_text('done')
    os.close(fd)
    sys.exit(0)
assert sys.argv[1:3] == ['captures', 'background'], 'unsupported fixture command'
action = sys.argv[3]
args = sys.argv[4:]
def arg(name):
    return args[args.index(name) + 1]
fixture = json.loads((root / 'fixture.json').read_text())
if action == 'paths':
    emit(dict(configPath=str(root / 'state' / 'config.json'), registryPath=str(root / 'state' / 'editors')))
elif action == 'editor-register':
    entry = dict(schemaVersion='1', pid=int(arg('--pid')), sessionId=arg('--session-id'),
                 socketPath=arg('--socket'), processStart='fixture-bootstrap-only')
    assert entry['socketPath'] == str(root / 'nvim.sock')
    (root / 'registration.json').write_text(json.dumps(entry))
    emit(dict(registered=True, entry=entry, config=fixture['public'], configDigest=fixture['digest']))
elif action == 'editor-context':
    assert arg('--config-digest') == fixture['digest']
    if (root / 'context-delay').exists():
        time.sleep(2.5)
    emit(dict(context=fixture['public']['destination']))
elif action == 'deliver-owned':
    assert arg('--config-digest') == fixture['digest']
    with (root / 'launches').open('a') as out:
        out.write(arg('--cycle-id') + '\n')
    if (root / 'uncertain').exists():
        assert (root / 'holder-ready').exists(), 'fixture lock holder not started'
        print('malformed fixture reply', flush=True)
        sys.exit(1)
    admission = lock('admission.lock', fcntl.LOCK_SH)
    delivery = lock('delivery.lock', fcntl.LOCK_EX)
    time.sleep(2.5)  # Exercise a slow synchronous writer independently of context.
    with (root / 'notes' / 'Inbox.md').open('a') as inbox:
        inbox.write('fixture delivery\n')
    os.close(delivery)
    os.close(admission)
    emit(dict(kind='capture-background-delivery', cycleId=arg('--cycle-id'), writerExited=True,
              report=fixture['report']))
elif action == 'lease-status':
    dead = False
    try:
        fd = lock('delivery.lock', fcntl.LOCK_EX | fcntl.LOCK_NB)
        dead = True
        os.close(fd)
    except BlockingIOError:
        pass
    emit(dict(cycleId=arg('--cycle-id'), writerGroupDead=dead))
elif action in ('editor-unregister', 'unregister'):
    emit(dict(unregistered=True))
else:
    raise AssertionError('unsupported fixture action: ' + action)
"#;

const LUA: &str = r#"
local root, runtime = vim.env.HIVE_FIXTURE_ROOT, vim.env.HIVE_FIXTURE_RUNTIME
assert(root and runtime)
vim.opt.runtimepath = { runtime, vim.env.VIMRUNTIME }
vim.o.swapfile, vim.o.backup, vim.o.writebackup, vim.o.undofile = false, false, false, false
vim.g.hive_bin = root .. '/hive'
local fixture = vim.json.decode(table.concat(vim.fn.readfile(root .. '/fixture.json'), '\n'))
local native_system = vim.fn.system
local inbox
local function status()
  return { readonly = vim.bo[inbox].readonly, modifiable = vim.bo[inbox].modifiable,
    modified = vim.bo[inbox].modified, hidden = vim.fn.bufwinid(inbox) == -1,
    busy = require('util.mw_ops').is_busy(),
    phase = require('util.hive_background').state().phase,
    lines = vim.api.nvim_buf_get_lines(inbox, 0, -1, false) }
end
local function observe(value)
  vim.fn.writefile({vim.json.encode(value)}, root .. '/observations.jsonl', 'a')
end
vim.fn.system = function(argv, ...)
  assert(type(argv) == 'table', 'fixture forbids shell-string execution')
  if vim.tbl_contains(argv, 'deliver-owned') then
    local before = status()
    assert(before.readonly and not before.modifiable and before.busy)
    local wrote = pcall(vim.api.nvim_buf_call, inbox, function() vim.cmd('silent write!') end)
    assert(not wrote, 'write! bypassed operation barrier')
    before.action, before.writeBlocked = 'deliver-owned', not wrote
    observe(before)
  end
  local output = native_system(argv, ...)
  if vim.tbl_contains(argv, 'lease-status') then
    local checked = vim.json.decode(output)
    local during = status()
    assert(during.readonly and not during.modifiable and during.busy)
    during.action, during.writerGroupDead = 'lease-status', checked.writerGroupDead
    observe(during)
  end
  return output
end
local setup = require('util.hive_background').setup()
assert(setup.phase == 'done' and setup.reason == 'registered', vim.inspect(setup))
inbox = vim.fn.bufadd(root .. '/notes/Inbox.md')
vim.fn.bufload(inbox)
assert(status().hidden)
local ops = require('util.mw_ops')
local with_operation = ops.with_operation
ops.with_operation = function(opts, start)
  assert(vim.deep_equal(opts.resolved_context.env, fixture.public.env))
  assert(opts.resolved_context.bin == fixture.public.mwBinary)
  assert(opts.resolved_context.cwd == root)
  return with_operation(opts, start)
end
_G.hive_fixture_control = function(payload)
  if payload.action == 'dirty' then vim.bo[inbox].modified = true
  elseif payload.action == 'clear' then vim.bo[inbox].modified = false
  else assert(payload.action == 'status') end
  return status()
end
for name in pairs(package.loaded) do
  assert(not name:match('keychain') and not name:match('google') and not name:match('credential'))
end
vim.fn.writefile({'ready'}, root .. '/ready')
-- Keep servicing native RPC and recovery timers, without sourcing an init file.
vim.wait(60000, function() return false end, 10)
vim.cmd('qa!')
"#;

struct OwnedGroup(Child);

impl Drop for OwnedGroup {
    fn drop(&mut self) {
        // This child was spawned with process_group(0) and is never reaped before
        // Drop, so its PID cannot be reused. Never signal a discovered/user PID.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

struct Fixture {
    _editor: OwnedGroup,
    _temp: tempfile::TempDir,
    paths: Paths,
    config: WorkerConfig,
    entry: EditorEntry,
    process: ProcessInfo,
}

impl Fixture {
    fn new(uncertain: bool) -> Self {
        // No fallback to PATH, user configuration, or a running editor.
        let binary = fs::canonicalize(
            std::env::var_os("NVIM_TEST_BIN")
                .expect("explicit NVIM_TEST_BIN is required for ignored native tests"),
        )
        .unwrap();
        let runtime = fs::canonicalize(
            std::env::var_os("HIVE_NVIM_RUNTIME")
                .expect("explicit HIVE_NVIM_RUNTIME is required for ignored native tests"),
        )
        .unwrap();
        assert!(runtime.join("lua/util/hive_background.lua").is_file());
        assert!(Path::new("/usr/bin/python3").is_file());
        let temp = super::tests::private_fixture();
        let root = fs::canonicalize(temp.path()).unwrap();
        let paths = Paths::in_root(root.join("state"));
        paths.ensure().unwrap();
        for dir in [
            "notes",
            "home",
            "xdg-config",
            "xdg-data",
            "xdg-state",
            "xdg-cache",
        ] {
            fs::create_dir(root.join(dir)).unwrap();
        }
        fs::write(root.join("notes/Inbox.md"), "fixture inbox\n").unwrap();
        fs::write(root.join("notes/dashboard.md"), "fixture dashboard\n").unwrap();
        fs::write(root.join("queue.json"), "{}").unwrap();
        fs::write(root.join("hive"), HELPER).unwrap();
        fs::write(root.join("mw"), "#!/bin/sh\nexit 97\n").unwrap();
        for name in ["hive", "mw"] {
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = WorkerConfig {
            schema_version: "1".into(),
            connection: "fixture".into(),
            account_namespace: format!("google-sub-v1:{}", "a".repeat(64)),
            queue_path: root.join("queue.json"),
            list_id: "fixture".into(),
            mw_binary: root.join("mw"),
            hive_binary: root.join("hive"),
            nvim_binary: binary.clone(),
            working_directory: root.clone(),
            destination: DestinationBinding {
                notes_dir: root.join("notes").to_str().unwrap().into(),
                inbox_path: root.join("notes/Inbox.md").to_str().unwrap().into(),
                dashboard_path: root.join("notes/dashboard.md").to_str().unwrap().into(),
            },
            mw_environment: BTreeMap::from([
                (
                    "NOTES_DIR".into(),
                    root.join("notes").to_str().unwrap().into(),
                ),
                (
                    "XDG_CONFIG_HOME".into(),
                    root.join("xdg-config").to_str().unwrap().into(),
                ),
                (
                    "XDG_DATA_HOME".into(),
                    root.join("xdg-data").to_str().unwrap().into(),
                ),
            ]),
            interval_seconds: 60,
            cycle_timeout_seconds: 35,
            delivery_limit: 1,
        };
        crate::state::write_json(&paths.config, &config).unwrap();
        let report = DeliveryReport {
            delivered: 1,
            ..Default::default()
        };
        fs::write(root.join("fixture.json"), serde_json::to_vec(&json!({
            "public":public_config(&config).unwrap(), "digest":config_digest(&config).unwrap(), "report":report,
        })).unwrap()).unwrap();
        fs::write(root.join("fixture.lua"), LUA).unwrap();
        if uncertain {
            fs::write(root.join("uncertain"), "fixture").unwrap();
        }
        let socket = root.join("nvim.sock");
        let log = fs::File::create(root.join("nvim.log")).unwrap();
        let child = Command::new(binary)
            .args([
                "--headless",
                "-u",
                "NONE",
                "-i",
                "NONE",
                "--noplugin",
                "-n",
                "--listen",
            ])
            .arg(&socket)
            .args([
                "-c",
                "lua dofile(vim.env.HIVE_FIXTURE_ROOT .. '/fixture.lua')",
            ])
            .current_dir(&root)
            .env_clear()
            .env("HOME", root.join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env("XDG_CONFIG_HOME", root.join("xdg-config"))
            .env("XDG_DATA_HOME", root.join("xdg-data"))
            .env("XDG_STATE_HOME", root.join("xdg-state"))
            .env("XDG_CACHE_HOME", root.join("xdg-cache"))
            .env("TMPDIR", &root)
            .env("HIVE_FIXTURE_ROOT", &root)
            .env("HIVE_FIXTURE_RUNTIME", runtime)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .unwrap();
        let editor = OwnedGroup(child);
        let until = Instant::now() + Duration::from_secs(10);
        while !root.join("ready").exists() {
            assert!(
                Instant::now() < until,
                "fixture startup failed: {}",
                fs::read_to_string(root.join("nvim.log")).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(20));
        }
        let receipt: EditorEntry =
            serde_json::from_slice(&fs::read(root.join("registration.json")).unwrap()).unwrap();
        assert_eq!(receipt.pid, editor.0.id());
        assert_eq!(receipt.socket_path, socket);
        // Replace bootstrap proof with the REAL OS start identity. No fake
        // registration enters the registry used by Rust discovery.
        let reply = register(&paths, &config, editor.0.id(), &socket, &receipt.session_id).unwrap();
        let entry: EditorEntry = serde_json::from_value(reply["entry"].clone()).unwrap();
        let process = crate::process::find_process(editor.0.id())
            .unwrap()
            .unwrap();
        assert_eq!(entry.process_start, process.start_identity);
        assert_ne!(entry.process_start, receipt.process_start);
        Self {
            _editor: editor,
            _temp: temp,
            paths,
            config,
            entry,
            process,
        }
    }

    fn root(&self) -> &Path {
        self._temp.path()
    }

    fn lease(&self) -> DeliveryLease {
        let coordinator = crate::process::find_process(std::process::id())
            .unwrap()
            .unwrap();
        DeliveryLease {
            schema_version: "1".into(),
            cycle_id: crate::nonce().unwrap(),
            config_digest: config_digest(&self.config).unwrap(),
            coordinator_pid: coordinator.pid,
            coordinator_start: coordinator.start_identity,
            deadline_ms: now_ms() + 35_000,
            destination: self.config.destination.clone(),
            editor_session: Some(self.entry.session_id.clone()),
            helper_pid: None,
            helper_pgid: None,
        }
    }

    fn control(&self, action: &str) -> Value {
        control(&self.entry, action).unwrap()
    }

    fn observations(&self) -> Vec<Value> {
        fs::read_to_string(self.root().join("observations.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn backend(&self) -> FixtureBackend {
        FixtureBackend {
            process: self.process.clone(),
            actions: Vec::new(),
            phases: Vec::new(),
        }
    }

    fn start_holder(&self) -> OwnedGroup {
        let child = Command::new("/usr/bin/python3")
            .arg(self.root().join("hive"))
            .arg("--hold-delivery")
            .current_dir(self.root())
            .env_clear()
            .env("HOME", self.root().join("home"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let holder = OwnedGroup(child);
        let until = Instant::now() + Duration::from_secs(2);
        while !self.root().join("holder-ready").exists() {
            assert!(
                Instant::now() < until,
                "fixture holder did not acquire delivery lock"
            );
            thread::sleep(Duration::from_millis(10));
        }
        holder
    }
}

// Only these tests use this fixed inspection script, and only on their owned
// socket, after the same native peer UID/PID verification as production RPC.
fn control(entry: &EditorEntry, action: &str) -> Result<Value> {
    let socket = connect(&entry.socket_path, Instant::now() + Duration::from_secs(2))?;
    verify_peer(&socket, entry.pid)?;
    let mut stream = DeadlineStream {
        stream: socket.take(MAX_RPC),
        until: Instant::now() + Duration::from_secs(2),
    };
    let message = rmpv::Value::Array(vec![
        0.into(),
        7.into(),
        "nvim_exec_lua".into(),
        rmpv::Value::Array(vec![
            "return _G.hive_fixture_control(...)".into(),
            rmpv::ext::to_value(vec![json!({"action":action})])?,
        ]),
    ]);
    rmpv::encode::write_value(&mut stream, &message)?;
    let response = rmpv::decode::read_value_with_max_depth(&mut stream, 32)?;
    let fields = response.as_array().context("fixture control response")?;
    ensure!(
        fields.len() == 4
            && fields[0].as_u64() == Some(1)
            && fields[1].as_u64() == Some(7)
            && fields[2].is_nil(),
        "fixture control failed: {response}"
    );
    Ok(rmpv::ext::from_value(fields[3].clone())?)
}

struct FixtureBackend {
    process: ProcessInfo,
    actions: Vec<String>,
    phases: Vec<String>,
}

impl Backend for FixtureBackend {
    fn inventory(&mut self) -> Result<Vec<ProcessInfo>> {
        // Deliberately never enumerate or connect to the user's live editors.
        Ok(vec![
            crate::process::find_process(self.process.pid)?.context("fixture editor exited")?,
        ])
    }

    fn find_process(&mut self, pid: u32) -> Result<Option<ProcessInfo>> {
        ensure!(
            pid == self.process.pid,
            "non-fixture process lookup forbidden"
        );
        Native.find_process(pid)
    }

    fn rpc(&mut self, entry: &EditorEntry, payload: Value, timeout: Duration) -> Result<Value> {
        ensure!(entry.pid == self.process.pid, "non-fixture RPC forbidden");
        self.actions
            .push(payload["action"].as_str().unwrap().into());
        let response = Native.rpc(entry, payload, timeout)?;
        let phase = response["phase"].as_str().unwrap_or("unknown");
        self.phases.push(phase.into());
        if phase == "running" {
            let guarded = control(entry, "status")?;
            // A recovery timer can finish between the response and inspection.
            if guarded["phase"] == "running" {
                ensure!(
                    guarded["busy"] == true
                        && guarded["readonly"] == true
                        && guarded["modifiable"] == false,
                    "running fixture lost barrier"
                );
            }
        }
        Ok(response)
    }
}

#[test]
#[ignore = "requires explicit NVIM_TEST_BIN and HIVE_NVIM_RUNTIME; launches isolated real Neovim"]
fn native_neovim_dirty_hidden_then_clean_delivery() {
    let fixture = Fixture::new(false);
    let admission = FileLock::acquire(&fixture.paths.admission_lock, true, Duration::ZERO).unwrap();
    let dirty = fixture.control("dirty");
    assert_eq!(dirty["hidden"], true);
    assert_eq!(dirty["modified"], true);
    let original = fs::read(&fixture.config.destination.inbox_path).unwrap();
    let mut ready_backend = fixture.backend();
    assert_eq!(
        readiness(
            &mut ready_backend,
            &fixture.config,
            vec![fixture.entry.clone()]
        ),
        EditorReadiness::Deferred(DeferralReason::DirtyBuffers)
    );
    assert_eq!(ready_backend.actions, ["describe"]);
    let mut backend = fixture.backend();
    super::tests::deferred(
        deliver(
            &mut backend,
            &fixture.config,
            &fixture.lease(),
            vec![fixture.entry.clone()],
        ),
        DeferralReason::DirtyBuffers,
    );
    assert_eq!(backend.actions, ["describe"]);
    assert!(!fixture.root().join("launches").exists());
    assert_eq!(
        fs::read(&fixture.config.destination.inbox_path).unwrap(),
        original
    );
    fixture.control("clear"); // Intentionally clear fixture dirtiness without saving.
    fs::write(fixture.root().join("context-delay"), "fixture-only delay").unwrap();
    let mut ready_backend = fixture.backend();
    let readiness_started = Instant::now();
    assert_eq!(
        readiness(
            &mut ready_backend,
            &fixture.config,
            vec![fixture.entry.clone()]
        ),
        EditorReadiness::Ready
    );
    assert!(readiness_started.elapsed() >= Duration::from_millis(2500));
    assert_eq!(ready_backend.actions, ["describe"]);
    assert!(!fixture.root().join("launches").exists());
    assert_eq!(fs::read_dir(&fixture.paths.leases).unwrap().count(), 0);
    assert_eq!(
        fs::read(&fixture.config.destination.inbox_path).unwrap(),
        original
    );
    let mut backend = fixture.backend();
    let started = Instant::now();
    match deliver(
        &mut backend,
        &fixture.config,
        &fixture.lease(),
        vec![fixture.entry.clone()],
    ) {
        EditorDecision::Completed(report) => assert_eq!(report.delivered, 1),
        other => panic!("real Neovim clean delivery: {other:?}"),
    }
    assert!(started.elapsed() >= Duration::from_secs(5));
    let status = fixture.control("status");
    assert_eq!(
        status["lines"],
        json!(["fixture inbox", "fixture delivery"])
    );
    assert_eq!(status["readonly"], false);
    assert_eq!(status["modifiable"], true);
    assert_eq!(status["busy"], false);
    assert_eq!(status["modified"], false);
    assert_eq!(
        fs::read_to_string(&fixture.config.destination.inbox_path).unwrap(),
        "fixture inbox\nfixture delivery\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.root().join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let observed = fixture.observations();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0]["writeBlocked"], true);
    assert_eq!(observed[0]["readonly"], true);
    assert_eq!(
        backend
            .actions
            .iter()
            .filter(|action| *action == "deliver")
            .count(),
        1
    );
    drop(admission);
}

#[test]
#[ignore = "requires explicit NVIM_TEST_BIN and HIVE_NVIM_RUNTIME; launches isolated real Neovim"]
fn native_neovim_unknown_writer_retains_barrier_until_lock_proof() {
    let fixture = Fixture::new(true);
    let _admission =
        FileLock::acquire(&fixture.paths.admission_lock, true, Duration::ZERO).unwrap();
    let original = fs::read(&fixture.config.destination.inbox_path).unwrap();
    let mut backend = fixture.backend();
    let started = Instant::now();
    let _holder = fixture.start_holder();
    super::tests::deferred(
        deliver(
            &mut backend,
            &fixture.config,
            &fixture.lease(),
            vec![fixture.entry.clone()],
        ),
        DeferralReason::UnknownOutcome,
    );
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert!(fixture.root().join("holder-done").exists());
    assert!(backend.phases.iter().any(|phase| phase == "running"));
    assert_eq!(backend.phases.last().unwrap(), "failed");
    assert_eq!(
        backend
            .actions
            .iter()
            .filter(|action| *action == "deliver")
            .count(),
        1
    );
    assert!(backend.actions.iter().any(|action| action == "state"));
    assert!(!backend.actions.iter().any(|action| action == "finish"));
    let status = fixture.control("status");
    assert_eq!(status["busy"], false);
    assert_eq!(status["readonly"], false);
    assert_eq!(status["modifiable"], true);
    assert_eq!(
        fs::read(&fixture.config.destination.inbox_path).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(fixture.root().join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let observations = fixture.observations();
    let probes: Vec<_> = observations
        .iter()
        .filter(|value| value["action"] == "lease-status")
        .collect();
    assert!(probes.iter().any(|value| value["writerGroupDead"] == false));
    assert_eq!(probes.last().unwrap()["writerGroupDead"], true);
    for probe in probes {
        assert_eq!(probe["busy"], true);
        assert_eq!(probe["readonly"], true);
        assert_eq!(probe["modifiable"], false);
    }
    assert_eq!(observations[0]["writeBlocked"], true);
}
