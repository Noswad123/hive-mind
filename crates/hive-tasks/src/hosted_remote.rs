//! Private transport laboratory. No production constructor or credential delivery
//! exists yet. Every write traverses upload/receive-pack, including inception.
//! Assumes cooperative exclusive administration: hostile reset/ABA is not fenced.
use super::*;
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
};

pub(super) const REF: &str = "refs/heads/hive-tasks-publisher-state";
const LIMIT: usize = 1024 * 1024;
const HISTORY: usize = 256;

// A local path is deliberately not a production URL-shaped constructor.
pub(super) struct Remote {
    path: PathBuf,
    root: String,
    namespace: String,
}

#[path = "hosted_remote_tests.rs"]
mod tests;
pub(super) struct View {
    stage: tempfile::TempDir,
    pub(super) oid: String,
    pub(super) history: Vec<Value>,
}
pub(super) struct Receipt {
    authority: PathBuf,
    root: String,
    namespace: String,
    old: String,
    new: String,
    operation: String,
    invocation: String,
}
pub(super) enum Exchange {
    Fresh(Receipt),
    Rejected,
    Unknown,
}
impl Receipt {
    pub(super) fn binds(
        &self,
        remote: &Remote,
        old: &str,
        new: &str,
        operation: &str,
        invocation: &str,
    ) -> bool {
        self.authority == remote.path
            && self.root == remote.root
            && self.namespace == remote.namespace
            && self.old == old
            && self.new == new
            && self.operation == operation
            && self.invocation == invocation
    }
}

// No inherited environment, repository, helpers, hooks, URL rewriting, agents or
// parser children. Output/errors never leave this facade. Group cleanup also runs
// after exit so pipe-holding children cannot escape a successful parent exit.
struct Process(Child);
impl Process {
    fn status(&self) -> Result<Option<bool>> {
        // Pin the leader PID/PGID until group cleanup finishes, including normal
        // exit. try_wait would reap it early and allow PID reuse before killpg.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.0.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                return Ok(None);
            }
            anyhow::bail!("remote_git_wait_failed");
        }
        #[cfg(target_os = "linux")]
        let (pid, status) = unsafe { (info.si_pid(), info.si_status()) };
        #[cfg(target_os = "macos")]
        let (pid, status) = (info.si_pid, info.si_status);
        if pid == 0 {
            return Ok(None);
        }
        ensure!(pid == self.0.id() as libc::pid_t, "remote_git_wait_failed");
        Ok(Some(info.si_code == libc::CLD_EXITED && status == 0))
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}
fn git(repo: &Path, args: &[&str], input: &[u8]) -> Result<(bool, String)> {
    ensure!(input.len() <= LIMIT, "remote_git_input_limit");
    let mut stdin = tempfile::tempfile()?;
    stdin.write_all(input)?;
    stdin.seek(SeekFrom::Start(0))?;
    let mut stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    let mut command = Command::new("/usr/bin/git");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/usr/bin/false")
        .env("GIT_AUTHOR_NAME", "Hive synthetic")
        .env("GIT_AUTHOR_EMAIL", "hive@invalid")
        .env("GIT_COMMITTER_NAME", "Hive synthetic")
        .env("GIT_COMMITTER_EMAIL", "hive@invalid")
        .args([
            "--no-pager",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "credential.helper=",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "fetch.unpackLimit=1",
            "-c",
            "transfer.unpackLimit=1",
            "-c",
            "core.abbrev=40",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .process_group(0)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?));
    // Kept packed by fetch.unpackLimit=1; cap even a malicious oversized pack
    // before object decoding. CPU/file limits are inherited by transport children.
    unsafe {
        command.pre_exec(|| {
            for (resource, bound) in [
                (libc::RLIMIT_FSIZE, 32 * 1024 * 1024),
                (libc::RLIMIT_CPU, 10),
            ] {
                let limit = libc::rlimit {
                    rlim_cur: bound,
                    rlim_max: bound,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let child = Process(
        command
            .spawn()
            .map_err(|_| anyhow!("remote_git_spawn_failed"))?,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = loop {
        if stdout.metadata()?.len() > LIMIT as u64
            || stderr.metadata()?.len() > LIMIT as u64
            || Instant::now() >= deadline
        {
            break Err(anyhow!("remote_git_resource_limit"));
        }
        match child.status() {
            Ok(Some(success)) => break Ok(success),
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break Err(anyhow!("remote_git_wait_failed")),
        }
    };
    drop(child);
    let success = result?;
    ensure!(
        stdout.metadata()?.len() <= LIMIT as u64 && stderr.metadata()?.len() <= LIMIT as u64,
        "remote_git_output_limit"
    );
    stdout.seek(SeekFrom::Start(0))?;
    let mut out = String::new();
    stdout
        .read_to_string(&mut out)
        .map_err(|_| anyhow!("remote_git_encoding"))?;
    Ok((success, out))
}
fn plumbing(repo: &Path, args: &[&str], input: &[u8]) -> Result<String> {
    let (ok, out) = git(repo, args, input)?;
    ensure!(ok, "remote_git_failed");
    Ok(out.trim_end().into())
}
fn stage() -> Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    plumbing(
        dir.path(),
        &["init", "--bare", "--template=", "--object-format=sha1", "."],
        b"",
    )?;
    Ok(dir)
}
fn commit(repo: &Path, old: Option<&str>, value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= LIMIT, "remote_state_limit");
    let blob = plumbing(repo, &["hash-object", "-w", "--stdin"], &bytes)?;
    let tree = plumbing(
        repo,
        &["mktree"],
        format!("100644 blob {blob}\tstate.json\n").as_bytes(),
    )?;
    let mut args = vec!["commit-tree", &tree];
    if let Some(old) = old {
        args.extend(["-p", old]);
    }
    plumbing(repo, &args, b"hosted test-origin state\n")
}
impl Remote {
    fn check_server(&self) -> Result<()> {
        storage::path(&self.path, true)?;
        // Only default bare configuration, not even an include/helper is accepted.
        let config = fs::read_to_string(self.path.join("config"))?;
        let normalized = config
            .replace("\tignorecase = true\n", "")
            .replace("\tprecomposeunicode = true\n", "");
        ensure!(
            normalized
                == "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = true\n",
            "untrusted_test_server_config"
        );
        ensure!(
            !self.path.join("shallow").exists()
                && !self.path.join("objects/info/alternates").exists(),
            "foreign_remote_storage"
        );
        Ok(())
    }
    // Explicit synthetic inception, never an observe-missing fallback. The caller
    // must keep the resulting root in independently trusted provisioning.
    pub(super) fn initialize(path: &Path, namespace: &str, genesis: &Value) -> Result<Self> {
        ensure!(
            genesis["origin"] == "Loopback" && genesis["namespace"] == namespace,
            "synthetic_inception_required"
        );
        let mut remote = Self {
            path: path.into(),
            root: String::new(),
            namespace: namespace.into(),
        };
        remote.check_server()?;
        let stage = stage()?;
        let new = commit(stage.path(), None, genesis)?;
        match remote.push(stage.path(), "", &new, "inception", "inception", false)? {
            Exchange::Fresh(_) => {
                remote.root = new;
                Ok(remote)
            }
            _ => anyhow::bail!("inception_not_acknowledged"),
        }
    }
    pub(super) fn reopen(&self) -> Self {
        Self {
            path: self.path.clone(),
            root: self.root.clone(),
            namespace: self.namespace.clone(),
        }
    }
    pub(super) fn observe(&self) -> Result<View> {
        let deadline = Instant::now() + Duration::from_secs(30);
        self.check_server()?;
        let stage = stage()?;
        let url = self
            .path
            .to_str()
            .ok_or_else(|| anyhow!("invalid_remote_path"))?;
        let refs = plumbing(stage.path(), &["ls-remote", "--refs", "--", url, REF], b"")?;
        let (tip, reference) = refs
            .split_once('\t')
            .ok_or_else(|| anyhow!("missing_remote_state"))?;
        ensure!(hex(tip, 40) && reference == REF, "invalid_remote_ref");
        let fetch_ref = format!("{REF}:refs/hive/observed");
        plumbing(
            stage.path(),
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                "--no-recurse-submodules",
                "--upload-pack=/usr/bin/git -c core.hooksPath=/dev/null upload-pack",
                "--",
                url,
                &fetch_ref,
            ],
            b"",
        )?;
        // Fetch racing ls-remote is deliberately not silently upgraded.
        ensure!(
            plumbing(
                stage.path(),
                &["rev-parse", "--verify", "refs/hive/observed"],
                b""
            )? == tip,
            "remote_ref_changed_during_observe"
        );
        let lines = plumbing(
            stage.path(),
            &["rev-list", "--parents", "--max-count=257", tip],
            b"",
        )?;
        let rows: Vec<_> = lines.lines().collect();
        ensure!(
            !rows.is_empty() && rows.len() <= HISTORY,
            "remote_history_limit"
        );
        let mut history = Vec::new();
        let mut total = 0;
        for (index, row) in rows.iter().enumerate() {
            remaining(deadline)?;
            let parts: Vec<_> = row.split(' ').collect();
            ensure!(hex(parts[0], 40), "invalid_remote_commit");
            if index + 1 == rows.len() {
                ensure!(
                    parts.len() == 1 && parts[0] == self.root,
                    "remote_inception_mismatch"
                );
            } else {
                ensure!(
                    parts.len() == 2 && rows[index + 1].split(' ').next() == Some(parts[1]),
                    "remote_history_discontinuous"
                );
            }
            let tree = plumbing(stage.path(), &["ls-tree", "-r", parts[0]], b"")?;
            let (entry, path) = tree
                .split_once('\t')
                .ok_or_else(|| anyhow!("invalid_remote_tree"))?;
            ensure!(
                path == "state.json" && entry.starts_with("100644 blob ") && hex(&entry[12..], 40),
                "invalid_remote_tree"
            );
            let raw = plumbing(stage.path(), &["cat-file", "blob", &entry[12..]], b"")?;
            total += raw.len();
            ensure!(total <= 16 * LIMIT, "remote_history_bytes_limit");
            let value = unique(raw.as_bytes())?;
            ensure!(
                value["origin"] == "Loopback" && value["namespace"] == self.namespace,
                "remote_origin_namespace_mismatch"
            );
            history.push(value);
        }
        history.reverse();
        remaining(deadline)?;
        Ok(View {
            stage,
            oid: tip.into(),
            history,
        })
    }
    pub(super) fn exchange(
        &self,
        view: &View,
        value: &Value,
        operation: &str,
        invocation: &str,
        lose_ack: bool,
    ) -> Result<Exchange> {
        self.check_server()?;
        ensure!(
            value["origin"] == "Loopback" && value["namespace"] == self.namespace,
            "remote_proposal_binding_mismatch"
        );
        let new = commit(view.stage.path(), Some(&view.oid), value)?;
        self.push(
            view.stage.path(),
            &view.oid,
            &new,
            operation,
            invocation,
            lose_ack,
        )
    }
    fn push(
        &self,
        stage: &Path,
        old: &str,
        new: &str,
        operation: &str,
        invocation: &str,
        lose_ack: bool,
    ) -> Result<Exchange> {
        ensure!(
            (old.is_empty() || hex(old, 40)) && hex(new, 40),
            "invalid_remote_proposal"
        );
        // Verify the actual proposed commit, not merely a caller's ancestry claim.
        let parents = plumbing(stage, &["rev-list", "--parents", "-n", "1", new], b"")?;
        ensure!(
            parents
                == if old.is_empty() {
                    new.to_string()
                } else {
                    format!("{new} {old}")
                },
            "remote_proposal_parent_mismatch"
        );
        let lease = format!("--force-with-lease={REF}:{old}");
        let spec = format!("{new}:{REF}");
        let output = git(
            stage,
            &[
                "push",
                "--porcelain",
                &lease,
                "--receive-pack=/usr/bin/git -c core.hooksPath=/dev/null receive-pack",
                "--",
                self.path
                    .to_str()
                    .ok_or_else(|| anyhow!("invalid_remote_path"))?,
                &spec,
            ],
            b"",
        );
        if lose_ack {
            return Ok(Exchange::Unknown);
        }
        let Ok((success, out)) = output else {
            return Ok(Exchange::Unknown);
        };
        self.acknowledgement(old, new, operation, invocation, success, &out)
    }
    fn acknowledgement(
        &self,
        old: &str,
        new: &str,
        operation: &str,
        invocation: &str,
        success: bool,
        out: &str,
    ) -> Result<Exchange> {
        let spec = format!("{new}:{REF}");
        let lines: Vec<_> = out.lines().collect();
        if lines.len() != 3
            || lines[0] != format!("To {}", self.path.display())
            || lines[2] != "Done"
        {
            return Ok(Exchange::Unknown);
        }
        let fields: Vec<_> = lines[1].split('\t').collect();
        if fields.len() != 3 || fields[1] != spec {
            return Ok(Exchange::Unknown);
        }
        if !success && fields[0] == "!" && fields[2] == "[rejected] (stale info)" {
            return Ok(Exchange::Rejected);
        }
        let fresh = if old.is_empty() {
            fields[0] == "*" && fields[2] == "[new branch]"
        } else {
            fields[0] == " " && fields[2] == format!("{old}..{new}")
        };
        if !success || !fresh {
            return Ok(Exchange::Unknown);
        }
        Ok(Exchange::Fresh(Receipt {
            authority: self.path.clone(),
            root: self.root.clone(),
            namespace: self.namespace.clone(),
            old: old.into(),
            new: new.into(),
            operation: operation.into(),
            invocation: invocation.into(),
        }))
    }
}
