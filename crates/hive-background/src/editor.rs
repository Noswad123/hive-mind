//! Cooperative editor admission and bounded, credential-free Neovim RPC.
//!
//! The coordinator MUST retain admission SH through this entire call, but MUST
//! NOT hold delivery EX: the editor-owned helper acquires delivery EX and its own
//! admission SH across the local write and exit. The NoEditor direct helper owns
//! those locks too. The outer worker lock serializes coordinator cycles.
//! A transport timeout is not permission to release an editor's guard: the
//! editor owns its helper and guard until its independently bounded finish.
use crate::state::{FileLock, Paths};
use crate::{DeferralReason, DeliveryLease, EditorEntry, ProcessInfo, WorkerConfig, now_ms};
use anyhow::{Context, Result, ensure};
use hive_capture::{DeliveryReport, DestinationBinding};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

const MAX_RPC: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 256;
// editor-context is synchronous and allows its helper 5s; leave 2s for native
// metadata and cleanup, without ever extending the caller's cycle deadline.
const DISCOVERY_BUDGET: Duration = Duration::from_secs(7);
const RPC_LUA: &str = "return require('util.hive_background').rpc(...)";

#[derive(Debug)]
pub enum EditorDecision {
    NoEditor,
    Deferred(DeferralReason),
    Completed(DeliveryReport),
}

/// Sanitized pre-source admission result, without editor paths or buffer data.
#[derive(Debug, PartialEq, Eq)]
pub enum EditorReadiness {
    NoEditor,
    Ready,
    Deferred(DeferralReason),
}

fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

fn valid_session(session: &str) -> bool {
    session.len() == 32
        && session
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn entry_path(paths: &Paths, pid: u32, session: &str) -> PathBuf {
    paths.registry.join(format!("{pid}-{session}.json"))
}

fn private_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == uid() && meta.mode() & 0o077 == 0,
        "editor directory is not private"
    );
    ensure!(
        path.is_absolute() && fs::canonicalize(path)? == path,
        "editor directory is not physical"
    );
    // The immediate directory is private. Sticky system ancestors (e.g. /tmp)
    // cannot replace it; non-sticky writable ancestors cannot be trusted.
    for ancestor in path.ancestors().skip(1) {
        let meta = fs::symlink_metadata(ancestor)?;
        ensure!(
            meta.is_dir() && (meta.uid() == 0 || meta.uid() == uid()),
            "untrusted editor directory ancestor"
        );
        ensure!(
            meta.mode() & 0o022 == 0 || (meta.uid() == 0 && meta.mode() & 0o1000 != 0),
            "writable editor directory ancestor"
        );
    }
    Ok(())
}

fn validate_socket(socket: &Path) -> Result<()> {
    private_directory(socket.parent().context("socket has no parent")?)?;
    let meta = fs::symlink_metadata(socket)?;
    ensure!(
        meta.file_type().is_socket() && meta.uid() == uid(),
        "untrusted editor socket"
    );
    Ok(())
}

fn read_entry(path: &Path) -> Result<EditorEntry> {
    let entry: EditorEntry = crate::state::read_json(path)?;
    ensure!(
        entry.schema_version == "1"
            && valid_session(&entry.session_id)
            && entry.pid > 0
            && !entry.process_start.is_empty(),
        "invalid editor entry"
    );
    ensure!(
        path.file_name()
            == Some(std::ffi::OsStr::new(&format!(
                "{}-{}.json",
                entry.pid, entry.session_id
            ))),
        "editor entry name mismatch"
    );
    Ok(entry)
}

/// Registration is performed before opening any note buffers. Only public
/// configuration is returned, including the validated, credential-free source
/// environment. Connection identifiers and provider list IDs stay local.
pub fn register(
    paths: &Paths,
    config: &WorkerConfig,
    pid: u32,
    socket: &Path,
    session: &str,
) -> Result<Value> {
    ensure!(valid_session(session) && pid > 0, "invalid editor identity");
    paths.ensure()?;
    let _admission = FileLock::acquire(&paths.admission_lock, false, Duration::from_secs(5))?;
    private_directory(&paths.root)?;
    private_directory(&paths.registry)?;
    let public = public_config(config)?;
    let process = crate::process::find_process(pid)?.context("editor process unavailable")?;
    ensure!(
        process.uid == uid()
            && fs::canonicalize(&process.executable)? == fs::canonicalize(&config.nvim_binary)?,
        "editor executable mismatch"
    );
    validate_socket(socket)?;
    let entry = EditorEntry {
        schema_version: "1".into(),
        session_id: session.into(),
        pid,
        process_start: process.start_identity,
        socket_path: socket.into(),
    };
    let path = entry_path(paths, pid, session);
    // Never overwrite an unknown or mismatching record.
    if path.try_exists()? {
        ensure!(read_entry(&path)? == entry, "editor entry already exists");
    } else {
        crate::state::write_bytes(&path, &serde_json::to_vec(&entry)?, None)?;
    }
    Ok(
        json!({"schemaVersion":"1", "registered":true, "entry":entry,
        "config": public, "configDigest": config_digest(config)?}),
    )
}

/// Busy admission deliberately leaves the record behind. Stale records are not
/// deleted by discovery, and uncertain records are never unlinked here.
pub fn unregister(paths: &Paths, pid: u32, session: &str) -> Result<()> {
    unregister_with(&mut Native, paths, pid, session)
}

fn unregister_with(
    backend: &mut impl Backend,
    paths: &Paths,
    pid: u32,
    session: &str,
) -> Result<()> {
    if !valid_session(session) || pid == 0 {
        return Ok(());
    }
    let Ok(_admission) = FileLock::acquire(&paths.admission_lock, false, Duration::ZERO) else {
        return Ok(());
    };
    private_directory(&paths.root)?;
    private_directory(&paths.registry)?;
    let path = entry_path(paths, pid, session);
    let Ok(bytes) = crate::state::read_bytes(&path) else {
        return Ok(());
    };
    let Ok(entry) = read_entry(&path) else {
        return Ok(());
    };
    let removable = match backend.find_process(pid)? {
        // Detached VimLeave cleanup commonly runs after its editor has exited.
        // Only native definite absence permits deletion without start proof.
        None => true,
        Some(process) => process.uid == uid() && process.start_identity == entry.process_start,
    };
    if entry.pid == pid && entry.session_id == session && removable {
        crate::state::remove_bytes(&path, &bytes)?;
    }
    Ok(())
}

fn public_config(config: &WorkerConfig) -> Result<Value> {
    let env: std::collections::BTreeMap<String, String> =
        crate::config::source_environment(config)?
            .into_iter()
            .map(|(key, value)| {
                Ok((
                    key.into_string()
                        .map_err(|_| anyhow::anyhow!("source environment key is not UTF-8"))?,
                    value
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("source environment value is not UTF-8"))?,
                ))
            })
            .collect::<Result<_>>()?;
    Ok(
        json!({"schemaVersion":"1", "accountNamespace":config.account_namespace,
        "queuePath":config.queue_path, "mwBinary":config.mw_binary,
        "hiveBinary":config.hive_binary, "nvimBinary":config.nvim_binary,
        "workingDirectory":config.working_directory, "destination":config.destination,
        "cycleTimeoutSeconds":config.cycle_timeout_seconds, "deliveryLimit":config.delivery_limit,
        "env":env}),
    )
}

fn config_digest(config: &WorkerConfig) -> Result<String> {
    crate::config::digest(config)
}

/// Test seam: no tests need to launch, discover, or contact user editors.
trait Backend {
    fn inventory(&mut self) -> Result<Vec<ProcessInfo>>;
    fn find_process(&mut self, pid: u32) -> Result<Option<ProcessInfo>>;
    fn rpc(&mut self, entry: &EditorEntry, request: Value, timeout: Duration) -> Result<Value>;
}

struct Native;
impl Backend for Native {
    fn inventory(&mut self) -> Result<Vec<ProcessInfo>> {
        crate::process::inventory()
    }
    fn find_process(&mut self, pid: u32) -> Result<Option<ProcessInfo>> {
        crate::process::find_process(pid)
    }

    fn rpc(&mut self, entry: &EditorEntry, request: Value, timeout: Duration) -> Result<Value> {
        let until = Instant::now() + timeout;
        let process = crate::process::find_process(entry.pid)?.context("editor disappeared")?;
        ensure!(
            process.uid == uid() && process.start_identity == entry.process_start,
            "editor identity changed"
        );
        validate_socket(&entry.socket_path)?;
        let stream = connect(&entry.socket_path, until)?;
        verify_peer(&stream, entry.pid)?;
        let mut stream = DeadlineStream {
            stream: stream.take(MAX_RPC),
            until,
        };
        let args = rmpv::ext::to_value(vec![request])?;
        let message = rmpv::Value::Array(vec![
            0.into(),
            1.into(),
            "nvim_exec_lua".into(),
            rmpv::Value::Array(vec![RPC_LUA.into(), args]),
        ]);
        rmpv::encode::write_value(&mut stream, &message)?;
        stream.flush()?;
        // Notifications are permitted, but share the same absolute deadline and
        // byte budget. Neither a notification flood nor trickle can extend it.
        loop {
            let value = rmpv::decode::read_value_with_max_depth(&mut stream, 32)?;
            let fields = value.as_array().context("invalid RPC response")?;
            if fields.first().and_then(rmpv::Value::as_u64) == Some(2) {
                continue;
            }
            ensure!(
                fields.len() == 4
                    && fields[0].as_u64() == Some(1)
                    && fields[1].as_u64() == Some(1)
                    && fields[2].is_nil(),
                "RPC failed"
            );
            return Ok(rmpv::ext::from_value(fields[3].clone())?);
        }
    }
}

// Enforce an absolute deadline on *each* decoder read, not just on the first
// byte of a MessagePack object. Socket timeouts alone allow slow trickle attacks.
struct DeadlineStream {
    stream: std::io::Take<UnixStream>,
    until: Instant,
}

impl DeadlineStream {
    fn remaining(&self) -> std::io::Result<Duration> {
        self.until
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "editor RPC deadline"))
    }

    fn wait(&self, events: libc::c_short) -> std::io::Result<()> {
        loop {
            let left = self.remaining()?;
            let mut poll = libc::pollfd {
                fd: self.stream.get_ref().as_raw_fd(),
                events,
                revents: 0,
            };
            // SAFETY: poll points to one valid descriptor for this call.
            let result = unsafe {
                libc::poll(
                    &mut poll,
                    1,
                    left.as_millis().clamp(1, i32::MAX as u128) as i32,
                )
            };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "editor RPC deadline",
                ));
            }
            if poll.revents & libc::POLLNVAL != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid editor socket",
                ));
            }
            return Ok(());
        }
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            self.wait(libc::POLLIN)?;
            match self.stream.read(buf) {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            self.wait(libc::POLLOUT)?;
            match self.stream.get_mut().write(buf) {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.get_mut().flush()
    }
}

fn connect(path: &Path, until: Instant) -> Result<UnixStream> {
    use std::os::{fd::FromRawFd, unix::ffi::OsStrExt};
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: zero initialization is valid for sockaddr_un and its padding.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    ensure!(
        !bytes.contains(&0) && bytes.len() < address.sun_path.len(),
        "invalid editor socket path"
    );
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = length as u8;
    }
    // SAFETY: socket returns a fresh descriptor, transferred exactly once below.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    ensure!(fd >= 0, "could not create editor socket");
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
        "could not secure editor socket"
    );
    #[cfg(target_os = "macos")]
    {
        let enabled: libc::c_int = 1;
        // SAFETY: enabled references an initialized integer option value.
        ensure!(
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_NOSIGPIPE,
                    (&enabled as *const libc::c_int).cast(),
                    std::mem::size_of_val(&enabled) as libc::socklen_t,
                )
            } == 0,
            "could not secure editor socket writes"
        );
    }
    stream.set_nonblocking(true)?;
    // SAFETY: address and length identify initialized sockaddr_un storage.
    let result =
        unsafe { libc::connect(fd, (&address as *const libc::sockaddr_un).cast(), length) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        ensure!(
            matches!(
                error.raw_os_error(),
                Some(libc::EINPROGRESS) | Some(libc::EAGAIN)
            ),
            "editor connection failed"
        );
        loop {
            let left = until
                .checked_duration_since(Instant::now())
                .context("editor connection deadline")?;
            ensure!(!left.is_zero(), "editor connection deadline");
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: poll points to a single valid pollfd.
            let result = unsafe {
                libc::poll(
                    &mut poll,
                    1,
                    left.as_millis().clamp(1, i32::MAX as u128) as i32,
                )
            };
            if result < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            ensure!(
                result > 0 && poll.revents & libc::POLLOUT != 0,
                "editor connection deadline"
            );
            ensure!(stream.take_error()?.is_none(), "editor connection failed");
            break;
        }
    }
    Ok(stream)
}

fn verify_peer(stream: &UnixStream, expected_pid: u32) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
        // SAFETY: pointers reference initialized, appropriately sized storage.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        ensure!(
            result == 0
                && len as usize == std::mem::size_of_val(&cred)
                && cred.uid == uid()
                && cred.pid > 0
                && cred.pid as u32 == expected_pid,
            "editor peer mismatch"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let (mut peer_uid, mut peer_gid) = (0, 0);
        // SAFETY: output pointers are valid for the duration of the call.
        ensure!(
            unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer_uid, &mut peer_gid) } == 0
                && peer_uid == uid(),
            "editor peer mismatch"
        );
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
        // Darwin LOCAL_PEERPID is 2 at SOL_LOCAL (0).
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                0,
                2,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut len,
            )
        };
        ensure!(
            result == 0
                && len as usize == std::mem::size_of_val(&pid)
                && pid > 0
                && pid as u32 == expected_pid,
            "editor peer mismatch"
        );
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("editor peer verification unsupported");
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Description {
    schema_version: String,
    session_id: String,
    pid: u32,
    parent_pid: u32,
    tui_parent_pid: Option<u32>,
    context: DestinationBinding,
    busy: bool,
    dirty: bool,
    mode: String,
}

fn request(action: &str, entry: &EditorEntry, lease: &DeliveryLease) -> Value {
    json!({"action":action, "schemaVersion":"1", "sessionId":entry.session_id,
        "cycleId":lease.cycle_id, "configDigest":lease.config_digest, "deadlineMs":lease.deadline_ms})
}

fn rpc_timeout(deadline: u64) -> Result<Duration> {
    let remaining = deadline.saturating_sub(now_ms());
    ensure!(remaining > 0, "editor deadline expired");
    Ok(Duration::from_millis(remaining))
}

fn read_registry(paths: &Paths) -> Result<Vec<EditorEntry>> {
    private_directory(&paths.root)?;
    private_directory(&paths.registry)?;
    let mut entries = Vec::new();
    for item in fs::read_dir(&paths.registry)? {
        ensure!(entries.len() < MAX_ENTRIES, "too many editor entries");
        entries.push(read_entry(&item?.path())?);
    }
    Ok(entries)
}

/// Errors crossing this boundary are intentionally reduced to deferral reasons:
/// raw RPC failures may contain private paths, buffers, or provider text.
pub fn deliver_via_editor(
    paths: &Paths,
    config: &WorkerConfig,
    lease: &DeliveryLease,
) -> Result<EditorDecision> {
    let entries = match read_registry(paths) {
        Ok(entries) => entries,
        Err(_) => return Ok(EditorDecision::Deferred(DeferralReason::EditorUnavailable)),
    };
    Ok(deliver(&mut Native, config, lease, entries))
}

/// Check readiness before source collection. The caller retains admission SH.
/// This sends describe only: no source/delivery helper or persistent lease is
/// created, and no write is authorized by a Ready result. Delivery must recheck.
pub fn check_readiness(paths: &Paths, config: &WorkerConfig) -> Result<EditorReadiness> {
    let entries = match read_registry(paths) {
        Ok(entries) => entries,
        Err(_) => return Ok(EditorReadiness::Deferred(DeferralReason::EditorUnavailable)),
    };
    Ok(readiness(&mut Native, config, entries))
}

fn readiness(
    backend: &mut impl Backend,
    config: &WorkerConfig,
    entries: Vec<EditorEntry>,
) -> EditorReadiness {
    let (Ok(cycle_id), Ok(config_digest)) = (crate::nonce(), config_digest(config)) else {
        return EditorReadiness::Deferred(DeferralReason::EditorUnavailable);
    };
    // In-memory request identity only. Coordinator/helper fields are not sent by
    // describe; there is deliberately no lease file or local writer invocation.
    // The 10s horizon also covers inventory around the 7s context budget.
    let request_context = DeliveryLease {
        schema_version: "1".into(),
        cycle_id,
        config_digest,
        coordinator_pid: 0,
        coordinator_start: String::new(),
        deadline_ms: now_ms() + 10_000,
        destination: config.destination.clone(),
        editor_session: None,
        helper_pid: None,
        helper_pgid: None,
    };
    match discover(backend, config, &request_context, entries) {
        Ok(None) => EditorReadiness::NoEditor,
        Ok(Some(_)) => EditorReadiness::Ready,
        Err(reason) => EditorReadiness::Deferred(reason),
    }
}

fn deliver(
    backend: &mut impl Backend,
    config: &WorkerConfig,
    lease: &DeliveryLease,
    entries: Vec<EditorEntry>,
) -> EditorDecision {
    match discover(backend, config, lease, entries).and_then(|entry| match entry {
        Some(entry) => deliver_to_editor(backend, lease, &entry),
        None => Ok(EditorDecision::NoEditor),
    }) {
        Ok(decision) => decision,
        Err(reason) => EditorDecision::Deferred(reason),
    }
}

fn discover(
    backend: &mut impl Backend,
    config: &WorkerConfig,
    lease: &DeliveryLease,
    entries: Vec<EditorEntry>,
) -> std::result::Result<Option<EditorEntry>, DeferralReason> {
    use DeferralReason::*;
    if lease.schema_version != "1"
        || lease.destination != config.destination
        || config_digest(config).ok().as_deref() != Some(&lease.config_digest)
    {
        return Err(EditorContextMismatch);
    }
    let executable = fs::canonicalize(&config.nvim_binary).map_err(|_| EditorUnavailable)?;
    let processes = backend.inventory().map_err(|_| EditorUnavailable)?;
    let mut live = Vec::new();
    let mut covered = std::collections::BTreeSet::new();
    for entry in entries {
        let Some(process) = processes.iter().find(|p| p.pid == entry.pid) else {
            // An inventory omission is not proof of death (UIDs can change and
            // process lists race). Only a direct native ESRCH is stale.
            if backend
                .find_process(entry.pid)
                .map_err(|_| EditorUnavailable)?
                .is_some()
            {
                return Err(EditorUnavailable);
            }
            continue;
        };
        if process.uid != uid()
            || process.start_identity != entry.process_start
            || fs::canonicalize(&process.executable).ok().as_ref() != Some(&executable)
            || entry.schema_version != "1"
            || !valid_session(&entry.session_id)
        {
            return Err(EditorUnavailable);
        }
        if !covered.insert(entry.pid) {
            return Err(EditorUnavailable);
        }
        let resolving_until = Instant::now() + DISCOVERY_BUDGET;
        let description = loop {
            let timeout = rpc_timeout(lease.deadline_ms)
                .map_err(|_| CycleBudget)?
                .min(
                    resolving_until
                        .checked_duration_since(Instant::now())
                        .filter(|remaining| !remaining.is_zero())
                        .ok_or(EditorUnavailable)?,
                );
            let value = backend
                .rpc(&entry, request("describe", &entry, lease), timeout)
                .map_err(|_| EditorUnavailable)?;
            if value.get("schemaVersion").and_then(Value::as_str) != Some("1")
                || value.get("sessionId").and_then(Value::as_str) != Some(&entry.session_id)
            {
                return Err(EditorUnavailable);
            }
            match value.get("phase").and_then(Value::as_str) {
                Some("resolving") if Instant::now() < resolving_until => {
                    thread::sleep(Duration::from_millis(20))
                }
                Some("ready") => {
                    break serde_json::from_value::<Description>(value)
                        .map_err(|_| EditorUnavailable)?;
                }
                Some("deferred") => {
                    return Err(serde_json::from_value(
                        value.get("reason").cloned().unwrap_or(Value::Null),
                    )
                    .unwrap_or(EditorUnavailable));
                }
                _ => return Err(EditorUnavailable),
            }
        };
        if description.schema_version != "1"
            || description.session_id != entry.session_id
            || description.pid != entry.pid
            || description.parent_pid != process.parent_pid
        {
            return Err(EditorUnavailable);
        }
        if let Some(parent_pid) = description.tui_parent_pid {
            let parent = processes
                .iter()
                .find(|p| p.pid == parent_pid)
                .ok_or(EditorUnavailable)?;
            if parent_pid == entry.pid
                || parent_pid != process.parent_pid
                || parent.uid != uid()
                || fs::canonicalize(&parent.executable).ok().as_ref() != Some(&executable)
            {
                return Err(EditorUnavailable);
            }
            covered.insert(parent_pid);
        }
        live.push((entry, description));
    }
    for process in &processes {
        if process.uid != uid() {
            continue;
        }
        let canonical = fs::canonicalize(&process.executable);
        let looks_like_nvim = process.executable.file_name() == Some(std::ffi::OsStr::new("nvim"));
        if (looks_like_nvim || canonical.as_ref().ok() == Some(&executable))
            && !covered.contains(&process.pid)
        {
            return Err(UnregisteredEditor);
        }
    }
    if live.is_empty() {
        return Ok(None);
    }
    if live.len() != 1 {
        return Err(MultipleEditors);
    }
    let (entry, description) = live.pop().unwrap();
    if lease
        .editor_session
        .as_ref()
        .is_some_and(|session| session != &entry.session_id)
    {
        return Err(EditorUnavailable);
    }
    if description.context != config.destination {
        return Err(EditorContextMismatch);
    }
    if description.dirty {
        return Err(DirtyBuffers);
    }
    if description.busy {
        return Err(EditorBusy);
    }
    if description.mode != "n" {
        return Err(UiActive);
    }
    // Recheck OS identity before granting readiness or requesting a write.
    // Neovim's deliver action revalidates context, digest, dirty/busy and guard.
    let current = backend.inventory().map_err(|_| EditorUnavailable)?;
    let relevant = |items: &[ProcessInfo]| {
        let mut items: Vec<_> = items
            .iter()
            .filter(|p| {
                p.uid == uid()
                    && (covered.contains(&p.pid)
                        || p.executable.file_name() == Some(std::ffi::OsStr::new("nvim"))
                        || fs::canonicalize(&p.executable).ok().as_ref() == Some(&executable))
            })
            .cloned()
            .collect();
        items.sort_by_key(|p| p.pid);
        items
    };
    if relevant(&current) != relevant(&processes) {
        return Err(EditorUnavailable);
    }
    Ok(Some(entry))
}

fn deliver_to_editor(
    backend: &mut impl Backend,
    lease: &DeliveryLease,
    entry: &EditorEntry,
) -> std::result::Result<EditorDecision, DeferralReason> {
    use DeferralReason::*;
    let timeout = rpc_timeout(lease.deadline_ms).map_err(|_| CycleBudget)?;
    let mut value = backend
        .rpc(entry, request("deliver", entry, lease), timeout)
        .map_err(|_| UnknownOutcome)?;
    loop {
        if value.get("schemaVersion").and_then(Value::as_str) != Some("1")
            || value.get("sessionId").and_then(Value::as_str) != Some(&entry.session_id)
            || value.get("cycleId").and_then(Value::as_str) != Some(&lease.cycle_id)
        {
            return Err(UnknownOutcome);
        }
        match value.get("phase").and_then(Value::as_str) {
            Some("done") => {
                if value.get("writerExited").and_then(Value::as_bool) != Some(true)
                    || value.get("conflicts").and_then(Value::as_u64) != Some(0)
                    || value
                        .get("exitCode")
                        .is_some_and(|code| code.as_i64() != Some(0))
                {
                    return Err(UnknownOutcome);
                }
                let report =
                    serde_json::from_value(value.get("report").cloned().ok_or(UnknownOutcome)?)
                        .map_err(|_| UnknownOutcome)?;
                return Ok(EditorDecision::Completed(report));
            }
            Some("deferred") => {
                // This phase is the protocol's prelaunch refusal, not an
                // uncertain writer outcome. Never surface arbitrary reason text.
                return Err(serde_json::from_value(
                    value.get("reason").cloned().ok_or(UnknownOutcome)?,
                )
                .unwrap_or(UnknownOutcome));
            }
            Some("preparing" | "running") => {}
            _ => return Err(UnknownOutcome),
        }
        thread::sleep(Duration::from_millis(20));
        let timeout = rpc_timeout(lease.deadline_ms).map_err(|_| UnknownOutcome)?;
        value = backend
            .rpc(entry, request("state", entry, lease), timeout)
            .map_err(|_| UnknownOutcome)?;
    }
}

#[cfg(all(test, target_os = "macos"))]
mod native_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{BTreeMap, VecDeque},
        os::unix::fs::PermissionsExt,
    };

    pub(super) fn private_fixture() -> tempfile::TempDir {
        let temp = crate::state::tests::fixture();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        temp
    }

    struct Mock {
        processes: Vec<ProcessInfo>,
        replies: VecDeque<Result<Value>>,
        actions: Vec<String>,
        requests: Vec<Value>,
        find_calls: Vec<u32>,
        lookup_error: bool,
        timeouts: Vec<Duration>,
        simulated_latency: VecDeque<Duration>,
        changed: Option<Vec<ProcessInfo>>,
    }

    impl Backend for Mock {
        fn inventory(&mut self) -> Result<Vec<ProcessInfo>> {
            if !self.actions.is_empty()
                && let Some(changed) = &self.changed
            {
                return Ok(changed.clone());
            }
            Ok(self.processes.clone())
        }

        fn find_process(&mut self, pid: u32) -> Result<Option<ProcessInfo>> {
            self.find_calls.push(pid);
            ensure!(!self.lookup_error, "injected uncertain process lookup");
            Ok(self.processes.iter().find(|p| p.pid == pid).cloned())
        }

        fn rpc(&mut self, entry: &EditorEntry, payload: Value, timeout: Duration) -> Result<Value> {
            assert_eq!(payload["schemaVersion"], "1");
            assert_eq!(payload["sessionId"], entry.session_id);
            assert!(payload.get("connection").is_none());
            self.actions
                .push(payload["action"].as_str().unwrap().into());
            self.requests.push(payload);
            self.timeouts.push(timeout);
            if self.simulated_latency.pop_front().unwrap_or_default() > timeout {
                anyhow::bail!("simulated RPC timeout");
            }
            self.replies.pop_front().expect("unexpected RPC")
        }
    }

    struct Fixture {
        config: WorkerConfig,
        lease: DeliveryLease,
        entry: EditorEntry,
        process: ProcessInfo,
    }

    impl Fixture {
        fn new() -> Self {
            let destination = DestinationBinding {
                notes_dir: "/test/notes".into(),
                inbox_path: "/test/notes/inbox.md".into(),
                dashboard_path: "/test/notes/dashboard.md".into(),
            };
            let config = WorkerConfig {
                schema_version: "1".into(),
                connection: "private-connection".into(),
                account_namespace: "0123456789abcdef".repeat(4),
                queue_path: "/test/queue".into(),
                list_id: "private-list".into(),
                mw_binary: "/test/mw".into(),
                hive_binary: "/test/hive".into(),
                nvim_binary: std::env::current_exe().unwrap(),
                working_directory: "/test".into(),
                destination: destination.clone(),
                mw_environment: BTreeMap::from([("SECRET".into(), "do-not-return".into())]),
                interval_seconds: 60,
                cycle_timeout_seconds: 30,
                delivery_limit: 10,
            };
            let process = ProcessInfo {
                pid: 100,
                parent_pid: 99,
                uid: uid(),
                process_group: 99,
                start_identity: "start-100".into(),
                executable: config.nvim_binary.clone(),
            };
            let entry = EditorEntry {
                schema_version: "1".into(),
                session_id: "a".repeat(32),
                pid: process.pid,
                process_start: process.start_identity.clone(),
                socket_path: "/not-a-real-editor".into(),
            };
            let lease = DeliveryLease {
                schema_version: "1".into(),
                cycle_id: "b".repeat(32),
                config_digest: config_digest(&config).unwrap(),
                coordinator_pid: std::process::id(),
                coordinator_start: "test-start".into(),
                deadline_ms: now_ms() + 30_000,
                destination,
                editor_session: None,
                helper_pid: None,
                helper_pgid: None,
            };
            Self {
                config,
                lease,
                entry,
                process,
            }
        }

        fn ready(&self) -> Value {
            json!({"schemaVersion":"1", "phase":"ready", "sessionId":self.entry.session_id,
                "pid":self.process.pid, "parentPid":self.process.parent_pid,
                "context":self.config.destination, "busy":false, "dirty":false, "mode":"n"})
        }

        fn done(&self) -> Value {
            json!({"schemaVersion":"1", "phase":"done", "sessionId":self.entry.session_id,
                "cycleId":self.lease.cycle_id, "writerExited":true, "conflicts":0,
                "report":DeliveryReport { delivered: 1, ..Default::default() }})
        }

        fn mock(&self, replies: Vec<Value>) -> Mock {
            Mock {
                processes: vec![self.process.clone()],
                replies: replies.into_iter().map(Ok).collect(),
                actions: Vec::new(),
                requests: Vec::new(),
                find_calls: Vec::new(),
                lookup_error: false,
                timeouts: Vec::new(),
                simulated_latency: VecDeque::new(),
                changed: None,
            }
        }

        fn run(&self, mock: &mut Mock) -> EditorDecision {
            deliver(mock, &self.config, &self.lease, vec![self.entry.clone()])
        }
    }

    pub(super) fn deferred(decision: EditorDecision, reason: DeferralReason) {
        match decision {
            EditorDecision::Deferred(actual) => assert_eq!(actual, reason),
            other => panic!("expected deferral, got {other:?}"),
        }
    }

    #[test]
    fn readiness_is_describe_only_with_bounded_fresh_request_identity() {
        let f = Fixture::new();
        let resolving =
            json!({"schemaVersion":"1", "sessionId":f.entry.session_id, "phase":"resolving"});
        let mut mock = f.mock(vec![resolving, f.ready()]);
        let before = now_ms();
        assert_eq!(
            readiness(&mut mock, &f.config, vec![f.entry.clone()]),
            EditorReadiness::Ready
        );
        assert_eq!(mock.actions, ["describe", "describe"]);
        assert_eq!(mock.requests[0], mock.requests[1]);
        let request = &mock.requests[0];
        assert!(valid_session(request["cycleId"].as_str().unwrap()));
        assert_eq!(request["configDigest"], config_digest(&f.config).unwrap());
        assert_eq!(request["sessionId"], f.entry.session_id);
        assert!(request["deadlineMs"].as_u64().unwrap() > before);
        assert!(request["deadlineMs"].as_u64().unwrap() <= now_ms() + 10_000);
        assert_eq!(request.as_object().unwrap().len(), 6);
        assert!(
            mock.timeouts
                .iter()
                .all(|timeout| *timeout <= DISCOVERY_BUDGET)
        );
        let mut next = f.mock(vec![f.ready()]);
        assert_eq!(
            readiness(&mut next, &f.config, vec![f.entry.clone()]),
            EditorReadiness::Ready
        );
        assert_ne!(request["cycleId"], next.requests[0]["cycleId"]);
    }

    #[test]
    fn readiness_preserves_quiet_context_and_protocol_checks() {
        let f = Fixture::new();
        for (field, value, reason) in [
            ("dirty", json!(true), DeferralReason::DirtyBuffers),
            ("busy", json!(true), DeferralReason::EditorBusy),
            ("mode", json!("i"), DeferralReason::UiActive),
            (
                "context",
                json!({"notesDir":"/elsewhere", "inboxPath":"/elsewhere/i", "dashboardPath":"/elsewhere/d"}),
                DeferralReason::EditorContextMismatch,
            ),
            (
                "schemaVersion",
                json!("2"),
                DeferralReason::EditorUnavailable,
            ),
            (
                "sessionId",
                json!("wrong-session"),
                DeferralReason::EditorUnavailable,
            ),
            ("phase", json!("unknown"), DeferralReason::EditorUnavailable),
        ] {
            let mut response = f.ready();
            response[field] = value;
            let mut mock = f.mock(vec![response]);
            assert_eq!(
                readiness(&mut mock, &f.config, vec![f.entry.clone()]),
                EditorReadiness::Deferred(reason)
            );
            assert_eq!(mock.actions, ["describe"]);
        }
        let response = json!({"schemaVersion":"1", "sessionId":f.entry.session_id,
            "phase":"deferred", "reason":"editor_context_mismatch"});
        let mut mock = f.mock(vec![response]);
        assert_eq!(
            readiness(&mut mock, &f.config, vec![f.entry.clone()]),
            EditorReadiness::Deferred(DeferralReason::EditorContextMismatch)
        );
        assert_eq!(mock.actions, ["describe"]);
        let mut mock = f.mock(vec![]);
        mock.replies
            .push_back(Err(anyhow::anyhow!("private RPC failure")));
        assert_eq!(
            readiness(&mut mock, &f.config, vec![f.entry.clone()]),
            EditorReadiness::Deferred(DeferralReason::EditorUnavailable)
        );
        assert_eq!(mock.actions, ["describe"]);
    }

    #[test]
    fn slow_synchronous_context_fits_discovery_budget_without_writer_calls() {
        let f = Fixture::new();
        let mut mock = f.mock(vec![f.ready()]);
        mock.simulated_latency
            .push_back(Duration::from_millis(2500));
        assert_eq!(
            readiness(&mut mock, &f.config, vec![f.entry.clone()]),
            EditorReadiness::Ready
        );
        assert_eq!(mock.actions, ["describe"]);
        assert!(mock.timeouts[0] > Duration::from_secs(5));
        assert!(mock.timeouts[0] <= DISCOVERY_BUDGET);
    }

    #[test]
    fn overbound_context_or_short_cycle_defers_without_delivery_or_retry() {
        for (cycle_ms, context_ms) in [(35_000, 7500), (1500, 2500)] {
            let mut f = Fixture::new();
            f.lease.deadline_ms = now_ms() + cycle_ms;
            let mut mock = f.mock(vec![f.ready()]);
            mock.simulated_latency
                .push_back(Duration::from_millis(context_ms));
            deferred(f.run(&mut mock), DeferralReason::EditorUnavailable);
            assert_eq!(mock.actions, ["describe"]);
            assert!(mock.timeouts[0] <= DISCOVERY_BUDGET.min(Duration::from_millis(cycle_ms)));
        }
        let f = Fixture::new();
        let mut mock = f.mock(vec![f.ready()]);
        mock.simulated_latency
            .push_back(Duration::from_millis(7500));
        assert_eq!(
            readiness(&mut mock, &f.config, vec![f.entry.clone()]),
            EditorReadiness::Deferred(DeferralReason::EditorUnavailable)
        );
        assert_eq!(mock.actions, ["describe"]);
    }

    #[test]
    fn readiness_preserves_process_coverage_and_start_checks() {
        let f = Fixture::new();
        let mut mock = f.mock(vec![]);
        assert_eq!(
            readiness(&mut mock, &f.config, vec![]),
            EditorReadiness::Deferred(DeferralReason::UnregisteredEditor)
        );
        assert!(mock.actions.is_empty());
        mock.processes.clear();
        assert_eq!(
            readiness(&mut mock, &f.config, vec![]),
            EditorReadiness::NoEditor
        );
        assert!(mock.actions.is_empty());
        let mut mock = f.mock(vec![]);
        mock.processes[0].start_identity = "reused".into();
        assert_eq!(
            readiness(&mut mock, &f.config, vec![f.entry.clone()]),
            EditorReadiness::Deferred(DeferralReason::EditorUnavailable)
        );
        assert!(mock.actions.is_empty());
        let mut mock = f.mock(vec![f.ready()]);
        mock.changed = Some(vec![]);
        assert_eq!(
            readiness(&mut mock, &f.config, vec![f.entry.clone()]),
            EditorReadiness::Deferred(DeferralReason::EditorUnavailable)
        );
        assert_eq!(mock.actions, ["describe"]);
    }

    #[test]
    fn unregister_removes_exact_dead_record_but_keeps_reused_or_uncertain_pid() {
        let f = Fixture::new();
        let temp = private_fixture();
        let paths = Paths::in_root(temp.path().join("state"));
        paths.ensure().unwrap();
        let path = entry_path(&paths, f.entry.pid, &f.entry.session_id);
        for variant in 0..5 {
            crate::state::write_json(&path, &f.entry).unwrap();
            let original = fs::read(&path).unwrap();
            let mut mock = f.mock(vec![]);
            match variant {
                0 => mock.processes.clear(),
                1 => {} // The original live identity can unregister too.
                2 => mock.processes[0].start_identity = "reused".into(),
                3 => mock.processes[0].uid = uid().wrapping_add(1),
                _ => mock.lookup_error = true,
            }
            let result = unregister_with(&mut mock, &paths, f.entry.pid, &f.entry.session_id);
            if variant == 4 {
                assert!(result.is_err());
            } else {
                result.unwrap();
            }
            if variant < 2 {
                assert!(!path.exists());
            } else {
                assert_eq!(fs::read(&path).unwrap(), original);
            }
            assert_eq!(mock.find_calls, [f.entry.pid]);
            assert!(mock.actions.is_empty());
        }
    }

    #[test]
    fn unregister_leaves_busy_unknown_and_mismatching_records_untouched() {
        let f = Fixture::new();
        let temp = private_fixture();
        let paths = Paths::in_root(temp.path().join("state"));
        paths.ensure().unwrap();
        let path = entry_path(&paths, f.entry.pid, &f.entry.session_id);
        crate::state::write_json(&path, &f.entry).unwrap();
        let original = fs::read(&path).unwrap();
        let mut mock = f.mock(vec![]);
        mock.processes.clear();
        let admission = FileLock::acquire(&paths.admission_lock, true, Duration::ZERO).unwrap();
        unregister_with(&mut mock, &paths, f.entry.pid, &f.entry.session_id).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(mock.find_calls.is_empty());
        drop(admission);
        unregister_with(&mut mock, &paths, f.entry.pid, &"e".repeat(32)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        let unknown = paths.registry.join("unknown.json");
        fs::rename(&path, &unknown).unwrap();
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        unregister_with(&mut mock, &paths, f.entry.pid, &f.entry.session_id).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        assert_eq!(fs::read(&unknown).unwrap(), original);
        assert!(mock.find_calls.is_empty());
    }

    #[test]
    fn unregister_rechecks_exact_bytes_after_process_probe() {
        struct Replaced {
            path: PathBuf,
            entry: EditorEntry,
        }
        impl Backend for Replaced {
            fn inventory(&mut self) -> Result<Vec<ProcessInfo>> {
                panic!("unexpected inventory");
            }
            fn rpc(&mut self, _: &EditorEntry, _: Value, _: Duration) -> Result<Value> {
                panic!("unexpected RPC");
            }
            fn find_process(&mut self, _: u32) -> Result<Option<ProcessInfo>> {
                crate::state::write_json(&self.path, &self.entry)?;
                Ok(None)
            }
        }
        let f = Fixture::new();
        let temp = private_fixture();
        let paths = Paths::in_root(temp.path().join("state"));
        paths.ensure().unwrap();
        let path = entry_path(&paths, f.entry.pid, &f.entry.session_id);
        crate::state::write_json(&path, &f.entry).unwrap();
        let mut replacement = f.entry.clone();
        replacement.process_start = "changed-during-probe".into();
        let mut backend = Replaced {
            path: path.clone(),
            entry: replacement.clone(),
        };
        assert!(unregister_with(&mut backend, &paths, f.entry.pid, &f.entry.session_id).is_err());
        assert_eq!(read_entry(&path).unwrap(), replacement);
    }

    #[test]
    fn no_editor_requires_empty_inventory() {
        let f = Fixture::new();
        let mut mock = f.mock(vec![]);
        deferred(
            deliver(&mut mock, &f.config, &f.lease, vec![]),
            DeferralReason::UnregisteredEditor,
        );
        mock.processes.clear();
        assert!(matches!(
            deliver(&mut mock, &f.config, &f.lease, vec![]),
            EditorDecision::NoEditor
        ));
        assert!(mock.actions.is_empty());
    }

    #[test]
    fn quiet_checks_never_request_delivery() {
        let f = Fixture::new();
        for (field, value, reason) in [
            ("dirty", json!(true), DeferralReason::DirtyBuffers),
            ("busy", json!(true), DeferralReason::EditorBusy),
            ("mode", json!("i"), DeferralReason::UiActive),
            (
                "context",
                json!({"notesDir":"/elsewhere", "inboxPath":"/elsewhere/i", "dashboardPath":"/elsewhere/d"}),
                DeferralReason::EditorContextMismatch,
            ),
        ] {
            let mut ready = f.ready();
            ready[field] = value;
            let mut mock = f.mock(vec![ready]);
            deferred(f.run(&mut mock), reason);
            assert_eq!(mock.actions, ["describe"]);
        }
    }

    #[test]
    fn unsupported_protocol_and_wrong_nonce_fail_closed() {
        let f = Fixture::new();
        for (field, value) in [
            ("schemaVersion", json!("2")),
            ("sessionId", json!("c".repeat(32))),
            ("pid", json!(101)),
            ("parentPid", json!(98)),
            ("phase", json!("unknown")),
        ] {
            let mut ready = f.ready();
            ready[field] = value;
            let mut mock = f.mock(vec![ready]);
            deferred(f.run(&mut mock), DeferralReason::EditorUnavailable);
            assert_eq!(mock.actions, ["describe"]);
        }
    }

    #[test]
    fn stale_socket_defers_without_delivery() {
        let f = Fixture::new();
        let mut mock = f.mock(vec![]);
        mock.replies
            .push_back(Err(anyhow::anyhow!("private socket failure")));
        deferred(f.run(&mut mock), DeferralReason::EditorUnavailable);
        assert_eq!(mock.actions, ["describe"]);
    }

    #[test]
    fn pid_reuse_is_not_a_dead_entry() {
        let f = Fixture::new();
        let mut mock = f.mock(vec![]);
        mock.processes[0].start_identity = "replacement".into();
        deferred(f.run(&mut mock), DeferralReason::EditorUnavailable);
        assert!(mock.actions.is_empty());
    }

    #[test]
    fn definitely_absent_process_is_ignored() {
        let f = Fixture::new();
        let mut mock = f.mock(vec![]);
        mock.processes.clear();
        assert!(matches!(f.run(&mut mock), EditorDecision::NoEditor));
    }

    #[test]
    fn standard_tui_parent_is_covered_only_after_verified_handshake() {
        let f = Fixture::new();
        let mut parent = f.process.clone();
        parent.pid = f.process.parent_pid;
        parent.parent_pid = 98;
        parent.start_identity = "parent-start".into();
        let mut ready = f.ready();
        ready["tuiParentPid"] = json!(parent.pid);
        let mut mock = f.mock(vec![ready, f.done()]);
        mock.processes.push(parent.clone());
        assert!(matches!(f.run(&mut mock), EditorDecision::Completed(_)));
        let mut mock = f.mock(vec![f.ready()]);
        mock.processes.push(parent);
        deferred(f.run(&mut mock), DeferralReason::UnregisteredEditor);
    }

    #[test]
    fn tui_parent_claim_must_match_os_parent_uid_and_executable() {
        let f = Fixture::new();
        for variant in 0..3 {
            let mut parent = f.process.clone();
            parent.pid = 99;
            let mut ready = f.ready();
            ready["tuiParentPid"] = json!(99);
            match variant {
                0 => ready["tuiParentPid"] = json!(98),
                1 => parent.uid = uid().wrapping_add(1),
                _ => parent.executable = "/nonexistent/frontend".into(),
            }
            let mut mock = f.mock(vec![ready]);
            mock.processes.push(parent);
            deferred(f.run(&mut mock), DeferralReason::EditorUnavailable);
        }
    }

    #[test]
    fn two_registered_backends_defer() {
        let f = Fixture::new();
        let mut second = f.entry.clone();
        second.pid = 101;
        second.session_id = "c".repeat(32);
        let mut process = f.process.clone();
        process.pid = second.pid;
        let mut ready = f.ready();
        ready["pid"] = json!(second.pid);
        ready["sessionId"] = json!(second.session_id);
        let mut mock = f.mock(vec![f.ready(), ready]);
        mock.processes.push(process);
        deferred(
            deliver(
                &mut mock,
                &f.config,
                &f.lease,
                vec![f.entry.clone(), second],
            ),
            DeferralReason::MultipleEditors,
        );
        assert_eq!(mock.actions, ["describe", "describe"]);
    }

    #[test]
    fn alternate_nvim_executable_is_still_unregistered() {
        let f = Fixture::new();
        let mut mock = f.mock(vec![]);
        mock.processes[0].executable = "/other/install/nvim".into();
        deferred(
            deliver(&mut mock, &f.config, &f.lease, vec![]),
            DeferralReason::UnregisteredEditor,
        );
    }

    #[test]
    fn identity_is_rechecked_before_delivery() {
        let f = Fixture::new();
        let mut mock = f.mock(vec![f.ready()]);
        let mut changed = f.process.clone();
        changed.start_identity = "reused-after-describe".into();
        mock.changed = Some(vec![changed]);
        deferred(f.run(&mut mock), DeferralReason::EditorUnavailable);
        assert_eq!(mock.actions, ["describe"]);
    }

    #[test]
    fn client_timeout_never_retries_delivery_or_calls_finish() {
        let f = Fixture::new();
        let mut mock = f.mock(vec![f.ready()]);
        mock.replies
            .push_back(Err(anyhow::anyhow!("timeout with private task text")));
        deferred(f.run(&mut mock), DeferralReason::UnknownOutcome);
        assert_eq!(mock.actions, ["describe", "deliver"]);
    }

    #[test]
    fn completion_requires_writer_exit_and_reconciliation() {
        let f = Fixture::new();
        for (field, value) in [
            ("writerExited", json!(false)),
            ("conflicts", json!(1)),
            ("conflicts", Value::Null),
            ("exitCode", json!(1)),
            ("cycleId", json!("wrong")),
            ("sessionId", json!("wrong")),
            ("report", json!({})),
            ("phase", json!("completed")),
        ] {
            let mut done = f.done();
            done[field] = value;
            let mut mock = f.mock(vec![f.ready(), done]);
            deferred(f.run(&mut mock), DeferralReason::UnknownOutcome);
            assert_eq!(mock.actions, ["describe", "deliver"]);
        }
    }

    #[test]
    fn state_poll_confirms_completion_without_second_delivery() {
        let f = Fixture::new();
        let preparing = json!({"schemaVersion":"1", "sessionId":f.entry.session_id,
            "cycleId":f.lease.cycle_id, "phase":"preparing"});
        let mut mock = f.mock(vec![f.ready(), preparing, f.done()]);
        match f.run(&mut mock) {
            EditorDecision::Completed(report) => assert_eq!(report.delivered, 1),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(mock.actions, ["describe", "deliver", "state"]);
    }

    #[test]
    fn synchronous_writer_gets_remaining_cycle_budget_for_deliver_and_state() {
        let mut f = Fixture::new();
        f.lease.deadline_ms = now_ms() + 35_000;
        let mut running = f.done();
        running["phase"] = json!("running");
        let mut mock = f.mock(vec![f.ready(), running, f.done()]);
        mock.simulated_latency = VecDeque::from([
            Duration::ZERO,
            Duration::from_millis(2500),
            Duration::from_millis(2500),
        ]);
        assert!(matches!(f.run(&mut mock), EditorDecision::Completed(_)));
        assert_eq!(mock.actions, ["describe", "deliver", "state"]);
        assert!(mock.timeouts[0] <= DISCOVERY_BUDGET);
        for timeout in &mock.timeouts[1..] {
            assert!(*timeout > Duration::from_millis(2500));
            assert!(*timeout <= Duration::from_secs(35));
        }
        assert!(mock.timeouts[2] <= mock.timeouts[1]);
    }

    #[test]
    fn prelaunch_deferral_maps_only_known_enum_reasons() {
        let f = Fixture::new();
        for reason in [
            DeferralReason::DirtyBuffers,
            DeferralReason::EditorBusy,
            DeferralReason::UiActive,
            DeferralReason::EditorContextMismatch,
            DeferralReason::SourceLocked,
            DeferralReason::CycleBudget,
        ] {
            let response = json!({"schemaVersion":"1", "sessionId":f.entry.session_id,
                "cycleId":f.lease.cycle_id, "phase":"deferred", "reason":reason});
            let mut mock = f.mock(vec![f.ready(), response]);
            deferred(f.run(&mut mock), reason);
            assert_eq!(mock.actions, ["describe", "deliver"]);
        }
        for reason in [
            json!("private arbitrary reason"),
            Value::Null,
            json!({"reason":"dirty_buffers"}),
        ] {
            let response = json!({"schemaVersion":"1", "sessionId":f.entry.session_id,
                "cycleId":f.lease.cycle_id, "phase":"deferred", "reason":reason});
            let mut mock = f.mock(vec![f.ready(), response]);
            deferred(f.run(&mut mock), DeferralReason::UnknownOutcome);
            assert_eq!(mock.actions, ["describe", "deliver"]);
        }
    }

    #[test]
    fn state_timeout_never_releases_guard_or_retries_delivery() {
        let f = Fixture::new();
        let preparing = json!({"schemaVersion":"1", "sessionId":f.entry.session_id,
            "cycleId":f.lease.cycle_id, "phase":"preparing"});
        let mut mock = f.mock(vec![f.ready(), preparing]);
        mock.replies
            .push_back(Err(anyhow::anyhow!("private state timeout")));
        deferred(f.run(&mut mock), DeferralReason::UnknownOutcome);
        assert_eq!(mock.actions, ["describe", "deliver", "state"]);
    }

    #[test]
    fn expired_budget_and_mismatching_lease_never_start_delivery() {
        let mut f = Fixture::new();
        f.lease.deadline_ms = 0;
        let mut mock = f.mock(vec![]);
        deferred(f.run(&mut mock), DeferralReason::CycleBudget);
        assert!(mock.actions.is_empty());
        f.lease.config_digest = "wrong".into();
        deferred(f.run(&mut mock), DeferralReason::EditorContextMismatch);
        assert!(mock.actions.is_empty());
    }

    #[test]
    fn resolving_is_bounded_and_may_become_ready() {
        let f = Fixture::new();
        let resolving =
            json!({"schemaVersion":"1", "sessionId":f.entry.session_id, "phase":"resolving"});
        let mut mock = f.mock(vec![resolving, f.ready(), f.done()]);
        assert!(matches!(f.run(&mut mock), EditorDecision::Completed(_)));
        assert_eq!(mock.actions, ["describe", "describe", "deliver"]);
    }

    #[test]
    fn public_configuration_has_exact_source_shape_and_only_allowed_environment() {
        let (temp, mut config) = crate::config::tests::fixture();
        config.connection = "private-connection".into();
        config.list_id = "private-list".into();
        for key in [
            "MW_CONFIG",
            "NOTES_DIR",
            "INBOX_PATH",
            "MW_INBOX_PATH",
            "DASHBOARD_PATH",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
        ] {
            config
                .mw_environment
                .insert(key.into(), temp.path().join(key).to_str().unwrap().into());
        }
        let expected_env: BTreeMap<String, String> = crate::config::source_environment(&config)
            .unwrap()
            .into_iter()
            .map(|(key, value)| (key.into_string().unwrap(), value.into_string().unwrap()))
            .collect();
        let public = public_config(&config).unwrap();
        assert_eq!(
            public,
            json!({"schemaVersion":"1", "accountNamespace":config.account_namespace,
            "queuePath":config.queue_path, "mwBinary":config.mw_binary,
            "hiveBinary":config.hive_binary, "nvimBinary":config.nvim_binary,
            "workingDirectory":config.working_directory, "destination":config.destination,
            "cycleTimeoutSeconds":config.cycle_timeout_seconds, "deliveryLimit":config.delivery_limit,
            "env":expected_env})
        );
        assert_eq!(
            public["env"]["HOME"],
            hive_google_auth::storage::system_user_home()
                .unwrap()
                .to_str()
                .unwrap()
        );
        assert_eq!(public["env"]["PATH"], "/usr/bin:/bin:/opt/homebrew/bin");
        for (key, value) in &config.mw_environment {
            assert_eq!(public["env"][key], *value);
        }
        let text = public.to_string();
        for private in [
            "private-connection",
            "private-list",
            "SECRET",
            "do-not-return",
            "mwEnvironment",
        ] {
            assert!(!text.contains(private));
        }
        assert!(text.contains("accountNamespace"));
    }

    #[test]
    fn public_configuration_rejects_credentials_and_source_environment_overrides() {
        let (_temp, mut config) = crate::config::tests::fixture();
        for key in ["SECRET", "GOOGLE_ACCESS_TOKEN", "HOME", "PATH"] {
            config
                .mw_environment
                .insert(key.into(), "/test/not-exported".into());
            assert!(public_config(&config).is_err());
            config.mw_environment.clear();
        }
    }

    #[test]
    fn entry_reader_rejects_malformed_permissive_symlink_and_name_mismatch() {
        let f = Fixture::new();
        let temp = private_fixture();
        let path = temp
            .path()
            .join(format!("{}-{}.json", f.entry.pid, f.entry.session_id));
        fs::write(&path, serde_json::to_vec(&f.entry).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_entry(&path).unwrap(), f.entry);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_entry(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let wrong = temp.path().join("unknown.json");
        fs::rename(&path, &wrong).unwrap();
        assert!(read_entry(&wrong).is_err());
        std::os::unix::fs::symlink(&wrong, &path).unwrap();
        assert!(read_entry(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"not JSON").unwrap();
        assert!(read_entry(&path).is_err());
    }

    #[test]
    fn peer_verification_checks_native_test_process_without_editor() {
        let (client, _server) = UnixStream::pair().unwrap();
        verify_peer(&client, std::process::id()).unwrap();
        assert!(verify_peer(&client, std::process::id().wrapping_add(1)).is_err());
    }

    #[test]
    fn socket_validation_requires_physical_private_parent_and_real_socket() {
        use std::os::unix::net::UnixListener;
        let temp = private_fixture();
        let socket = temp.path().join("rpc.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        validate_socket(&socket).unwrap();
        let link = temp.path().join("link.sock");
        std::os::unix::fs::symlink(&socket, &link).unwrap();
        assert!(validate_socket(&link).is_err());
        let file = temp.path().join("regular.sock");
        fs::write(&file, b"").unwrap();
        assert!(validate_socket(&file).is_err());
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_socket(&socket).is_err());
    }

    #[test]
    fn registry_bounds_and_unknown_files_fail_closed() {
        let temp = private_fixture();
        let paths = Paths::in_root(temp.path().join("state"));
        paths.ensure().unwrap();
        assert!(read_registry(&paths).unwrap().is_empty());
        let unknown = paths.registry.join("unknown.json");
        fs::write(&unknown, b"{}").unwrap();
        fs::set_permissions(&unknown, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_registry(&paths).is_err());
        let f = Fixture::new();
        deferred(
            deliver_via_editor(&paths, &f.config, &f.lease).unwrap(),
            DeferralReason::EditorUnavailable,
        );
        assert_eq!(
            check_readiness(&paths, &f.config).unwrap(),
            EditorReadiness::Deferred(DeferralReason::EditorUnavailable)
        );
        fs::remove_file(unknown).unwrap();
        let path = entry_path(&paths, f.entry.pid, &f.entry.session_id);
        fs::write(&path, vec![b' '; crate::state::MAX_BYTES + 1]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_registry(&paths).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn registration_and_best_effort_unregister_only_touch_proven_record() {
        use std::os::unix::net::UnixListener;
        let temp = private_fixture();
        let paths = Paths::in_root(temp.path().join("state"));
        let socket = temp.path().join("rpc.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let (_config_temp, mut config) = crate::config::tests::fixture();
        config.nvim_binary = std::env::current_exe().unwrap();
        let pid = std::process::id();
        let session = "d".repeat(32);
        let registered = register(&paths, &config, pid, &socket, &session).unwrap();
        assert_eq!(registered["registered"], true);
        assert_eq!(registered["entry"]["socketPath"], socket.to_str().unwrap());
        assert_eq!(registered["config"], public_config(&config).unwrap());
        assert_eq!(
            registered["configDigest"],
            crate::config::digest(&config).unwrap()
        );
        assert!(
            registered["configDigest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        let path = entry_path(&paths, pid, &session);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            register(&paths, &config, pid, &socket, &session).unwrap(),
            registered
        );
        let admission = FileLock::acquire(&paths.admission_lock, true, Duration::ZERO).unwrap();
        unregister(&paths, pid, &session).unwrap();
        assert!(path.exists());
        drop(admission);
        unregister(&paths, pid, &"e".repeat(32)).unwrap();
        assert!(path.exists());
        let mut entry = read_entry(&path).unwrap();
        entry.process_start = "unproven-start".into();
        crate::state::write_json(&path, &entry).unwrap();
        unregister(&paths, pid, &session).unwrap();
        assert!(path.exists());
        assert!(register(&paths, &config, pid, &socket, &session).is_err());
        entry.process_start = crate::process::find_process(pid)
            .unwrap()
            .unwrap()
            .start_identity;
        crate::state::write_json(&path, &entry).unwrap();
        unregister(&paths, pid, &session).unwrap();
        assert!(!path.exists());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn native_rpc_uses_fixed_lua_and_one_structured_argument() {
        use std::os::unix::net::UnixListener;
        let temp = private_fixture();
        let socket = temp.path().join("rpc.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut f = Fixture::new();
        let process = crate::process::find_process(std::process::id())
            .unwrap()
            .unwrap();
        f.entry.pid = process.pid;
        f.entry.process_start = process.start_identity;
        f.entry.socket_path = socket;
        f.lease.cycle_id = "literal '); os.execute('never') -- text".into();
        let expected = request("describe", &f.entry, &f.lease);
        let server_expected = expected.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let value = rmpv::decode::read_value(&mut stream).unwrap();
            let fields = value.as_array().unwrap();
            assert_eq!(fields[0].as_u64(), Some(0));
            assert_eq!(fields[2].as_str(), Some("nvim_exec_lua"));
            let args = fields[3].as_array().unwrap();
            assert_eq!(args[0].as_str(), Some(RPC_LUA));
            let payload: Vec<Value> = rmpv::ext::from_value(args[1].clone()).unwrap();
            assert_eq!(payload, [server_expected]);
            let notification =
                rmpv::Value::Array(vec![2.into(), "test".into(), rmpv::Value::Array(vec![])]);
            rmpv::encode::write_value(&mut stream, &notification).unwrap();
            let response = rmpv::Value::Array(vec![
                1.into(),
                fields[1].clone(),
                rmpv::Value::Nil,
                rmpv::ext::to_value(json!({"test":true})).unwrap(),
            ]);
            rmpv::encode::write_value(&mut stream, &response).unwrap();
        });
        let reply = Native
            .rpc(&f.entry, expected, Duration::from_secs(2))
            .unwrap();
        assert_eq!(reply, json!({"test":true}));
        server.join().unwrap();
    }

    #[test]
    fn rpc_deadline_is_absolute_even_if_bytes_remain_available() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"bytes already available").unwrap();
        let mut stream = DeadlineStream {
            stream: client.take(MAX_RPC),
            until: Instant::now(),
        };
        assert_eq!(
            stream.read(&mut [0; 1]).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert_eq!(
            stream.write(b"not sent").unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }
}
