use anyhow::{Context, Result, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) const LIMIT: usize = 1024 * 1024;
pub(crate) const STATE_REF: &str = "refs/heads/hive-tasks-fake-state";

pub(crate) fn path(path: &Path, existing: bool) -> Result<PathBuf> {
    ensure!(
        path.is_absolute()
            && path.as_os_str().len() <= 4096
            && path
                .to_str()
                .is_some_and(|s| !s.chars().any(char::is_control)),
        "bounded absolute path required"
    );
    let parent = path.parent().context("parent required")?.canonicalize()?;
    let canonical = parent.join(path.file_name().context("file name required")?);
    ensure!(canonical == path, "canonical path required");
    if existing {
        ensure!(
            !fs::symlink_metadata(path)?.file_type().is_symlink(),
            "symlink refused"
        );
        ensure!(path.canonicalize()? == path, "canonical identity changed");
    }
    Ok(canonical)
}

pub(crate) fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    self::path(path, true)?;
    let file = open(path, false)?;
    ensure!(file.metadata()?.is_file(), "regular file required");
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= LIMIT, "JSON size limit");
    Ok(serde_json::from_slice(&bytes)?)
}

fn open(path: &Path, write: bool) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(OpenOptions::new()
        .read(true)
        .write(write)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?)
}

pub(crate) struct Lock {
    _file: File,
}
impl Lock {
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        let file = open(path, true)?;
        ensure!(file.metadata()?.is_file(), "regular lock required");
        // Nonblocking: competing invocations fail rather than wait across dispatch.
        loop {
            if unsafe {
                libc::flock(
                    std::os::fd::AsRawFd::as_raw_fd(&file),
                    libc::LOCK_EX | libc::LOCK_NB,
                )
            } == 0
            {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            let message = if error.kind() == std::io::ErrorKind::WouldBlock {
                "fake session busy"
            } else {
                "fake session lock acquisition failed"
            };
            return Err(error).with_context(|| format!("{message}: {}", path.display()));
        }
        Ok(Self { _file: file })
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        // A concurrent fork may retain this open description until exec, even with
        // CLOEXEC. Unlock explicitly instead of waiting for its last duplicate to close.
        loop {
            if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self._file), libc::LOCK_UN) }
                == 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
        }
    }
}

pub(crate) fn create(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= LIMIT, "JSON size limit");
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("parent required")?)?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

/// Isolated, bounded plumbing only. No worktree, index, hooks, credential helpers or network.
pub(crate) fn git(repo: &Path, args: &[&str], input: &[u8]) -> Result<String> {
    ensure!(input.len() <= LIMIT, "Git input limit");
    let mut stdin = tempfile::tempfile()?;
    stdin.write_all(input)?;
    stdin.seek(SeekFrom::Start(0))?;
    let mut stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    let mut child = Command::new("/usr/bin/git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Hive Fake")
        .env("GIT_AUTHOR_EMAIL", "fake@invalid")
        .env("GIT_COMMITTER_NAME", "Hive Fake")
        .env("GIT_COMMITTER_EMAIL", "fake@invalid")
        .args([
            "--no-pager",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.logAllRefUpdates=false",
            "-c",
            "core.fsync=loose-object,reference",
            "-c",
            "core.fsyncMethod=fsync",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .spawn()?;
    let start = Instant::now();
    let status = loop {
        if stdout.metadata()?.len() > LIMIT as u64
            || stderr.metadata()?.len() > LIMIT as u64
            || start.elapsed() > Duration::from_secs(10)
        {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Git resource limit");
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    ensure!(status.success(), "fake state Git operation failed");
    ensure!(stdout.metadata()?.len() <= LIMIT as u64, "Git output limit");
    stdout.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    stdout.read_to_string(&mut text)?;
    Ok(text.trim_end().into())
}

pub(crate) fn check_repo(repo: &Path) -> Result<String> {
    path(repo, true)?;
    ensure!(
        git(repo, &["rev-parse", "--is-bare-repository"], b"")? == "true",
        "dedicated bare state required"
    );
    ensure!(
        git(repo, &["rev-parse", "--absolute-git-dir"], b"")?
            == repo.to_str().context("UTF-8 path required")?,
        "foreign Git directory"
    );
    ensure!(
        !repo.join("objects/info/alternates").exists() && !repo.join("shallow").exists(),
        "foreign object storage"
    );
    let refs = git(repo, &["for-each-ref", "--format=%(refname)"], b"")?;
    ensure!(refs == STATE_REF, "foreign or missing state ref");
    let oid = git(repo, &["rev-parse", "--verify", STATE_REF], b"")?;
    ensure!(
        oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid state ref"
    );
    Ok(oid)
}

pub(crate) fn load<T: DeserializeOwned>(repo: &Path) -> Result<(String, T)> {
    let oid = check_repo(repo)?;
    let bytes = git(
        repo,
        &["cat-file", "blob", &format!("{oid}:state.json")],
        b"",
    )?;
    Ok((oid, serde_json::from_str(&bytes)?))
}

// Wait for sync-configured object writes and the exact-ref CAS before returning.
// Directory/host-crash durability is not proven; recovery remains process-crash-only.
pub(crate) fn save<T: Serialize>(repo: &Path, old: Option<&str>, value: &T) -> Result<String> {
    if let Some(old) = old {
        ensure!(check_repo(repo)? == old, "stale state CAS");
    }
    let bytes = serde_json::to_vec(value)?;
    let blob = git(repo, &["hash-object", "-w", "--stdin"], &bytes)?;
    let tree = git(
        repo,
        &["mktree"],
        format!("100644 blob {blob}\tstate.json\n").as_bytes(),
    )?;
    let mut args = vec!["commit-tree", &tree];
    if let Some(old) = old {
        args.extend(["-p", old]);
    }
    let commit = git(repo, &args, b"fake-only state\n")?;
    git(
        repo,
        &[
            "update-ref",
            STATE_REF,
            &commit,
            old.unwrap_or("0000000000000000000000000000000000000000"),
        ],
        b"",
    )?;
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_releases_lock_with_shared_description_still_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lock");
        create(&path, b"").unwrap();
        let lock = Lock::acquire(&path).unwrap();
        // dup/fork share the open file description, including its flock ownership.
        let shared_description = lock._file.try_clone().unwrap();
        let error = Lock::acquire(&path).err().expect("held lock must contend");
        let source = error.downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(source.kind(), std::io::ErrorKind::WouldBlock);
        assert!(source.raw_os_error().is_some());
        drop(lock);
        let reacquired = Lock::acquire(&path).unwrap();
        // Closing a stale duplicate must not unlock the new, independent description.
        drop(shared_description);
        assert!(Lock::acquire(&path).is_err());
        drop(reacquired);
        Lock::acquire(&path).unwrap();
    }

    #[test]
    fn acquire_drop_reacquire_under_concurrent_subprocess_load() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lock");
        create(&path, b"").unwrap();
        let start = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    for _ in 0..64 {
                        assert!(
                            Command::new("/usr/bin/git")
                                .arg("--version")
                                .env_clear()
                                .output()
                                .unwrap()
                                .status
                                .success()
                        );
                        assert!(
                            Command::new("/bin/sh")
                                .args(["-c", "exit 0"])
                                .env_clear()
                                .status()
                                .unwrap()
                                .success()
                        );
                    }
                });
            }
            start.wait();
            for _ in 0..512 {
                let lock = Lock::acquire(&path).unwrap();
                let duplicate = lock._file.try_clone().unwrap();
                std::thread::yield_now();
                drop(lock);
                let next = Lock::acquire(&path).unwrap();
                drop(duplicate);
                drop(next);
            }
        });
        Lock::acquire(&path).unwrap();
    }

    #[test]
    fn drop_releases_lock_while_subprocess_keeps_inherited_description() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lock");
        create(&path, b"").unwrap();
        let lock = Lock::acquire(&path).unwrap();
        // Deliberately keep the shared description across exec, making the
        // otherwise brief fork/exec inheritance window deterministic.
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .env_clear()
            .stdin(Stdio::from(lock._file.try_clone().unwrap()))
            .spawn()
            .unwrap();
        drop(lock);
        let reacquired = Lock::acquire(&path);
        let alive = child.try_wait().unwrap().is_none();
        // Reap the child even if the acquisition above failed.
        let _ = child.kill();
        child.wait().unwrap();
        assert!(
            alive,
            "child must still hold its inherited descriptor during acquisition"
        );
        reacquired.unwrap();
    }

    #[test]
    fn git_object_and_ref_writes_use_explicit_fsync_without_config_changes() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().canonicalize().unwrap().join("state.git");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--bare", "--template=", "."], b"").unwrap();
        let config = fs::read(repo.join("config")).unwrap();
        assert_eq!(
            git(&repo, &["config", "--get", "core.fsync"], b"").unwrap(),
            "loose-object,reference"
        );
        assert_eq!(
            git(&repo, &["config", "--get", "core.fsyncMethod"], b"").unwrap(),
            "fsync"
        );
        let first = save(&repo, None, &serde_json::json!({"test":1})).unwrap();
        let second = save(&repo, Some(&first), &serde_json::json!({"test":2})).unwrap();
        let (oid, value): (_, serde_json::Value) = load(&repo).unwrap();
        assert_eq!(oid, second);
        assert_eq!(value, serde_json::json!({"test":2}));
        assert_eq!(fs::read(repo.join("config")).unwrap(), config);
    }
}
