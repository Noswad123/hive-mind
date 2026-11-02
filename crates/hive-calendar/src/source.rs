//! Bounded committed blobs and ancestry; Markdown snapshots are not a complete publication.
//! Git >= 2.50 is required for GIT_NO_LAZY_FETCH. Trusted Git and MW are not an
//! OS security sandbox; this isolates their inputs/configuration, not hostile code.
//! Standalone MW copies are capped at 256 MiB. SIGINT/SIGTERM unwind cleanup;
//! SIGKILL and abrupt host termination cannot provide that guarantee.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{CommittedSnapshot, SourceBlob, SourceTask};

#[path = "process.rs"]
mod process;

const FILE_LIMIT: usize = 10_000;
const FILE_BYTES: usize = 2 * 1024 * 1024;
const TOTAL_BYTES: usize = 64 * 1024 * 1024;
const OUTPUT_BYTES: usize = 70 * 1024 * 1024;
const ITEM_LIMIT: usize = 100_000;
const PARSER_BYTES: u64 = 256 * 1024 * 1024;
const JSON_BYTES: usize = 1024 * 1024;

pub(crate) struct SourceSession {
    cancelled: Arc<AtomicBool>,
    registrations: Vec<signal_hook::SigId>,
    deadline: Instant,
}

impl SourceSession {
    #[cfg(test)]
    fn new() -> Result<Self> {
        Self::before(Instant::now() + Duration::from_secs(60))
    }

    fn before(deadline: Instant) -> Result<Self> {
        Self::operation(deadline.min(Instant::now() + Duration::from_secs(60)))
    }

    /// Keep this guard alive across approval, CPU replay and the final state CAS.
    /// Helper guards unregister only their own actions, never this root action.
    pub(crate) fn operation(deadline: Instant) -> Result<Self> {
        let mut session = Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            registrations: Vec::new(),
            deadline,
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            let id = signal_hook::flag::register(signal, session.cancelled.clone())
                .map_err(|_| anyhow!("source cancellation setup failed"))?;
            session.registrations.push(id);
        }
        Ok(session)
    }

    /// A child owns no signal registrations and cannot reset cancellation.
    pub(crate) fn child(&self, deadline: Instant) -> Result<Self> {
        self.check()?;
        Ok(Self {
            cancelled: Arc::clone(&self.cancelled),
            registrations: Vec::new(),
            deadline: deadline.min(self.deadline),
        })
    }

    #[cfg(test)]
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub(crate) fn check(&self) -> Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::Relaxed),
            "source operation cancelled"
        );
        ensure!(Instant::now() < self.deadline, "source deadline exceeded");
        Ok(())
    }
}

impl Drop for SourceSession {
    fn drop(&mut self) {
        for id in self.registrations.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').count() <= 64
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git"))
}

fn markdown(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".md")
}

struct Sandbox {
    _temp: tempfile::TempDir,
    root: PathBuf,
    notes: PathBuf,
    dashboard: PathBuf,
    env: BTreeMap<OsString, OsString>,
}

impl Sandbox {
    fn new() -> Result<Self> {
        let mut sandbox = Self::git_only()?;
        let config = sandbox.root.join("config/mw.toml");
        // Existing empty TOML suppresses MW's dotenv fallback; config::load is read-only.
        fs::write(&config, b"").map_err(|_| anyhow!("private config unavailable"))?;
        for (key, value) in [
            ("MW_CONFIG", config),
            ("NOTES_DIR", sandbox.notes.clone()),
            ("MW_INBOX_PATH", sandbox.notes.join("inbox.md")),
            ("INBOX_PATH", sandbox.notes.join("inbox.md")),
            ("DASHBOARD_PATH", sandbox.dashboard.clone()),
        ] {
            sandbox.env.insert(key.into(), value.into_os_string());
        }
        Ok(sandbox)
    }

    fn git_only() -> Result<Self> {
        // Do not use ambient TMPDIR to select the source workspace.
        let temp = tempfile::Builder::new()
            .prefix("hive-calendar-")
            .tempdir_in("/tmp")
            .map_err(|_| anyhow!("private source directory unavailable"))?;
        let root = temp
            .path()
            .canonicalize()
            .map_err(|_| anyhow!("private source directory unavailable"))?;
        let mut env = BTreeMap::new();
        for (key, dir) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
            ("TMPDIR", "tmp"),
        ] {
            let path = root.join(dir);
            fs::create_dir(&path).map_err(|_| anyhow!("private source directory unavailable"))?;
            env.insert(key.into(), path.into_os_string());
        }
        let notes = root.join("notes");
        fs::create_dir(&notes).map_err(|_| anyhow!("private notes directory unavailable"))?;
        let dashboard = notes.join("dashboard.md");
        for (key, value) in [("TMP", root.join("tmp")), ("TEMP", root.join("tmp"))] {
            env.insert(key.into(), value.into_os_string());
        }
        for (key, value) in [("PATH", "/usr/bin:/bin"), ("LANG", "C"), ("LC_ALL", "C")] {
            env.insert(key.into(), value.into());
        }
        Ok(Self {
            _temp: temp,
            root,
            notes,
            dashboard,
            env,
        })
    }

    fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command.env_clear().envs(&self.env).current_dir(&self.root);
        command
    }

    fn copy_parser(
        &self,
        executable: &mut fs::File,
        session: &SourceSession,
    ) -> Result<(PathBuf, String)> {
        session.check()?;
        let metadata = executable
            .metadata()
            .map_err(|_| anyhow!("MW executable unavailable"))?;
        ensure!(metadata.is_file(), "MW executable must be a regular file");
        ensure!(
            metadata.len() <= PARSER_BYTES,
            "MW executable size limit exceeded"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o111 != 0,
                "MW file is not executable"
            );
        }
        let binary = self.root.join("mw-parser");
        let mut copy = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&binary)
            .map_err(|_| anyhow!("private MW executable unavailable"))?;
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buf = [0; 65536];
        loop {
            session.check()?;
            let n = executable
                .read(&mut buf)
                .map_err(|_| anyhow!("MW executable unreadable"))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            ensure!(total <= PARSER_BYTES, "MW executable size limit exceeded");
            copy.write_all(&buf[..n])
                .map_err(|_| anyhow!("private MW executable copy failed"))?;
            hasher.update(&buf[..n]);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            copy.set_permissions(fs::Permissions::from_mode(0o700))
                .map_err(|_| anyhow!("private MW executable permissions failed"))?;
        }
        // Closing the writer is required before exec on Linux. Only this standalone
        // private artifact is invoked; install-relative parser assets are unsupported.
        drop(copy);
        session.check()?;
        Ok((binary, format!("{:x}", hasher.finalize())))
    }

    fn git(
        &self,
        repo: &Path,
        args: &[&str],
        input: &[u8],
        session: &SourceSession,
        cap: usize,
    ) -> Result<Vec<u8>> {
        ensure!(
            args.iter().map(|s| s.len() + 1).sum::<usize>() < 1024 * 1024,
            "source argument limit exceeded"
        );
        let mut command = self.command(Path::new("/usr/bin/git"));
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_GRAFT_FILE", "/dev/null")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .args([
                "--no-pager",
                "--no-optional-locks",
                "--no-replace-objects",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "protocol.allow=never",
                "-c",
                "core.attributesFile=/dev/null",
                "-C",
            ])
            .arg(repo)
            .args(args);
        session.check()?;
        process::run(
            &mut command,
            input,
            session.deadline,
            cap,
            &session.cancelled,
        )
    }

    fn require_commit(&self, repo: &Path, commit: &str, session: &SourceSession) -> Result<()> {
        ensure!(
            hex(commit, 40) || hex(commit, 64),
            "commit must be a full lowercase object ID"
        );
        let format = self.git(
            repo,
            &["rev-parse", "--show-object-format"],
            b"",
            session,
            1024,
        )?;
        ensure!(
            matches!(
                (format.as_slice(), commit.len()),
                (b"sha1\n", 40) | (b"sha256\n", 64)
            ),
            "commit must match repository object format"
        );
        let kind = self.git(
            repo,
            &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
            format!("{commit}\n").as_bytes(),
            session,
            1024,
        )?;
        ensure!(
            kind == format!("{commit} commit\n").as_bytes(),
            "source object must be exactly a commit"
        );
        let parsed = self.git(
            repo,
            &["rev-list", "--no-walk", "--max-count=1", commit, "--"],
            b"",
            session,
            1024,
        )?;
        ensure!(
            parsed == format!("{commit}\n").as_bytes(),
            "invalid source commit"
        );
        Ok(())
    }
}

pub(crate) fn read_snapshot(repo: &Path, commit: &str, mw_bin: &Path) -> Result<CommittedSnapshot> {
    read_snapshot_before(
        repo,
        commit,
        mw_bin,
        Instant::now() + Duration::from_secs(60),
    )
}

pub(crate) fn read_snapshot_before(
    repo: &Path,
    commit: &str,
    mw_bin: &Path,
    deadline: Instant,
) -> Result<CommittedSnapshot> {
    // Construct before any private workspace so handlers remain installed through
    // process-group cleanup and TempDir destruction on cancellation/error/success.
    let session = SourceSession::before(deadline)?;
    session.check()?;
    ensure!(
        hex(commit, 40) || hex(commit, 64),
        "commit must be a full lowercase object ID"
    );
    ensure!(
        repo.is_absolute() && mw_bin.is_absolute(),
        "source paths must be absolute"
    );
    let repo = repo
        .canonicalize()
        .map_err(|_| anyhow!("repository unavailable"))?;
    ensure!(repo.is_dir(), "repository must be a directory");
    let binary = mw_bin
        .canonicalize()
        .map_err(|_| anyhow!("MW executable unavailable"))?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut executable = options
        .open(&binary)
        .map_err(|_| anyhow!("MW executable unavailable"))?;
    let sandbox = Sandbox::new()?;
    let (binary, parser_sha256) = sandbox.copy_parser(&mut executable, &session)?;
    let version = sandbox.git(&repo, &["--version"], b"", &session, 1024)?;
    ensure!(supported_git(&version), "Git 2.50 or newer is required");
    sandbox.require_commit(&repo, commit, &session)?;
    let tree = sandbox.git(
        &repo,
        &["ls-tree", "-r", "-z", "--full-tree", commit],
        b"",
        &session,
        8 * 1024 * 1024,
    )?;
    let entries = parse_tree(&tree, commit.len(), &session)?;
    let input: Vec<u8> = entries
        .iter()
        .flat_map(|(_, oid)| oid.bytes().chain(std::iter::once(b'\n')))
        .collect();
    let batch = sandbox.git(
        &repo,
        &["cat-file", "--batch"],
        &input,
        &session,
        OUTPUT_BYTES,
    )?;
    let files = parse_batch(&batch, &entries, &session)?;
    for (path, blob) in &files {
        session.check()?;
        let target = sandbox.notes.join(path);
        fs::create_dir_all(target.parent().unwrap())
            .map_err(|_| anyhow!("private Markdown directory failed"))?;
        // Only validated regular blobs are materialized, never checkout, filters or .env.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .map_err(|_| anyhow!("private Markdown materialization failed"))?;
        file.write_all(&blob.content)
            .map_err(|_| anyhow!("private Markdown materialization failed"))?;
    }
    let bytes = process::run(
        sandbox
            .command(&binary)
            .args(["todos", "snapshot", "--json"]),
        b"",
        session.deadline,
        OUTPUT_BYTES,
        &session.cancelled,
    )?;
    let items = parse_snapshot(&bytes, &sandbox, &files, &session)?;
    session.check()?;
    Ok(CommittedSnapshot {
        commit: commit.into(),
        parser_sha256,
        files,
        items,
    })
}

fn git_source(repo: &Path, session: &SourceSession) -> Result<(PathBuf, Sandbox)> {
    session.check()?;
    ensure!(repo.is_absolute(), "repository path must be absolute");
    let repo = repo
        .canonicalize()
        .map_err(|_| anyhow!("repository unavailable"))?;
    ensure!(repo.is_dir(), "repository must be a directory");
    let sandbox = Sandbox::git_only()?;
    let version = sandbox.git(&repo, &["--version"], b"", session, 1024)?;
    ensure!(supported_git(&version), "Git 2.50 or newer is required");
    Ok((repo, sandbox))
}

/// Internal transport only: the caller owns the state-repository policy. The
/// session outlives its sandbox, including process cleanup on cancellation.
pub(crate) struct GitSession {
    sandbox: Sandbox,
    repo: PathBuf,
    session: SourceSession,
}

impl GitSession {
    #[cfg(test)]
    pub(crate) fn before(repo: &Path, deadline: Instant) -> Result<Self> {
        let session = SourceSession::before(deadline)?;
        let (repo, sandbox) = git_source(repo, &session)?;
        Ok(Self {
            sandbox,
            repo,
            session,
        })
    }

    pub(crate) fn with_operation(
        repo: &Path,
        deadline: Instant,
        operation: &SourceSession,
    ) -> Result<Self> {
        let session = operation.child(deadline)?;
        let (repo, sandbox) = git_source(repo, &session)?;
        Ok(Self {
            sandbox,
            repo,
            session,
        })
    }

    pub(crate) fn repo(&self) -> &Path {
        &self.repo
    }

    pub(crate) fn check(&self) -> Result<()> {
        self.session.check()
    }

    pub(crate) fn run(&self, args: &[&str], input: &[u8], cap: usize) -> Result<Vec<u8>> {
        // Command-line overrides only; never change repository configuration.
        let mut guarded = vec![
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.logAllRefUpdates=false",
        ];
        guarded.extend_from_slice(args);
        self.sandbox
            .git(&self.repo, &guarded, input, &self.session, cap)
    }
}

pub(crate) fn read_json_blob_before(
    repo: &Path,
    commit: &str,
    path: &str,
    deadline: Instant,
) -> Result<SourceBlob> {
    let session = SourceSession::before(deadline)?;
    session.check()?;
    ensure!(
        valid_path(path) && path.to_ascii_lowercase().ends_with(".json"),
        "source path must be a relative JSON path"
    );
    ensure!(
        hex(commit, 40) || hex(commit, 64),
        "commit must be a full lowercase object ID"
    );
    let (repo, sandbox) = git_source(repo, &session)?;
    sandbox.require_commit(&repo, commit, &session)?;
    let tree = sandbox.git(
        &repo,
        &[
            "ls-tree",
            "-z",
            "--full-tree",
            commit,
            "--",
            &format!(":(literal){path}"),
        ],
        b"",
        &session,
        8192,
    )?;
    let oid = json_tree_oid(&tree, path, commit.len())?;
    let batch = sandbox.git(
        &repo,
        &["cat-file", "--batch"],
        format!("{oid}\n").as_bytes(),
        &session,
        JSON_BYTES + 128,
    )?;
    let mut files = parse_batch(&batch, &[(path.into(), oid)], &session)?;
    let blob = files
        .remove(path)
        .ok_or_else(|| anyhow!("missing JSON blob"))?;
    ensure!(
        blob.content.len() <= JSON_BYTES,
        "JSON blob size limit exceeded"
    );
    session.check()?;
    Ok(blob)
}

/// Prove only local committed-path absence, not remote head or authoritative ledger absence.
pub(crate) fn require_json_absent_before(
    repo: &Path,
    commit: &str,
    path: &str,
    deadline: Instant,
) -> Result<()> {
    let session = SourceSession::before(deadline)?;
    session.check()?;
    ensure!(
        valid_path(path) && path.to_ascii_lowercase().ends_with(".json"),
        "source path must be a relative JSON path"
    );
    ensure!(
        hex(commit, 40) || hex(commit, 64),
        "commit must be a full lowercase object ID"
    );
    let (repo, sandbox) = git_source(repo, &session)?;
    sandbox.require_commit(&repo, commit, &session)?;
    // valid_path caps this walk at 64 entries. A missing parent permits future
    // directory creation; an existing non-tree parent must never prove absence.
    for end in path
        .match_indices('/')
        .map(|(i, _)| i)
        .chain(std::iter::once(path.len()))
    {
        let prefix = &path[..end];
        let tree = sandbox.git(
            &repo,
            &[
                "ls-tree",
                "-z",
                "--full-tree",
                commit,
                "--",
                &format!(":(literal){prefix}"),
            ],
            b"",
            &session,
            8192,
        )?;
        if tree.is_empty() {
            return session.check();
        }
        ensure!(end != path.len(), "JSON source path already exists");
        let record = tree
            .strip_suffix(b"\0")
            .and_then(|record| std::str::from_utf8(record).ok())
            .ok_or_else(|| anyhow!("invalid JSON parent tree entry"))?;
        let (header, actual_path) = record
            .split_once('\t')
            .ok_or_else(|| anyhow!("invalid JSON parent tree entry"))?;
        let parts: Vec<_> = header.split(' ').collect();
        ensure!(
            actual_path == prefix
                && parts.len() == 3
                && parts[0] == "040000"
                && parts[1] == "tree"
                && hex(parts[2], commit.len()),
            "JSON source parent must be a tree"
        );
    }
    unreachable!("validated path always has a final component")
}

fn json_tree_oid(bytes: &[u8], path: &str, oid_len: usize) -> Result<String> {
    let record = bytes
        .strip_suffix(b"\0")
        .ok_or_else(|| anyhow!("missing or invalid JSON tree entry"))?;
    let record = std::str::from_utf8(record).map_err(|_| anyhow!("invalid JSON tree encoding"))?;
    let (header, actual_path) = record
        .split_once('\t')
        .ok_or_else(|| anyhow!("invalid JSON tree entry"))?;
    ensure!(actual_path == path, "JSON tree path mismatch");
    let parts: Vec<_> = header.split(' ').collect();
    ensure!(
        parts.len() == 3
            && matches!(parts[0], "100644" | "100755")
            && parts[1] == "blob"
            && hex(parts[2], oid_len),
        "JSON source must be a regular blob"
    );
    Ok(parts[2].into())
}

pub(crate) fn require_ancestor_before(
    repo: &Path,
    ancestor: &str,
    descendant: &str,
    deadline: Instant,
) -> Result<()> {
    let session = SourceSession::before(deadline)?;
    session.check()?;
    ensure!(
        (hex(ancestor, 40) || hex(ancestor, 64)) && (hex(descendant, 40) || hex(descendant, 64)),
        "commits must be full lowercase object IDs"
    );
    let (repo, sandbox) = git_source(repo, &session)?;
    sandbox.require_commit(&repo, ancestor, &session)?;
    sandbox.require_commit(&repo, descendant, &session)?;
    let shallow = sandbox.git(
        &repo,
        &["rev-parse", "--is-shallow-repository"],
        b"",
        &session,
        1024,
    )?;
    ensure!(shallow == b"false\n", "source history must not be shallow");
    if ancestor != descendant {
        let output = sandbox.git(
            &repo,
            &["merge-base", "--is-ancestor", ancestor, descendant],
            b"",
            &session,
            1024,
        )?;
        ensure!(output.is_empty(), "invalid ancestry response");
    }
    session.check()
}

fn supported_git(bytes: &[u8]) -> bool {
    let Some(version) = std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.strip_prefix("git version "))
        .and_then(|s| s.split_whitespace().next())
    else {
        return false;
    };
    let mut parts = version.split('.');
    match (
        parts.next().and_then(|s| s.parse::<u32>().ok()),
        parts.next().and_then(|s| s.parse::<u32>().ok()),
    ) {
        (Some(major), Some(minor)) => major > 2 || (major == 2 && minor >= 50),
        _ => false,
    }
}

fn parse_tree(
    bytes: &[u8],
    oid_len: usize,
    session: &SourceSession,
) -> Result<Vec<(String, String)>> {
    session.check()?;
    ensure!(
        bytes.is_empty() || bytes.last() == Some(&0),
        "invalid Git tree framing"
    );
    let mut entries = Vec::new();
    if bytes.is_empty() {
        return Ok(entries);
    }
    let mut paths = BTreeSet::new();
    for record in bytes[..bytes.len() - 1].split(|b| *b == 0) {
        session.check()?;
        let record =
            std::str::from_utf8(record).map_err(|_| anyhow!("invalid Git tree encoding"))?;
        let (header, path) = record
            .split_once('\t')
            .ok_or_else(|| anyhow!("invalid Git tree entry"))?;
        ensure!(
            valid_path(path) && paths.insert(path.to_owned()),
            "invalid Git tree path"
        );
        let parts: Vec<_> = header.split(' ').collect();
        ensure!(
            parts.len() == 3 && hex(parts[2], oid_len),
            "invalid Git tree object ID"
        );
        ensure!(
            matches!(
                (parts[0], parts[1]),
                ("100644" | "100755" | "120000", "blob") | ("160000", "commit")
            ),
            "unsupported Git tree mode"
        );
        if markdown(path) {
            ensure!(
                matches!(parts[0], "100644" | "100755") && parts[1] == "blob",
                "Markdown must be a regular blob"
            );
            entries.push((path.into(), parts[2].into()));
            ensure!(
                entries.len() <= FILE_LIMIT,
                "Markdown file count limit exceeded"
            );
        }
    }
    Ok(entries)
}

fn parse_batch(
    mut bytes: &[u8],
    entries: &[(String, String)],
    session: &SourceSession,
) -> Result<BTreeMap<String, SourceBlob>> {
    session.check()?;
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for (path, oid) in entries {
        session.check()?;
        let end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| anyhow!("invalid Git blob framing"))?;
        ensure!(end < 128, "invalid Git blob header");
        let header =
            std::str::from_utf8(&bytes[..end]).map_err(|_| anyhow!("invalid Git blob header"))?;
        let parts: Vec<_> = header.split(' ').collect();
        ensure!(
            parts.len() == 3 && parts[0] == oid && parts[1] == "blob",
            "invalid Git blob provenance"
        );
        ensure!(
            !parts[2].is_empty() && parts[2].bytes().all(|b| b.is_ascii_digit()),
            "invalid Git blob size"
        );
        let len = parts[2]
            .parse::<usize>()
            .map_err(|_| anyhow!("invalid Git blob size"))?;
        ensure!(len <= FILE_BYTES, "Markdown file size limit exceeded");
        total += len;
        ensure!(total <= TOTAL_BYTES, "Markdown total size limit exceeded");
        bytes = &bytes[end + 1..];
        ensure!(
            bytes.len() > len && bytes[len] == b'\n',
            "invalid Git blob framing"
        );
        ensure!(
            files
                .insert(
                    path.clone(),
                    SourceBlob {
                        object_id: oid.clone(),
                        content: bytes[..len].to_vec()
                    }
                )
                .is_none(),
            "duplicate Git blob path"
        );
        bytes = &bytes[len + 1..];
    }
    ensure!(bytes.is_empty(), "unexpected Git blob output");
    Ok(files)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    schema_version: String,
    source: String,
    notes_dir: String,
    dashboard_path: String,
    items: Vec<SourceTask>,
}

fn parse_snapshot(
    bytes: &[u8],
    sandbox: &Sandbox,
    files: &BTreeMap<String, SourceBlob>,
    session: &SourceSession,
) -> Result<Vec<SourceTask>> {
    session.check()?;
    let mut snapshot: Snapshot =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("invalid MW snapshot response"))?;
    ensure!(
        snapshot.schema_version == "1" && snapshot.source == "saved-markdown",
        "invalid MW snapshot provenance"
    );
    ensure!(
        Some(snapshot.notes_dir.as_str()) == sandbox.notes.to_str()
            && Some(snapshot.dashboard_path.as_str()) == sandbox.dashboard.to_str(),
        "MW snapshot paths differ from private source"
    );
    ensure!(
        snapshot.items.len() <= ITEM_LIMIT,
        "MW snapshot item limit exceeded"
    );
    let mut ids = BTreeSet::new();
    let mut literal_bytes = 0usize;
    let checkbox = regex::Regex::new(r"^\s*[-*]\s*\[[ xX]\]\s+.+?\s*$")
        .map_err(|_| anyhow!("source checkbox validation unavailable"))?;
    let revisions: BTreeMap<_, _> = files
        .iter()
        .map(|(path, blob)| {
            session.check()?;
            Ok((
                path.as_str(),
                format!("sha256:{:x}", Sha256::digest(&blob.content)),
            ))
        })
        .collect::<Result<_>>()?;
    for task in &mut snapshot.items {
        session.check()?;
        ensure!(valid_path(&task.source_path), "invalid MW source path");
        let blob = files
            .get(&task.source_path)
            .ok_or_else(|| anyhow!("MW source is not committed Markdown"))?;
        ensure!(
            Some(&task.source_revision) == revisions.get(task.source_path.as_str()),
            "MW source revision mismatch"
        );
        let line_count = blob.content.split(|b| *b == b'\n').count()
            - usize::from(blob.content.last() == Some(&b'\n'));
        ensure!(
            !blob.content.is_empty() && task.line > 0 && task.line <= line_count,
            "invalid MW source line"
        );
        let literal = blob
            .content
            .split(|b| *b == b'\n')
            .nth(task.line - 1)
            .ok_or_else(|| anyhow!("invalid MW source line"))?;
        literal_bytes += literal.len();
        ensure!(
            literal_bytes <= TOTAL_BYTES,
            "MW literal text size limit exceeded"
        );
        task.literal_text = String::from_utf8(literal.to_vec())
            .map_err(|_| anyhow!("invalid MW source line encoding"))?;
        ensure!(
            checkbox.is_match(&task.literal_text),
            "MW source line is not a checkbox"
        );
        ensure!(
            !task.source_id.trim().is_empty()
                && task.source_id.len() <= 4096
                && !task.source_id.chars().any(char::is_control),
            "invalid MW source locator"
        );
        let identity_ok = match (task.identity_kind.as_str(), task.stable_id.as_deref()) {
            ("stable", Some(stable)) => hex(stable, 32) && task.id == format!("task:{stable}"),
            ("positional", None) => task
                .id
                .strip_prefix(&format!("{}:{}:", task.source_id, task.line))
                .is_some_and(|order| {
                    !order.is_empty()
                        && order.bytes().all(|b| b.is_ascii_digit())
                        && order
                            .parse::<usize>()
                            .is_ok_and(|n| n < line_count && order == n.to_string())
                }),
            _ => false,
        };
        ensure!(
            identity_ok && ids.insert(task.id.clone()),
            "invalid or duplicate MW task identity"
        );
    }
    Ok(snapshot.items)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        repo: PathBuf,
        mw: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir_in("/tmp").unwrap();
            let root = temp.path().canonicalize().unwrap();
            let repo = root.join("repo");
            fs::create_dir(&repo).unwrap();
            let mw = root.join("mw");
            let fixture = Self {
                _temp: temp,
                root,
                repo,
                mw,
            };
            fixture.git(&["init", "--quiet"]);
            fixture.stub("");
            fixture
        }

        fn git(&self, args: &[&str]) -> Vec<u8> {
            let output = Command::new("/usr/bin/git")
                .env_clear()
                .env("HOME", &self.root)
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-C",
                ])
                .arg(&self.repo)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "fixture Git failed");
            output.stdout
        }

        fn commit(&self) -> String {
            self.git(&["add", "--all"]);
            self.git(&[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "isolated fixture",
            ]);
            String::from_utf8(self.git(&["rev-parse", "HEAD"]))
                .unwrap()
                .trim()
                .into()
        }

        fn stub(&self, checks: &str) {
            let script = format!(
                r#"#!/bin/sh
set -eu
[ "$#" = 3 ] && [ "$1" = todos ] && [ "$2" = snapshot ] && [ "$3" = --json ]
[ "$PWD" != '{repo}' ]
[ -d "$HOME" ] && [ -d "$XDG_CONFIG_HOME" ] && [ -d "$XDG_DATA_HOME" ] && [ -d "$XDG_CACHE_HOME" ] && [ -d "$TMPDIR" ]
[ -f "$MW_CONFIG" ] && [ ! -s "$MW_CONFIG" ]
[ "$NOTES_DIR" = "$PWD/notes" ] && [ "$DASHBOARD_PATH" = "$NOTES_DIR/dashboard.md" ]
[ "$MW_INBOX_PATH" = "$NOTES_DIR/inbox.md" ] && [ "$INBOX_PATH" = "$MW_INBOX_PATH" ]
[ ! -e .env ] && [ ! -e "$NOTES_DIR/.env" ] && [ ! -e "$NOTES_DIR/.git" ]
for key in $(/usr/bin/env); do
  case "$key" in
    HOME=*|PATH=*|XDG_CONFIG_HOME=*|XDG_DATA_HOME=*|XDG_CACHE_HOME=*|XDG_STATE_HOME=*|MW_CONFIG=*|NOTES_DIR=*|MW_INBOX_PATH=*|INBOX_PATH=*|DASHBOARD_PATH=*|TMPDIR=*|TMP=*|TEMP=*|LANG=*|LC_ALL=*|PWD=*|SHLVL=*|_=*) ;;
    *) exit 9 ;;
  esac
done
{checks}
printf '{{"schemaVersion":"1","source":"saved-markdown","notesDir":"%s","dashboardPath":"%s","items":[]}}' "$NOTES_DIR" "$DASHBOARD_PATH"
"#,
                repo = self.repo.display()
            );
            fs::write(&self.mw, script).unwrap();
            fs::set_permissions(&self.mw, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(120)
    }

    #[test]
    fn json_absence_uses_literal_committed_paths_and_is_read_only_despite_poison() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.repo.join("state")).unwrap();
        fs::write(fixture.repo.join("state/present.json"), "{}").unwrap();
        let absent = fixture.commit();
        let path = "state/:(glob)*[x].JSON";
        fs::write(fixture.repo.join(path), "private committed content").unwrap();
        let present = fixture.commit();
        fixture.git(&["replace", &absent, &present]);
        fs::remove_file(&fixture.mw).unwrap();
        fs::remove_file(fixture.repo.join(path)).unwrap();
        symlink("/does/not/exist/secret", fixture.repo.join(path)).unwrap();
        fs::write(fixture.repo.join(".env"), "private poison").unwrap();
        let head = fixture.git(&["rev-parse", "HEAD"]);
        let refs = fixture.git(&["show-ref"]);
        let status = fixture.git(&["status", "--porcelain=v1", "-z"]);
        let index = fs::read(fixture.repo.join(".git/index")).unwrap();
        for missing in [
            path,
            "state/missing.json",
            "new/deep/ledger.json",
            "state/*.json",
        ] {
            require_json_absent_before(&fixture.repo, &absent, missing, deadline()).unwrap();
        }
        for existing in [path, "state/present.json"] {
            let error = require_json_absent_before(&fixture.repo, &present, existing, deadline())
                .unwrap_err()
                .to_string();
            assert_eq!(error, "JSON source path already exists");
            assert!(!error.contains("private"));
        }
        assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(fixture.git(&["show-ref"]), refs);
        assert_eq!(fs::read(fixture.repo.join(".git/index")).unwrap(), index);
        assert_eq!(fixture.git(&["status", "--porcelain=v1", "-z"]), status);
        assert_eq!(
            fs::read_link(fixture.repo.join(path)).unwrap(),
            Path::new("/does/not/exist/secret")
        );
        fixture.git(&["replace", "-d", &absent]);
        fixture.git(&["replace", &present, &absent]);
        assert!(require_json_absent_before(&fixture.repo, &present, path, deadline()).is_err());
    }

    #[test]
    fn json_absence_refuses_every_existing_mode_and_non_tree_parent() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("file.json"), "{}").unwrap();
        fs::write(fixture.repo.join("exec.json"), "{}").unwrap();
        fs::set_permissions(
            fixture.repo.join("exec.json"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        symlink("/does/not/exist/secret", fixture.repo.join("link.json")).unwrap();
        fs::create_dir(fixture.repo.join("dir.json")).unwrap();
        fs::write(fixture.repo.join("dir.json/child.json"), "{}").unwrap();
        let base = fixture.commit();
        fixture.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{base},module.json"),
        ]);
        fixture.git(&["commit", "--quiet", "-m", "gitlink fixture"]);
        let commit = String::from_utf8(fixture.git(&["rev-parse", "HEAD"])).unwrap();
        for path in [
            "file.json",
            "exec.json",
            "link.json",
            "dir.json",
            "module.json",
        ] {
            assert_eq!(
                require_json_absent_before(&fixture.repo, commit.trim(), path, deadline())
                    .unwrap_err()
                    .to_string(),
                "JSON source path already exists",
                "{path}"
            );
        }
        for parent in ["file.json", "exec.json", "link.json", "module.json"] {
            for suffix in ["child.json", "missing/child.json"] {
                assert_eq!(
                    require_json_absent_before(
                        &fixture.repo,
                        commit.trim(),
                        &format!("{parent}/{suffix}"),
                        deadline()
                    )
                    .unwrap_err()
                    .to_string(),
                    "JSON source parent must be a tree",
                    "{parent}/{suffix}"
                );
            }
        }
        require_json_absent_before(
            &fixture.repo,
            commit.trim(),
            "dir.json/new/ledger.json",
            deadline(),
        )
        .unwrap();
    }

    #[test]
    fn json_absence_rejects_unsafe_paths_noncommits_and_exceeded_bounds() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("good.json"), "{}").unwrap();
        let commit = fixture.commit();
        for path in [
            "",
            "/a.json",
            "../a.json",
            "./a.json",
            "a//b.json",
            "a\\b.json",
            ".GiT/a.json",
            "a.md",
            "a.json\n",
        ] {
            assert!(
                require_json_absent_before(&fixture.repo, &commit, path, deadline()).is_err(),
                "{path}"
            );
        }
        for path in [
            format!("{}a.json", "x/".repeat(64)),
            format!("{}.json", "x".repeat(4092)),
        ] {
            assert_eq!(
                require_json_absent_before(&fixture.repo, &commit, &path, deadline())
                    .unwrap_err()
                    .to_string(),
                "source path must be a relative JSON path"
            );
        }
        // The 64-component bound is inclusive, and missing parents remain legal.
        require_json_absent_before(
            &fixture.repo,
            &commit,
            &format!("{}a.json", "x/".repeat(63)),
            deadline(),
        )
        .unwrap();
        let deepest = fixture.repo.join("x/".repeat(63));
        fs::create_dir_all(&deepest).unwrap();
        fs::write(deepest.join("sibling.txt"), "committed parent trees").unwrap();
        let deep_commit = fixture.commit();
        require_json_absent_before(
            &fixture.repo,
            &deep_commit,
            &format!("{}a.json", "x/".repeat(63)),
            deadline(),
        )
        .unwrap();
        let tree = String::from_utf8(fixture.git(&["rev-parse", "HEAD^{tree}"])).unwrap();
        let blob = String::from_utf8(fixture.git(&["rev-parse", "HEAD:good.json"])).unwrap();
        for invalid in [
            "HEAD",
            "--help",
            &commit[..12],
            &commit.to_uppercase(),
            &"0".repeat(40),
            &"a".repeat(64),
            tree.trim(),
            blob.trim(),
        ] {
            assert!(
                require_json_absent_before(&fixture.repo, invalid, "missing.json", deadline())
                    .is_err()
            );
        }
        assert!(
            require_json_absent_before(Path::new("relative"), &commit, "missing.json", deadline())
                .is_err()
        );
        assert_eq!(
            require_json_absent_before(&fixture.repo, &commit, "missing.json", Instant::now())
                .unwrap_err()
                .to_string(),
            "source deadline exceeded"
        );
        let session = SourceSession::new().unwrap();
        session.cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            git_source(&fixture.repo, &session)
                .err()
                .unwrap()
                .to_string(),
            "source operation cancelled"
        );
    }

    #[test]
    fn json_blob_is_exact_committed_literal_path_without_parser_or_worktree_reads() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.repo.join("state")).unwrap();
        let path = "state/:(glob)*[x].JSON";
        let content = b"{\"committed\":true}\n";
        fs::write(fixture.repo.join(path), content).unwrap();
        let commit = fixture.commit();
        let oid =
            String::from_utf8(fixture.git(&["rev-parse", &format!("{commit}:{path}")])).unwrap();
        fs::remove_file(&fixture.mw).unwrap();
        fs::remove_file(fixture.repo.join(path)).unwrap();
        symlink("/does/not/exist/secret", fixture.repo.join(path)).unwrap();
        fs::write(fixture.repo.join("state/untracked.json"), "poison").unwrap();
        fs::write(fixture.repo.join(".env"), "poison").unwrap();
        let blob = read_json_blob_before(&fixture.repo, &commit, path, deadline()).unwrap();
        assert_eq!(blob.content, content);
        assert_eq!(blob.object_id, oid.trim());
        fs::remove_file(fixture.repo.join(path)).unwrap();
        fs::write(fixture.repo.join(path), b"replacement poison").unwrap();
        let replacement = fixture.commit();
        fixture.git(&["replace", &commit, &replacement]);
        let replacement_oid =
            String::from_utf8(fixture.git(&["rev-parse", &format!("{replacement}:{path}")]))
                .unwrap();
        fixture.git(&["replace", oid.trim(), replacement_oid.trim()]);
        let original = read_json_blob_before(&fixture.repo, &commit, path, deadline()).unwrap();
        assert_eq!(original.object_id, oid.trim());
        assert_eq!(original.content, content);
        fs::remove_file(fixture.repo.join(path)).unwrap();
        symlink("/does/not/exist/secret", fixture.repo.join(path)).unwrap();
        assert!(
            fs::symlink_metadata(fixture.repo.join(path))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            read_json_blob_before(&fixture.repo, &commit, "state/untracked.json", deadline())
                .is_err()
        );
        assert!(read_json_blob_before(&fixture.repo, &commit, "state/*.JSON", deadline()).is_err());
        let session = SourceSession::new().unwrap();
        let sandbox = Sandbox::git_only().unwrap();
        assert!(!sandbox.root.join("config/mw.toml").exists());
        assert!(!sandbox.env.contains_key(&OsString::from("MW_CONFIG")));
        sandbox
            .require_commit(&fixture.repo, &commit, &session)
            .unwrap();
    }

    #[test]
    fn json_blob_rejects_unsafe_paths_noncommits_missing_and_nonregular_entries() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("good.json"), "{}").unwrap();
        symlink("good.json", fixture.repo.join("link.json")).unwrap();
        fs::create_dir(fixture.repo.join("dir.json")).unwrap();
        fs::write(fixture.repo.join("dir.json/child.json"), "{}").unwrap();
        let base = fixture.commit();
        fixture.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{base},module.json"),
        ]);
        fixture.git(&["commit", "--quiet", "-m", "gitlink fixture"]);
        let commit = String::from_utf8(fixture.git(&["rev-parse", "HEAD"])).unwrap();
        let commit = commit.trim();
        for path in [
            "",
            "/good.json",
            "../good.json",
            "./good.json",
            "a//good.json",
            "a\\good.json",
            ".git/good.json",
            "good.md",
            "good.json\n",
            "missing.json",
            "link.json",
            "module.json",
            "dir.json",
            ":(glob)*.json",
        ] {
            assert!(
                read_json_blob_before(&fixture.repo, commit, path, deadline()).is_err(),
                "{path}"
            );
        }
        let tree = String::from_utf8(fixture.git(&["rev-parse", "HEAD^{tree}"])).unwrap();
        let blob = String::from_utf8(fixture.git(&["rev-parse", "HEAD:good.json"])).unwrap();
        for invalid in [
            "HEAD",
            "main",
            "--help",
            &commit[..12],
            &commit.to_uppercase(),
            &"0".repeat(40),
            &"a".repeat(64),
            tree.trim(),
            blob.trim(),
        ] {
            assert!(
                read_json_blob_before(&fixture.repo, invalid, "good.json", deadline()).is_err()
            );
            assert!(require_ancestor_before(&fixture.repo, invalid, invalid, deadline()).is_err());
        }
    }

    #[test]
    fn json_blob_limit_and_malformed_tree_and_batch_objects_are_rejected() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("cap.json"), vec![b' '; JSON_BYTES]).unwrap();
        let commit = fixture.commit();
        assert_eq!(
            read_json_blob_before(&fixture.repo, &commit, "cap.json", deadline())
                .unwrap()
                .content
                .len(),
            JSON_BYTES
        );
        fs::write(fixture.repo.join("cap.json"), vec![b' '; JSON_BYTES + 1]).unwrap();
        let commit = fixture.commit();
        assert!(read_json_blob_before(&fixture.repo, &commit, "cap.json", deadline()).is_err());
        let oid = "a".repeat(40);
        for bytes in [
            Vec::new(),
            format!("100644 blob {oid}\ta.json").into_bytes(),
            format!("100644 blob {oid}\tb.json\0").into_bytes(),
            format!("100644 blob {oid}\ta.json\0extra\0").into_bytes(),
            format!("120000 blob {oid}\ta.json\0").into_bytes(),
            format!("160000 commit {oid}\ta.json\0").into_bytes(),
            format!("100644 blob {}\ta.json\0", "A".repeat(40)).into_bytes(),
        ] {
            assert!(json_tree_oid(&bytes, "a.json", 40).is_err());
        }
        // Delete only a fixture-owned loose blob to exercise missing object handling.
        let oid = String::from_utf8(fixture.git(&["rev-parse", "HEAD:cap.json"])).unwrap();
        let oid = oid.trim();
        fs::remove_file(
            fixture
                .repo
                .join(".git/objects")
                .join(&oid[..2])
                .join(&oid[2..]),
        )
        .unwrap();
        assert!(read_json_blob_before(&fixture.repo, &commit, "cap.json", deadline()).is_err());
    }

    #[test]
    fn ancestry_accepts_only_valid_self_and_forward_complete_history() {
        let fixture = Fixture::new();
        let first = fixture.commit();
        let second = fixture.commit();
        fixture.git(&["checkout", "--quiet", "--detach", &first]);
        fs::write(fixture.repo.join("divergent.json"), "{}").unwrap();
        let divergent = fixture.commit();
        assert!(require_ancestor_before(&fixture.repo, &first, &first, deadline()).is_ok());
        assert!(require_ancestor_before(&fixture.repo, &first, &second, deadline()).is_ok());
        for (a, b) in [
            (&second, &first),
            (&second, &divergent),
            (&divergent, &second),
            (&"0".repeat(40), &second),
        ] {
            assert!(require_ancestor_before(&fixture.repo, a, b, deadline()).is_err());
        }
        // Replacements and legacy grafts must not forge a positive relation.
        fixture.git(&["checkout", "--quiet", "--detach", &second]);
        let forged = fixture.commit();
        fixture.git(&["replace", &divergent, &forged]);
        assert!(require_ancestor_before(&fixture.repo, &second, &divergent, deadline()).is_err());
        fs::write(
            fixture.repo.join(".git/info/grafts"),
            format!("{divergent} {second}\n{second}\n"),
        )
        .unwrap();
        assert!(require_ancestor_before(&fixture.repo, &second, &divergent, deadline()).is_err());
        assert!(require_ancestor_before(&fixture.repo, &first, &second, deadline()).is_ok());
        fs::write(fixture.repo.join(".git/shallow"), format!("{second}\n")).unwrap();
        assert!(require_ancestor_before(&fixture.repo, &first, &second, deadline()).is_err());
        assert!(require_ancestor_before(&fixture.repo, &second, &second, deadline()).is_err());
        fs::remove_file(fixture.repo.join(".git/shallow")).unwrap();
        fs::remove_file(
            fixture
                .repo
                .join(".git/objects")
                .join(&first[..2])
                .join(&first[2..]),
        )
        .unwrap();
        assert!(require_ancestor_before(&fixture.repo, &first, &second, deadline()).is_err());
    }

    #[test]
    fn source_deadlines_are_caller_bounded_and_at_most_sixty_seconds() {
        let start = Instant::now();
        let short = start + Duration::from_secs(1);
        assert_eq!(SourceSession::before(short).unwrap().deadline, short);
        let long = SourceSession::before(deadline()).unwrap();
        assert!(long.deadline <= Instant::now() + Duration::from_secs(60));
        let fixture = Fixture::new();
        let commit = fixture.commit();
        let expired = Instant::now();
        for error in [
            read_snapshot_before(&fixture.repo, &commit, &fixture.mw, expired)
                .err()
                .unwrap(),
            read_json_blob_before(&fixture.repo, &commit, "a.json", expired)
                .err()
                .unwrap(),
            require_ancestor_before(&fixture.repo, &commit, &commit, expired).unwrap_err(),
        ] {
            assert_eq!(error.to_string(), "source deadline exceeded");
        }
    }

    #[test]
    fn operation_children_share_cancellation_and_keep_the_full_root_deadline() {
        let end = deadline();
        let root = SourceSession::operation(end).unwrap();
        assert_eq!(root.deadline, end);
        assert_eq!(root.registrations.len(), 2);
        let child = root.child(end + Duration::from_secs(60)).unwrap();
        assert_eq!(child.deadline, end);
        assert!(child.registrations.is_empty());
        assert!(Arc::ptr_eq(&root.cancelled, &child.cancelled));
        let short = Instant::now() + Duration::from_secs(1);
        assert_eq!(root.child(short).unwrap().deadline, short);
        drop(SourceSession::before(end).unwrap());
        assert!(root.check().is_ok());
        child.cancel();
        drop(child);
        assert_eq!(
            root.check().unwrap_err().to_string(),
            "source operation cancelled"
        );
        assert!(root.child(end).is_err());
    }

    #[test]
    fn malformed_commits_and_missing_intermediate_history_fail_closed() {
        let fixture = Fixture::new();
        let first = fixture.commit();
        let middle = fixture.commit();
        let last = fixture.commit();
        fs::remove_file(
            fixture
                .repo
                .join(".git/objects")
                .join(&middle[..2])
                .join(&middle[2..]),
        )
        .unwrap();
        assert!(require_ancestor_before(&fixture.repo, &first, &last, deadline()).is_err());
        let malformed = fixture.root.join("malformed-commit");
        fs::write(&malformed, b"not a commit\n").unwrap();
        let oid = String::from_utf8(fixture.git(&[
            "hash-object",
            "--literally",
            "-t",
            "commit",
            "-w",
            malformed.to_str().unwrap(),
        ]))
        .unwrap();
        for error in [
            require_ancestor_before(&fixture.repo, oid.trim(), oid.trim(), deadline()).err(),
            read_json_blob_before(&fixture.repo, oid.trim(), "a.json", deadline()).err(),
            require_json_absent_before(&fixture.repo, oid.trim(), "a.json", deadline()).err(),
        ] {
            let error = error.unwrap().to_string();
            assert!(!error.contains("not a commit"));
        }
    }

    #[test]
    fn json_and_ancestry_helpers_support_sha256_without_abbreviations() {
        let fixture = Fixture::new();
        let repo = fixture.root.join("sha256-repo");
        fs::create_dir(&repo).unwrap();
        let fixture = Fixture { repo, ..fixture };
        fixture.git(&["init", "--quiet", "--object-format=sha256"]);
        fs::write(fixture.repo.join("ledger.json"), b"{}\n").unwrap();
        let first = fixture.commit();
        let last = fixture.commit();
        let blob = read_json_blob_before(&fixture.repo, &first, "ledger.json", deadline()).unwrap();
        assert_eq!(blob.object_id.len(), 64);
        assert_eq!(blob.content, b"{}\n");
        require_json_absent_before(&fixture.repo, &first, "new/ledger.json", deadline()).unwrap();
        assert!(
            require_json_absent_before(&fixture.repo, &first, "ledger.json", deadline()).is_err()
        );
        assert!(
            require_json_absent_before(&fixture.repo, &first[..40], "missing.json", deadline())
                .is_err()
        );
        assert!(require_ancestor_before(&fixture.repo, &first, &last, deadline()).is_ok());
        assert!(require_ancestor_before(&fixture.repo, &first, &first, deadline()).is_ok());
        assert!(require_ancestor_before(&fixture.repo, &last, &first, deadline()).is_err());
        assert!(require_ancestor_before(&fixture.repo, &first[..40], &last, deadline()).is_err());
        assert!(
            read_json_blob_before(&fixture.repo, &first[..40], "ledger.json", deadline()).is_err()
        );
    }

    #[test]
    fn only_committed_regular_markdown_is_materialized_in_private_environment() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("a.MD"), b"committed\0bytes\n").unwrap();
        fs::write(fixture.repo.join(".env"), "SECRET=do-not-read").unwrap();
        fs::write(fixture.repo.join("ignored.txt"), "not Markdown").unwrap();
        let commit = fixture.commit();
        fs::write(fixture.repo.join("a.MD"), "uncommitted").unwrap();
        fs::write(fixture.repo.join("untracked.md"), "untracked").unwrap();
        fixture.stub("[ ! -e \"$NOTES_DIR/untracked.md\" ] && [ ! -e \"$NOTES_DIR/ignored.txt\" ]\n[ $(/usr/bin/wc -c < \"$NOTES_DIR/a.MD\") = 16 ]");
        let snapshot = read_snapshot(&fixture.repo, &commit, &fixture.mw).unwrap();
        assert_eq!(snapshot.commit, commit);
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files["a.MD"].content, b"committed\0bytes\n");
        assert_eq!(
            snapshot.parser_sha256,
            format!("{:x}", Sha256::digest(fs::read(&fixture.mw).unwrap()))
        );
        assert_eq!(snapshot.files["a.MD"].object_id.len(), 40);
        assert!(snapshot.items.is_empty());
        assert_eq!(fs::read(fixture.repo.join("a.MD")).unwrap(), b"uncommitted");
    }

    #[test]
    fn rejects_refs_noncommits_relative_paths_and_nonexecutables() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("a.md"), "fixture").unwrap();
        let commit = fixture.commit();
        for invalid in [
            "HEAD",
            "main",
            "--help",
            "abcd",
            &commit.to_uppercase(),
            &format!("{commit}^{{commit}}"),
            &"0".repeat(40),
        ] {
            assert!(read_snapshot(&fixture.repo, invalid, &fixture.mw).is_err());
        }
        for object in ["HEAD^{tree}", "HEAD:a.md"] {
            let oid = String::from_utf8(fixture.git(&["rev-parse", object])).unwrap();
            assert!(read_snapshot(&fixture.repo, oid.trim(), &fixture.mw).is_err());
        }
        assert!(read_snapshot(Path::new("relative"), &commit, &fixture.mw).is_err());
        assert!(read_snapshot(&fixture.repo, &commit, Path::new("mw")).is_err());
        fs::set_permissions(&fixture.mw, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_snapshot(&fixture.repo, &commit, &fixture.mw).is_err());
    }

    #[test]
    fn rejects_committed_markdown_symlinks_and_gitlinks_without_following() {
        let fixture = Fixture::new();
        symlink("/does/not/exist/secret", fixture.repo.join("evil.md")).unwrap();
        let commit = fixture.commit();
        assert!(read_snapshot(&fixture.repo, &commit, &fixture.mw).is_err());
        fs::remove_file(fixture.repo.join("evil.md")).unwrap();
        fixture.git(&["add", "--all"]);
        fixture.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{commit},module.md"),
        ]);
        fixture.git(&["commit", "--quiet", "-m", "gitlink fixture"]);
        let oid = String::from_utf8(fixture.git(&["rev-parse", "HEAD"])).unwrap();
        assert!(read_snapshot(&fixture.repo, oid.trim(), &fixture.mw).is_err());
    }

    #[test]
    fn sha256_repository_requires_the_full_oid_even_for_an_empty_tree() {
        let fixture = Fixture::new();
        let repo = fixture.root.join("sha256-repo");
        fs::create_dir(&repo).unwrap();
        let sha256 = Fixture {
            _temp: fixture._temp,
            root: fixture.root,
            repo,
            mw: fixture.mw,
        };
        sha256.git(&["init", "--quiet", "--object-format=sha256"]);
        let empty = sha256.commit();
        assert_eq!(empty.len(), 64);
        assert!(read_snapshot(&sha256.repo, &empty[..40], &sha256.mw).is_err());
        assert!(read_snapshot(&sha256.repo, &empty, &sha256.mw).is_ok());
        fs::write(sha256.repo.join("a.md"), "fixture\n").unwrap();
        let commit = sha256.commit();
        let snapshot = read_snapshot(&sha256.repo, &commit, &sha256.mw).unwrap();
        assert_eq!(snapshot.files["a.md"].object_id.len(), 64);
    }

    #[test]
    fn replacement_objects_and_worktree_edits_do_not_change_committed_content() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("a.md"), "original\n").unwrap();
        let original = fixture.commit();
        fs::write(fixture.repo.join("a.md"), "replacement\n").unwrap();
        let replacement = fixture.commit();
        fixture.git(&["replace", &original, &replacement]);
        let snapshot = read_snapshot(&fixture.repo, &original, &fixture.mw).unwrap();
        assert_eq!(snapshot.files["a.md"].content, b"original\n");
    }

    #[test]
    fn tree_parser_rejects_malformed_modes_ids_paths_and_framing() {
        let session = SourceSession::new().unwrap();
        let oid = "a".repeat(40);
        for (mode, kind, id, path) in [
            ("100644", "blob", oid.clone(), "../a.md"),
            ("100644", "blob", oid.clone(), "/a.md"),
            ("100644", "blob", oid.clone(), "a//b.md"),
            ("100644", "blob", oid.clone(), "./a.md"),
            ("100644", "blob", oid.clone(), "a\\b.md"),
            ("100644", "blob", oid.clone(), "a\nb.md"),
            ("100644", "blob", oid.clone(), ".GiT/a.md"),
            ("120000", "blob", oid.clone(), "a.Md"),
            ("160000", "commit", oid.clone(), "a.md"),
            ("100664", "blob", oid.clone(), "a.md"),
            ("100644", "tree", oid.clone(), "a.md"),
            ("100644", "blob", "A".repeat(40), "a.md"),
            ("100644", "blob", "a".repeat(39), "a.md"),
        ] {
            assert!(
                parse_tree(
                    format!("{mode} {kind} {id}\t{path}\0").as_bytes(),
                    40,
                    &session
                )
                .is_err(),
                "{path}"
            );
        }
        for bytes in [b"bad".as_slice(), b"\0", b"\xff\0"] {
            assert!(parse_tree(bytes, 40, &session).is_err());
        }
        let record = format!("100644 blob {oid}\ta.md\0");
        assert!(parse_tree(record.repeat(2).as_bytes(), 40, &session).is_err());
        assert!(parse_tree(format!("{record}\0").as_bytes(), 40, &session).is_err());
        assert!(!valid_path(&format!("{}a.md", "x/".repeat(64))));
        assert!(!valid_path(&"a".repeat(4097)));
        assert_eq!(
            parse_tree(
                format!("120000 blob {oid}\tignored.txt\0").as_bytes(),
                40,
                &session
            )
            .unwrap()
            .len(),
            0
        );
        let records: String = (0..=FILE_LIMIT)
            .map(|n| format!("100644 blob {oid}\t{n}.md\0"))
            .collect();
        assert!(parse_tree(records.as_bytes(), 40, &session).is_err());
        assert!(
            parse_tree(
                format!("100755 blob {}\tx.MD\0", "a".repeat(64)).as_bytes(),
                64,
                &session
            )
            .is_ok()
        );
    }

    #[test]
    fn batch_parser_is_byte_exact_and_rejects_size_type_id_and_framing_errors() {
        let session = SourceSession::new().unwrap();
        let oid = "a".repeat(40);
        let entries = vec![("a.md".into(), oid.clone())];
        let mut good = format!("{oid} blob 4\n").into_bytes();
        good.extend_from_slice(b"\0\xff\nX\n");
        assert_eq!(
            parse_batch(&good, &entries, &session).unwrap()["a.md"].content,
            b"\0\xff\nX"
        );
        for bad in [
            format!("{oid} blob 0\n"),
            format!("{oid} tree 0\n\n"),
            format!("{} blob 0\n\n", "b".repeat(40)),
            format!("{oid} blob -1\n\n"),
            format!("{oid} missing\n"),
            format!("{oid} blob {}\n", FILE_BYTES + 1),
            format!("{oid} blob 999999999999999999999999999\n"),
            format!("{oid} blob 0\nX"),
            format!("{oid} blob 0\n\nextra"),
        ] {
            assert!(parse_batch(bad.as_bytes(), &entries, &session).is_err());
        }
        let mut batch = Vec::new();
        let mut entries = Vec::new();
        for n in 0..33 {
            entries.push((format!("{n}.md"), oid.clone()));
            batch.extend_from_slice(format!("{oid} blob {FILE_BYTES}\n").as_bytes());
            batch.resize(batch.len() + FILE_BYTES, b'x');
            batch.push(b'\n');
        }
        assert!(parse_batch(&batch, &entries, &session).is_err());
    }

    fn snapshot_fixture(sandbox: &Sandbox) -> (BTreeMap<String, SourceBlob>, Value) {
        let content = b"- [ ] fixture\n".to_vec();
        let task = json!({"id":"source:1:0", "stableId":null, "identityKind":"positional",
            "sourceRevision":format!("sha256:{:x}", Sha256::digest(&content)), "sourceId":"source",
            "sourcePath":"a.md", "line":1, "text":"fixture", "done":false,
            "metadata":{"due":"", "raw":"", "start":"", "estimate":""}});
        let value = json!({"schemaVersion":"1", "source":"saved-markdown", "notesDir":sandbox.notes,
            "dashboardPath":sandbox.dashboard,"items":[task]});
        (
            BTreeMap::from([(
                "a.md".into(),
                SourceBlob {
                    object_id: "a".repeat(40),
                    content,
                },
            )]),
            value,
        )
    }

    #[test]
    fn validates_snapshot_schema_exact_paths_provenance_lines_and_identity() {
        let session = SourceSession::new().unwrap();
        let sandbox = Sandbox::new().unwrap();
        let (files, original) = snapshot_fixture(&sandbox);
        let parse =
            |v: &Value| parse_snapshot(&serde_json::to_vec(v).unwrap(), &sandbox, &files, &session);
        assert_eq!(parse(&original).unwrap().len(), 1);
        let mut stable = original.clone();
        stable["items"][0]["id"] = json!(format!("task:{}", "a".repeat(32)));
        stable["items"][0]["stableId"] = json!("a".repeat(32));
        stable["items"][0]["identityKind"] = json!("stable");
        assert!(parse(&stable).is_ok());
        for (pointer, value) in [
            ("/schemaVersion", json!(1)),
            ("/schemaVersion", json!("2")),
            ("/source", json!("database")),
            ("/notesDir", json!("/private/user/notes")),
            (
                "/dashboardPath",
                json!(format!("{}/../notes/dashboard.md", sandbox.notes.display())),
            ),
            ("/items/0/sourcePath", json!("../a.md")),
            ("/items/0/sourcePath", json!("other.md")),
            (
                "/items/0/sourceRevision",
                json!(format!("sha256:{}", "a".repeat(64))),
            ),
            ("/items/0/sourceId", json!("")),
            ("/items/0/sourceId", json!("secret\n")),
            ("/items/0/line", json!(0)),
            ("/items/0/line", json!(2)),
            ("/items/0/id", json!("source:1:00")),
            ("/items/0/id", json!("wrong:1:0")),
            ("/items/0/id", json!("source:1:2")),
            ("/items/0/identityKind", json!("stable")),
            ("/items/0/stableId", json!("a".repeat(32))),
        ] {
            let mut bad = original.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            let error = parse(&bad).err().unwrap().to_string();
            assert!(
                !error.contains("secret") && !error.contains("/private/user"),
                "{pointer}"
            );
        }
        for base in [&original, &stable] {
            let mut duplicate = base.clone();
            duplicate["items"]
                .as_array_mut()
                .unwrap()
                .push(base["items"][0].clone());
            assert!(parse(&duplicate).is_err());
        }
        let mut excessive = original.clone();
        excessive["items"] = Value::Array(vec![original["items"][0].clone(); ITEM_LIMIT + 1]);
        assert!(
            parse(&excessive)
                .err()
                .unwrap()
                .to_string()
                .contains("item limit")
        );
        assert!(
            parse_snapshot(b"secret invalid JSON", &sandbox, &files, &session)
                .err()
                .unwrap()
                .to_string()
                == "invalid MW snapshot response"
        );
    }

    #[test]
    fn git_version_requires_lazy_fetch_guard_support() {
        for version in ["git version 2.50.1 (Apple Git-155)", "git version 3.0.0"] {
            assert!(supported_git(version.as_bytes()));
        }
        for version in ["git version 2.49.9", "git version 1.99.0", "garbage"] {
            assert!(!supported_git(version.as_bytes()));
        }
    }

    #[test]
    fn literal_text_is_the_exact_committed_checkbox_not_normalized_parser_text() {
        let session = SourceSession::new().unwrap();
        let sandbox = Sandbox::new().unwrap();
        let (mut files, mut value) = snapshot_fixture(&sandbox);
        let literal = "  * [ ] [Area due:2026-11-01] Work due:2026-11-02  \r";
        files.get_mut("a.md").unwrap().content = format!("# Header\n{literal}\n").into_bytes();
        value["items"][0]["line"] = json!(2);
        value["items"][0]["id"] = json!("source:2:0");
        value["items"][0]["text"] = json!("Work due:2026-11-02");
        value["items"][0]["literalText"] = json!("forged parser content");
        value["items"][0]["sourceRevision"] = json!(format!(
            "sha256:{:x}",
            Sha256::digest(&files["a.md"].content)
        ));
        let items = parse_snapshot(
            &serde_json::to_vec(&value).unwrap(),
            &sandbox,
            &files,
            &session,
        )
        .unwrap();
        assert_eq!(items[0].literal_text, literal);
        assert_eq!(items[0].text, "Work due:2026-11-02");
        for content in [
            b"# not a checkbox\n".to_vec(),
            b"- [ ] secret\xff\n".to_vec(),
        ] {
            files.get_mut("a.md").unwrap().content = content;
            value["items"][0]["line"] = json!(1);
            value["items"][0]["id"] = json!("source:1:0");
            value["items"][0]["sourceRevision"] = json!(format!(
                "sha256:{:x}",
                Sha256::digest(&files["a.md"].content)
            ));
            let error = parse_snapshot(
                &serde_json::to_vec(&value).unwrap(),
                &sandbox,
                &files,
                &session,
            )
            .err()
            .unwrap()
            .to_string();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn parser_copy_uses_open_handle_and_hashes_the_invoked_private_artifact() {
        let fixture = Fixture::new();
        let session = SourceSession::new().unwrap();
        let sandbox = Sandbox::new().unwrap();
        let original = fs::read(&fixture.mw).unwrap();
        let mut opened = fs::File::open(&fixture.mw).unwrap();
        let replacement = fixture.root.join("replacement-mw");
        fs::write(&replacement, "#!/bin/sh\nprintf secret >&2\nexit 9\n").unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
        fs::rename(replacement, &fixture.mw).unwrap();
        let (binary, digest) = sandbox.copy_parser(&mut opened, &session).unwrap();
        assert_ne!(binary, fixture.mw);
        assert_eq!(digest, format!("{:x}", Sha256::digest(&original)));
        assert_eq!(fs::read(&binary).unwrap(), original);
        assert_eq!(
            fs::metadata(&binary).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let out = process::run(
            sandbox
                .command(&binary)
                .args(["todos", "snapshot", "--json"]),
            b"",
            session.deadline,
            OUTPUT_BYTES,
            &session.cancelled,
        )
        .unwrap();
        assert!(
            parse_snapshot(&out, &sandbox, &BTreeMap::new(), &session)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn original_parser_path_can_be_atomically_replaced_during_private_execution() {
        let fixture = Fixture::new();
        let commit = fixture.commit();
        let replacement = fixture.root.join("replacement-mw");
        fs::write(&replacement, "#!/bin/sh\nprintf secret >&2\nexit 9\n").unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
        fixture.stub(&format!(
            "[ \"$0\" = \"$PWD/mw-parser\" ]\n/bin/mv '{}' '{}'",
            replacement.display(),
            fixture.mw.display()
        ));
        let original_digest = format!("{:x}", Sha256::digest(fs::read(&fixture.mw).unwrap()));
        let snapshot = read_snapshot(&fixture.repo, &commit, &fixture.mw).unwrap();
        assert_eq!(snapshot.parser_sha256, original_digest);
        assert_ne!(
            snapshot.parser_sha256,
            format!("{:x}", Sha256::digest(fs::read(&fixture.mw).unwrap()))
        );
        let error = read_snapshot(&fixture.repo, &commit, &fixture.mw)
            .err()
            .unwrap()
            .to_string();
        assert_eq!(error, "source process failed");
    }

    #[test]
    fn copying_and_parsing_check_session_cancellation_and_parser_size() {
        let fixture = Fixture::new();
        let session = SourceSession::new().unwrap();
        let sandbox = Sandbox::new().unwrap();
        let mut opened = fs::File::open(&fixture.mw).unwrap();
        session.cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            sandbox
                .copy_parser(&mut opened, &session)
                .err()
                .unwrap()
                .to_string(),
            "source operation cancelled"
        );
        assert_eq!(
            parse_tree(b"", 40, &session).err().unwrap().to_string(),
            "source operation cancelled"
        );
        assert_eq!(
            parse_batch(b"", &[], &session).err().unwrap().to_string(),
            "source operation cancelled"
        );
        assert_eq!(
            parse_snapshot(b"{}", &sandbox, &BTreeMap::new(), &session)
                .err()
                .unwrap()
                .to_string(),
            "source operation cancelled"
        );
        session.cancelled.store(false, Ordering::Relaxed);
        let oversized = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&fixture.mw)
            .unwrap();
        oversized.set_len(PARSER_BYTES + 1).unwrap();
        assert_eq!(
            sandbox
                .copy_parser(&mut opened, &session)
                .err()
                .unwrap()
                .to_string(),
            "MW executable size limit exceeded"
        );
    }

    #[test]
    #[ignore = "internal subprocess fixture, invoked by the SIGINT/SIGTERM test"]
    fn cancellation_preview_child() {
        let root = PathBuf::from(
            std::env::var_os("HIVE_CALENDAR_CANCEL_FIXTURE").expect("explicit fixture root"),
        );
        let commit = fs::read_to_string(root.join("commit")).unwrap();
        let error = read_snapshot(&root.join("repo"), &commit, &root.join("mw"))
            .err()
            .unwrap()
            .to_string();
        assert_eq!(error, "source operation cancelled");
        fs::write(root.join("finished"), "cancelled").unwrap();
    }

    #[test]
    fn sigint_and_sigterm_unwind_preview_groups_and_private_workspace() {
        use std::{
            ffi::CString,
            os::unix::fs::OpenOptionsExt,
            process::{Child, Stdio},
        };
        struct FixtureChild(Option<Child>);
        impl Drop for FixtureChild {
            fn drop(&mut self) {
                if let Some(child) = self.0.as_mut() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let fixture = Fixture::new();
            fs::write(fixture.root.join("commit"), fixture.commit()).unwrap();
            let fifo = fixture.root.join("descendant-pipe");
            let fifo_c = CString::new(fifo.to_str().unwrap()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
            let mut reader = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
                .unwrap();
            fs::write(&fixture.mw, format!("#!/bin/sh\nset -eu\nexec 3>'{}'\n/bin/sleep 30 &\nprintf '%s' \"$PWD\" >'{}'\nwait\n", fifo.display(), fixture.root.join("started").display())).unwrap();
            let mut child = FixtureChild(Some(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "source::tests::cancellation_preview_child",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env_clear()
                    .env("HIVE_CALENDAR_CANCEL_FIXTURE", &fixture.root)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            ));
            let deadline = Instant::now() + Duration::from_secs(10);
            let private_root = loop {
                if let Ok(path) = fs::read_to_string(fixture.root.join("started"))
                    && !path.is_empty()
                {
                    break PathBuf::from(path);
                }
                assert!(Instant::now() < deadline, "preview fixture did not start");
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(private_root.is_dir());
            assert_eq!(
                reader.read(&mut [0; 1]).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            // Only signal our own unreaped preview-test child, never an ambient process.
            assert_eq!(
                unsafe { libc::kill(child.0.as_ref().unwrap().id() as libc::pid_t, signal) },
                0
            );
            let status = loop {
                if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
                    child.0.take();
                    break status;
                }
                assert!(
                    Instant::now() < deadline,
                    "preview fixture failed to cancel"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(
                status.success(),
                "cancellation fixture did not unwind normally"
            );
            assert_eq!(
                fs::read_to_string(fixture.root.join("finished")).unwrap(),
                "cancelled"
            );
            assert!(!private_root.exists(), "private source workspace leaked");
            loop {
                match reader.read(&mut [0; 1]) {
                    Ok(0) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "MW or descendant retained pipe after cancellation"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    result => panic!("unexpected fixture pipe result: {result:?}"),
                }
            }
        }
    }

    #[test]
    #[ignore = "requires explicit MW_TEST_BIN pointing to a trusted read-only-compatible MW executable"]
    fn real_mw_saved_markdown_fixture() {
        let binary =
            PathBuf::from(std::env::var_os("MW_TEST_BIN").expect("set explicit MW_TEST_BIN"));
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("a.md"), "---\nid: calendar-fixture\ndomains: [task]\ntask_active: true\n---\n# Fixture\n## Todo\n- [ ] fixture {due:2026-11-01}\n").unwrap();
        let commit = fixture.commit();
        let snapshot = read_snapshot(&fixture.repo, &commit, &binary).unwrap();
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.items.len(), 1);
    }
}
