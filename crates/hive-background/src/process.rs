//! Metadata-only process discovery and cooperative, process-group supervision.
//!
//! Commands must be trusted programs: descendants must not escape their process
//! group. File descriptors passed to the supervisor must be owned cooperative
//! lock descriptors, kept open by the caller until this function returns.

use crate::ProcessInfo;
use anyhow::{Result, bail};
use std::{
    io::{self, Read},
    os::unix::{
        ffi::OsStringExt,
        io::{AsRawFd, RawFd},
        process::CommandExt,
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 1024 * 1024;
const TICK: Duration = Duration::from_millis(20);
const GRACE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessReason {
    Exited,
    Timeout,
    Cancelled,
    OutputLimit,
}

#[derive(Debug)]
pub struct ProcessResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub reason: ProcessReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    pid: u32,
    parent: u32,
    uid: u32,
    group: u32,
    start: String,
    zombie: bool,
}

fn valid_pid(pid: u32) -> Result<i32> {
    if pid <= 1 || pid > i32::MAX as u32 {
        bail!("invalid process identifier");
    }
    Ok(pid as i32)
}

fn definitely_gone(pid: u32) -> Result<bool> {
    let pid = valid_pid(pid)?;
    // Signal zero probes existence only; EPERM is explicitly not absence.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Ok(false);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Ok(true),
        Some(libc::EPERM) => Ok(false),
        _ => bail!("could not determine process liveness"),
    }
}

#[cfg(target_os = "macos")]
fn identity(pid: u32) -> Result<Option<Identity>> {
    let raw_pid = valid_pid(pid)?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let read = unsafe {
        libc::proc_pidinfo(
            raw_pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if read != size as i32 {
        if definitely_gone(pid)? {
            return Ok(None);
        }
        bail!("process metadata unavailable");
    }
    let info = unsafe { info.assume_init() };
    Ok(Some(Identity {
        pid: info.pbi_pid,
        parent: info.pbi_ppid,
        uid: info.pbi_uid,
        group: info.pbi_pgid,
        start: format!("{}.{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        zombie: info.pbi_status == 5, // Darwin SZOMB; zombies cannot write.
    }))
}

#[cfg(not(target_os = "macos"))]
fn identity(_: u32) -> Result<Option<Identity>> {
    bail!("native process metadata requires macOS")
}

/// Resolve native executable and microsecond start identity, never argv or titles.
/// Only a definite ESRCH is absence; inaccessible or racing metadata is an error.
pub fn find_process(pid: u32) -> Result<Option<ProcessInfo>> {
    let Some(before) = identity(pid)? else {
        return Ok(None);
    };
    #[cfg(target_os = "macos")]
    let executable = {
        let mut bytes = vec![0u8; 4096];
        let count = unsafe {
            libc::proc_pidpath(pid as i32, bytes.as_mut_ptr().cast(), bytes.len() as u32)
        };
        if count <= 0 || count as usize >= bytes.len() {
            if definitely_gone(pid)? {
                return Ok(None);
            }
            bail!("process executable unavailable");
        }
        let length = bytes
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| anyhow::anyhow!("invalid process executable"))?;
        bytes.truncate(length);
        std::fs::canonicalize(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
            .map_err(|_| anyhow::anyhow!("process executable cannot be resolved"))?
    };
    #[cfg(not(target_os = "macos"))]
    let executable = PathBuf::new();
    let Some(after) = identity(pid)? else {
        return Ok(None);
    };
    if before != after {
        bail!("process identity changed during inspection");
    }
    Ok(Some(ProcessInfo {
        pid,
        parent_pid: after.parent,
        uid: after.uid,
        process_group: after.group,
        start_identity: after.start,
        executable,
    }))
}

fn inventory_command() -> Command {
    let mut command = Command::new("/bin/ps");
    command
        .args(["-axo", "pid=,ppid=,uid=,pgid=,lstart="])
        .env_clear()
        .env("LC_ALL", "C");
    command
}

fn parse_ps_uid(value: &str) -> Result<u32> {
    let uid = value
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("invalid process inventory identifier"))?;
    if !(i64::from(i32::MIN)..=i64::from(u32::MAX)).contains(&uid) {
        bail!("invalid process inventory identifier");
    }
    // Darwin ps may display a 32-bit C uid_t as signed (nobody: -2).
    // Preserve those bits only after checking the signed/unsigned 32-bit range.
    Ok(uid as u32)
}

#[cfg(target_os = "macos")]
fn inventory_zombie(pid: u32, uid: u32) -> bool {
    unsafe extern "C" {
        fn hive_inventory_zombie(pid: libc::pid_t, expected_uid: libc::uid_t) -> libc::c_int;
    }
    let Ok(pid) = valid_pid(pid) else {
        return false;
    };
    // This snapshot only permits inventory omission, not signals or lock release.
    unsafe { hive_inventory_zombie(pid, uid) == 1 }
}

#[cfg(not(target_os = "macos"))]
fn inventory_zombie(_: u32, _: u32) -> bool {
    false
}

/// All same-effective-UID processes, not just editors. Callers identify Neovim
/// by the canonical executable, not its spoofable process name or command line.
pub fn inventory() -> Result<Vec<ProcessInfo>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let result = run_supervised(&mut inventory_command(), Duration::from_millis(1400), &[])?;
    if result.reason != ProcessReason::Exited || result.exit_code != Some(0) {
        bail!("process inventory unavailable");
    }
    let text = std::str::from_utf8(&result.stdout)
        .map_err(|_| anyhow::anyhow!("invalid process inventory"))?;
    let uid = unsafe { libc::geteuid() };
    inventory_from_text(text, uid, deadline, find_process, inventory_zombie)
}

fn inventory_from_text(
    text: &str,
    uid: u32,
    deadline: Instant,
    mut lookup: impl FnMut(u32) -> Result<Option<ProcessInfo>>,
    mut zombie: impl FnMut(u32, u32) -> bool,
) -> Result<Vec<ProcessInfo>> {
    let mut processes = Vec::new();
    for line in text.lines() {
        if Instant::now() >= deadline {
            bail!("process inventory deadline exceeded");
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 9 {
            bail!("invalid process inventory metadata");
        }
        let pid = fields[0]
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("invalid process inventory identifier"))?;
        let listed_uid = parse_ps_uid(fields[2])?;
        if pid <= 1 || listed_uid != uid {
            continue;
        }
        let found = match lookup(pid) {
            Ok(found) => found,
            Err(error) => {
                if zombie(pid, uid) {
                    continue;
                }
                return Err(error);
            }
        };
        if let Some(process) = found
            && process.uid == uid
        {
            processes.push(process);
        }
    }
    Ok(processes)
}

#[cfg(target_os = "macos")]
fn group_members(pgid: u32, exited_child: Option<u32>) -> Result<Vec<Identity>> {
    valid_pid(pgid)?;
    // Fixed allocation bounds even on a hostile or rapidly changing process list.
    let mut pids = vec![0i32; OUTPUT_LIMIT / std::mem::size_of::<i32>()];
    // PROC_PGRP_ONLY selects in the kernel, without inspecting other sessions.
    let bytes =
        unsafe { libc::proc_listpids(2, pgid, pids.as_mut_ptr().cast(), OUTPUT_LIMIT as i32) };
    if bytes < 0 || bytes as usize >= OUTPUT_LIMIT {
        bail!("process membership unavailable");
    }
    if bytes == 0 {
        if unsafe { libc::kill(-(pgid as i32), 0) } == -1
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return Ok(Vec::new());
        }
        bail!("process membership unavailable");
    }
    pids.truncate(bytes as usize / std::mem::size_of::<i32>());
    let mut members = Vec::new();
    for pid in pids {
        if pid <= 1 || exited_child == Some(pid as u32) {
            continue;
        }
        if let Some(member) = identity(pid as u32)?
            && member.group == pgid
        {
            members.push(member);
        }
    }
    Ok(members)
}

#[cfg(not(target_os = "macos"))]
fn group_members(_: u32, _: Option<u32>) -> Result<Vec<Identity>> {
    bail!("native process membership requires macOS")
}

/// Liveness only, NOT authorization to signal a group. All visible members must
/// have this effective UID; callers must additionally validate start identities.
pub fn group_alive(pgid: u32) -> Result<bool> {
    let pgid_raw = valid_pid(pgid)?;
    if unsafe { libc::kill(-pgid_raw, 0) } != 0 {
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => return Ok(false),
            _ => bail!("process group liveness unavailable"),
        }
    }
    let members = group_members(pgid, None)?;
    if members.is_empty() {
        bail!("process group membership changed");
    }
    if members.iter().any(|m| m.uid != unsafe { libc::geteuid() }) {
        bail!("process group has a different owner");
    }
    Ok(members.iter().any(|m| !m.zombie))
}

struct Signals(Vec<signal_hook::SigId>);
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

fn nonblocking(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        bail!("could not configure child output");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn child_exited(pid: u32) -> Result<bool> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        bail!("could not inspect supervised child");
    }
    Ok(unsafe { info.assume_init() }.si_pid != 0)
}

#[cfg(not(target_os = "macos"))]
fn child_exited(_: u32) -> Result<bool> {
    bail!("native child supervision requires macOS")
}

// Nonblocking readers avoid detached draining threads or a join hanging on an
// inherited pipe. Continue discarding after the combined retention limit.
fn drain(reader: &mut impl Read, output: &mut Vec<u8>, remaining: &mut usize) -> Result<bool> {
    let mut bytes = [0u8; 8192];
    for _ in 0..32 {
        match reader.read(&mut bytes) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                let keep = n.min(*remaining);
                output.extend_from_slice(&bytes[..keep]);
                *remaining -= keep;
                if keep != n {
                    return Ok(true);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => bail!("could not read child output"),
        }
    }
    Ok(false)
}

/// Run a trusted program in its own group, retaining only bounded output. Errors
/// are static and never include argv, environment, or child output. Cancellation
/// handlers are unregistered on return. No shell is introduced here.
pub fn run_supervised(
    command: &mut Command,
    timeout: Duration,
    inherited_fds: &[RawFd],
) -> Result<ProcessResult> {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut signals = Signals(Vec::new());
    for signal in [libc::SIGTERM, libc::SIGINT] {
        signals.0.push(
            signal_hook::flag::register(signal, cancel.clone())
                .map_err(|_| anyhow::anyhow!("could not register cancellation handler"))?,
        );
    }
    supervise(command, timeout, inherited_fds, cancel)
}

fn supervise(
    command: &mut Command,
    timeout: Duration,
    inherited_fds: &[RawFd],
    cancel: Arc<AtomicBool>,
) -> Result<ProcessResult> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| anyhow::anyhow!("invalid process timeout"))?;
    let fds = inherited_fds.to_vec();
    for fd in &fds {
        if *fd < 3 || unsafe { libc::fcntl(*fd, libc::F_GETFD) } < 0 {
            bail!("invalid cooperative lock descriptor");
        }
    }
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Only async-signal-safe syscalls, with all allocation done before fork.
    unsafe {
        command.pre_exec(move || {
            for fd in &fds {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("could not spawn supervised process"))?;
    let pgid = child.id();
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut result = ProcessResult {
        stdout: Vec::new(),
        stderr: Vec::new(),
        exit_code: None,
        timed_out: false,
        cancelled: false,
        reason: ProcessReason::Exited,
    };
    let mut remaining = OUTPUT_LIMIT;
    let mut failure = false;
    let mut stopping = None;
    let mut exited = false;
    if nonblocking(stdout.as_raw_fd()).is_err() || nonblocking(stderr.as_raw_fd()).is_err() {
        failure = true;
    }
    loop {
        let mut overflow = false;
        if !failure {
            for (reader, output) in [
                (&mut stdout as &mut dyn Read, &mut result.stdout),
                (&mut stderr as &mut dyn Read, &mut result.stderr),
            ] {
                // Adapt the trait object without allocating a reader thread.
                let mut reader = reader;
                match drain(&mut reader, output, &mut remaining) {
                    Ok(exceeded) => overflow |= exceeded,
                    Err(_) => failure = true,
                }
            }
        }
        if !exited {
            match child_exited(pgid) {
                Ok(value) => exited = value,
                Err(_) => failure = true,
            }
        }
        // Darwin's proc_pidinfo does not expose unreaped zombies. WNOWAIT
        // already proves our direct zombie cannot write, so omit only that
        // exact pinned child, never an arbitrary inaccessible group member.
        let members = group_members(pgid, exited.then_some(pgid));
        let (live, authorized) = match &members {
            Ok(members) => {
                let owned = members.iter().all(|m| m.uid == unsafe { libc::geteuid() });
                // WNOWAIT keeps our direct child unreaped throughout cleanup.
                // Its PID therefore cannot be reused to create an unrelated
                // group with this number, even after the leader has exited.
                (members.iter().any(|m| !m.zombie), owned)
            }
            Err(_) => {
                // Exit can race metadata lookup (notably Darwin zombies).
                // Retain ownership and retry, never infer absence or signal
                // an unvalidated group from an inaccessible snapshot.
                (true, false)
            }
        };
        if cancel.load(Ordering::Relaxed) {
            result.cancelled = true;
            result.reason = ProcessReason::Cancelled;
        } else if Instant::now() >= deadline && !exited {
            result.timed_out = true;
            result.reason = ProcessReason::Timeout;
        } else if overflow && result.reason == ProcessReason::Exited {
            result.reason = ProcessReason::OutputLimit;
        }
        let stop = failure || result.reason != ProcessReason::Exited || exited;
        if stop && live {
            let began = *stopping.get_or_insert_with(Instant::now);
            if authorized {
                let signal = if began.elapsed() >= GRACE {
                    libc::SIGKILL
                } else {
                    libc::SIGTERM
                };
                unsafe {
                    libc::kill(-(pgid as i32), signal);
                }
            } else if !exited {
                // Unreaped direct child cannot be a reused PID. Never signal an
                // unvalidated numeric group on a metadata failure.
                let _ = child.kill();
            }
        }
        if exited && !live {
            break;
        }
        thread::sleep(TICK);
    }
    let status = child
        .wait()
        .map_err(|_| anyhow::anyhow!("could not reap supervised process"))?;
    result.exit_code = status.code();
    if !failure {
        // No live group member can hold the pipes open now. Drain to EOF.
        loop {
            let before = remaining;
            let exceeded = drain(&mut stdout, &mut result.stdout, &mut remaining)?
                | drain(&mut stderr, &mut result.stderr, &mut remaining)?;
            if exceeded && result.reason == ProcessReason::Exited {
                result.reason = ProcessReason::OutputLimit;
            }
            if before == remaining {
                break;
            }
        }
    }
    if failure {
        bail!("supervised process metadata or output unavailable");
    }
    Ok(result)
}

/// Independent child-side deadline/parent-death protection. Drop only after all
/// supervised writers have been reaped; dropping cancels protection and joins
/// the watcher. The caller must already be in a dedicated group (not a terminal
/// group), and its parent and all descendants must obey the same group contract.
pub struct Watchdog {
    cancelled: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn install_group_watchdog(
    deadline_ms: u64,
    expected_parent: u32,
    owner_pid: Option<u32>,
) -> Result<Watchdog> {
    let remaining = Duration::from_millis(deadline_ms.saturating_sub(crate::now_ms()));
    let deadline = Instant::now()
        .checked_add(remaining)
        .ok_or_else(|| anyhow::anyhow!("invalid watchdog deadline"))?;
    valid_pid(expected_parent)?;
    if let Some(owner) = owner_pid {
        valid_pid(owner)?;
    }
    let pgid = unsafe { libc::getpgrp() };
    if pgid <= 1 {
        bail!("watchdog requires a dedicated process group");
    }
    let me = identity(std::process::id())?
        .ok_or_else(|| anyhow::anyhow!("watchdog identity unavailable"))?;
    if me.group != pgid as u32 || !group_alive(pgid as u32)? {
        bail!("watchdog group cannot be validated");
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let thread = thread::Builder::new()
        .name("group-watchdog".into())
        .spawn(move || {
            while !flag.load(Ordering::Acquire) {
                let parent_changed = unsafe { libc::getppid() } as u32 != expected_parent;
                let owner_gone = owner_pid.is_some_and(|pid| definitely_gone(pid).unwrap_or(false));
                if Instant::now() >= deadline || parent_changed || owner_gone {
                    // This thread itself pins the group's identity. Unlike external
                    // PID-based supervision, group-number reuse cannot race here.
                    unsafe {
                        libc::kill(-pgid, libc::SIGKILL);
                    }
                    return;
                }
                thread::sleep(TICK);
            }
        })
        .map_err(|_| anyhow::anyhow!("could not start group watchdog"))?;
    Ok(Watchdog {
        cancelled,
        thread: Some(thread),
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};

    #[test]
    fn ps_uid_accepts_only_checked_32_bit_representations() {
        for (text, expected) in [
            ("503", 503),
            ("0", 0),
            ("4294967295", u32::MAX),
            ("-1", u32::MAX),
            ("-2", 4_294_967_294),
            ("-2147483648", 2_147_483_648),
        ] {
            assert_eq!(parse_ps_uid(text).unwrap(), expected);
        }
        for text in [
            "",
            "nobody",
            "1.0",
            "--2",
            " 503",
            "503 ",
            "4294967296",
            "-2147483649",
            "9223372036854775808",
            "-9223372036854775809",
        ] {
            assert!(parse_ps_uid(text).is_err(), "accepted invalid UID: {text}");
        }
    }

    #[test]
    fn inventory_normalizes_listed_uid_and_rechecks_native_uid() {
        let text = "41 1 -2 2 Mon Nov 1 00:00:00 2026\n42 1 503 2 Mon Nov 1 00:00:00 2026";
        for native_uid in [503, 504] {
            let processes = inventory_from_text(
                text,
                503,
                Instant::now() + Duration::from_secs(2),
                |pid| {
                    assert_eq!(pid, 42, "another UID must be filtered before lookup");
                    Ok(Some(ProcessInfo {
                        pid,
                        parent_pid: 1,
                        uid: native_uid,
                        process_group: 42,
                        start_identity: "fixture".into(),
                        executable: PathBuf::from("/fixture/editor"),
                    }))
                },
                |_, _| panic!("successful lookup must not query zombies"),
            )
            .unwrap();
            assert_eq!(processes.len(), usize::from(native_uid == 503));
        }
    }

    #[test]
    fn inventory_skips_only_proven_zombie_errors_and_keeps_live_candidates() {
        let me = ProcessInfo {
            pid: 42,
            parent_pid: 1,
            uid: 501,
            process_group: 42,
            start_identity: "fixture".into(),
            executable: PathBuf::from("/fixture/editor"),
        };
        let missing = me.pid + 1;
        let text = format!(
            "{missing} 1 {} 2 Mon Nov 1 00:00:00 2026\n{} 1 {} 2 Mon Nov 1 00:00:00 2026",
            me.uid, me.pid, me.uid
        );
        let processes = inventory_from_text(
            &text,
            me.uid,
            Instant::now() + Duration::from_secs(2),
            |pid| {
                if pid == missing {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied).into())
                } else {
                    Ok(Some(me.clone()))
                }
            },
            |pid, uid| {
                assert_eq!((pid, uid), (missing, me.uid));
                true
            },
        )
        .unwrap();
        assert_eq!(processes, [me]);
    }

    #[test]
    fn inventory_preserves_unproven_lookup_errors() {
        let uid = unsafe { libc::geteuid() };
        for zombie in [inventory_zombie as fn(u32, u32) -> bool, |_, _| false] {
            for pid in [std::process::id(), i32::MAX as u32 + 1] {
                let text = format!("{pid} 1 {uid} 2 Mon Nov 1 00:00:00 2026");
                let error = inventory_from_text(
                    &text,
                    uid,
                    Instant::now() + Duration::from_secs(2),
                    |_| Err(io::Error::from(io::ErrorKind::PermissionDenied).into()),
                    zombie,
                )
                .unwrap_err();
                assert_eq!(
                    error.downcast::<io::Error>().unwrap().kind(),
                    io::ErrorKind::PermissionDenied
                );
            }
        }
        for pid in [0, 1, u32::MAX] {
            assert!(!inventory_zombie(pid, uid));
        }
    }

    #[test]
    fn inventory_ok_and_malformed_input_do_not_query_zombies() {
        let uid = unsafe { libc::geteuid() };
        for text in [
            "malformed",
            "bad 1 0 2 Mon Nov 1 00:00:00 2026",
            "-2 1 503 2 Mon Nov 1 00:00:00 2026",
            "2 1 -2147483649 2 Mon Nov 1 00:00:00 2026",
            "2 1 4294967296 2 Mon Nov 1 00:00:00 2026",
        ] {
            assert!(
                inventory_from_text(
                    text,
                    uid,
                    Instant::now() + Duration::from_secs(2),
                    |_| panic!("malformed inventory must fail before lookup"),
                    |_, _| panic!("malformed inventory must not query zombies"),
                )
                .is_err()
            );
        }
        let text = format!("2 1 {uid} 2 Mon Nov 1 00:00:00 2026");
        assert!(
            inventory_from_text(
                &text,
                uid,
                Instant::now() + Duration::from_secs(2),
                |_| Ok(None),
                |_, _| panic!("successful lookup must not query zombies"),
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn native_zombie_query_proves_unreaped_child_only() {
        struct Reap(std::process::Child);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let child = Reap(Command::new("/usr/bin/true").spawn().unwrap());
        let pid = child.0.id();
        let uid = unsafe { libc::geteuid() };
        let deadline = Instant::now() + Duration::from_secs(2);
        // child_exited uses waitid(WNOWAIT), so the fixture remains unreaped.
        while !child_exited(pid).unwrap() {
            assert!(Instant::now() < deadline, "fixture did not exit");
            thread::sleep(TICK);
        }
        assert!(!definitely_gone(pid).unwrap());
        assert!(find_process(pid).is_err());
        assert!(inventory_zombie(pid, uid));
        assert!(!inventory_zombie(pid, uid.wrapping_add(1)));
        assert!(!inventory_zombie(std::process::id(), uid));
        let text = format!("{pid} 1 {uid} 2 Mon Nov 1 00:00:00 2026");
        assert!(
            inventory_from_text(&text, uid, deadline, find_process, inventory_zombie)
                .unwrap()
                .is_empty()
        );
    }

    fn fixture_command(mode: &str, directory: &std::path::Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "process::tests::fixture", "--nocapture"])
            .env("HIVE_PROCESS_FIXTURE", mode)
            .env("HIVE_PROCESS_FIXTURE_DIR", directory);
        command
    }

    // Only the explicitly selected child test uses these modes. No user
    // configuration, running editor, or existing service is ever touched.
    #[test]
    fn fixture() {
        let Ok(mode) = std::env::var("HIVE_PROCESS_FIXTURE") else {
            return;
        };
        let directory = PathBuf::from(std::env::var_os("HIVE_PROCESS_FIXTURE_DIR").unwrap());
        match mode.as_str() {
            "lock-holder" => {
                let lock = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(directory.join("lock"))
                    .unwrap();
                assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
                run_supervised(
                    &mut fixture_command("writer", &directory),
                    Duration::from_secs(5),
                    &[lock.as_raw_fd()],
                )
                .unwrap();
            }
            "writer" => {
                let parent = unsafe { libc::getppid() } as u32;
                // Give the outer test time to kill only its fixture supervisor
                // and verify that the inherited descriptor still holds the lock.
                let mut sleeper = Command::new("/bin/sleep").arg("5").spawn().unwrap();
                std::fs::write(
                    directory.join("ready"),
                    format!("{} {}", std::process::id(), sleeper.id()),
                )
                .unwrap();
                thread::sleep(Duration::from_millis(500));
                let _watchdog =
                    install_group_watchdog(crate::now_ms() + 2000, parent, None).unwrap();
                let _ = sleeper.wait();
            }
            "deadline" => {
                let _watchdog = install_group_watchdog(
                    crate::now_ms() + 100,
                    unsafe { libc::getppid() } as u32,
                    None,
                )
                .unwrap();
                thread::sleep(Duration::from_secs(5));
                panic!("deadline watchdog did not fire");
            }
            "drop" => {
                let watchdog = install_group_watchdog(
                    crate::now_ms() + 100,
                    unsafe { libc::getppid() } as u32,
                    None,
                )
                .unwrap();
                drop(watchdog);
                thread::sleep(Duration::from_millis(200));
            }
            _ => panic!("unknown fixture"),
        }
    }

    #[test]
    fn inherited_lock_survives_supervisor_death_until_watchdog_kills_writers() {
        let directory = tempfile::tempdir().unwrap();
        File::create(directory.path().join("lock")).unwrap();
        let mut holder = fixture_command("lock-holder", directory.path())
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        let ready = directory.path().join("ready");
        while !ready.exists() && Instant::now() < deadline {
            thread::sleep(TICK);
        }
        // Always reap the direct fixture, including on setup failure.
        holder.kill().unwrap();
        holder.wait().unwrap();
        assert!(ready.exists(), "writer fixture did not start");
        let pids: Vec<u32> = std::fs::read_to_string(ready)
            .unwrap()
            .split_whitespace()
            .map(|p| p.parse().unwrap())
            .collect();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.path().join("lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            -1,
            "parent death released a lock still owned by a writer"
        );
        loop {
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "watchdog did not release writer locks"
            );
            thread::sleep(TICK);
        }
        for pid in pids {
            assert!(
                identity(pid).unwrap().is_none_or(|p| p.zombie),
                "fixture writer remains live"
            );
        }
    }

    #[test]
    fn watchdog_deadline_and_raii_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let killed = run_supervised(
            &mut fixture_command("deadline", directory.path()),
            Duration::from_secs(2),
            &[],
        )
        .unwrap();
        assert_eq!(killed.exit_code, None);
        assert!(!killed.timed_out);
        let normal = run_supervised(
            &mut fixture_command("drop", directory.path()),
            Duration::from_secs(2),
            &[],
        )
        .unwrap();
        assert_eq!(normal.exit_code, Some(0));
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn inventory_is_metadata_only() {
        let command = inventory_command();
        assert_eq!(command.get_program(), "/bin/ps");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-axo", "pid=,ppid=,uid=,pgid=,lstart="]
        );
        let me = find_process(std::process::id()).unwrap().unwrap();
        assert_eq!(me.uid, unsafe { libc::geteuid() });
        assert_eq!(
            me.executable,
            std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
        );
        assert_eq!(find_process(me.pid).unwrap().unwrap(), me);
        // Existing same-UID processes may be protected on the test host.
        // Only independently verified zombies may be omitted on lookup errors.
        match inventory() {
            Ok(processes) => assert!(processes.iter().any(|p| p.pid == me.pid)),
            Err(error) => assert!(
                [
                    "process metadata unavailable",
                    "process executable unavailable",
                    "process executable cannot be resolved",
                    "process identity changed during inspection",
                    "process inventory deadline exceeded",
                ]
                .contains(&error.to_string().as_str()),
                "unexpected inventory error: {error}"
            ),
        }
    }

    #[test]
    fn missing_program_has_static_error() {
        let error = run_supervised(
            &mut Command::new("/no/such/secret-program"),
            Duration::from_secs(1),
            &[],
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "could not spawn supervised process");
    }

    #[test]
    fn invalid_identifiers_are_not_group_signals() {
        for pid in [0, 1, u32::MAX] {
            assert!(find_process(pid).is_err());
            assert!(group_alive(pid).is_err());
        }
        let mut child = shell("exit 0").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(find_process(pid).unwrap().is_none());
    }

    #[test]
    fn returns_output_and_status() {
        let result = run_supervised(
            &mut shell("printf hello; printf warning >&2; exit 7"),
            Duration::from_secs(2),
            &[],
        )
        .unwrap();
        assert_eq!(result.stdout, b"hello");
        assert_eq!(result.stderr, b"warning");
        assert_eq!(result.exit_code, Some(7));
        assert_eq!(result.reason, ProcessReason::Exited);
    }

    #[test]
    fn timeout_kills_nested_writers_without_signalling_parent_group() {
        let parent_group = unsafe { libc::getpgrp() };
        let start = Instant::now();
        let result = run_supervised(
            &mut shell("trap '' TERM; (trap '' TERM; sleep 30) & wait"),
            Duration::from_millis(150),
            &[],
        )
        .unwrap();
        assert!(result.timed_out);
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(unsafe { libc::getpgrp() }, parent_group);
    }

    #[test]
    fn cancellation_cleans_up_descendants() {
        let flag = Arc::new(AtomicBool::new(false));
        let trigger = flag.clone();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            trigger.store(true, Ordering::Relaxed);
        });
        let result = supervise(
            &mut shell("sleep 30 & wait"),
            Duration::from_secs(5),
            &[],
            flag,
        )
        .unwrap();
        thread.join().unwrap();
        assert!(result.cancelled);
    }

    #[test]
    fn combined_output_is_bounded() {
        let result = run_supervised(
            &mut shell("while :; do printf '%01024d' 0; printf '%01024d' 0 >&2; done"),
            Duration::from_secs(5),
            &[],
        )
        .unwrap();
        assert_eq!(result.reason, ProcessReason::OutputLimit);
        assert!(result.stdout.len() + result.stderr.len() <= OUTPUT_LIMIT);
    }

    #[test]
    fn normal_exit_also_cleans_up_inherited_pipes() {
        let start = Instant::now();
        let result =
            run_supervised(&mut shell("sleep 30 & exit 0"), Duration::from_secs(2), &[]).unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
