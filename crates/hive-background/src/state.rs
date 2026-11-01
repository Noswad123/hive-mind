//! Private, bounded state files. Locks are persistent inodes, never stale-unlinked.
use anyhow::{Result, bail, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) const MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub config: PathBuf,
    pub status: PathBuf,
    pub registry: PathBuf,
    pub leases: PathBuf,
    pub worker_lock: PathBuf,
    pub admission_lock: PathBuf,
    pub delivery_lock: PathBuf,
}

impl Paths {
    pub fn production() -> Result<Self> {
        Ok(Self::in_root(
            hive_google_auth::storage::system_user_home()?
                .join("Library/Application Support/Hive/background"),
        ))
    }

    /// Alternate roots are for library fixtures, not a production CLI override.
    pub fn in_root(root: PathBuf) -> Self {
        Self {
            config: root.join("config.json"),
            status: root.join("status.json"),
            registry: root.join("editors"),
            leases: root.join("leases"),
            worker_lock: root.join("worker.lock"),
            admission_lock: root.join("admission.lock"),
            delivery_lock: root.join("delivery.lock"),
            root,
        }
    }

    pub fn ensure(&self) -> Result<()> {
        directory(&self.root, true)?;
        directory(&self.registry, true)?;
        directory(&self.leases, true)?;
        Ok(())
    }
}

fn cstr(value: &std::ffi::OsStr) -> Result<CString> {
    Ok(CString::new(value.as_bytes())?)
}

fn fd_result(fd: libc::c_int) -> Result<File> {
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Walk from / using pinned directory descriptors; never follow an ancestor link.
pub(crate) fn directory(path: &Path, create: bool) -> Result<File> {
    directory_policy(path, create, true)
}

// LaunchAgents may already be a conventional 0755 directory. Its contents
// still use private single-link files and the same trusted ancestry checks.
pub(crate) fn directory_policy(path: &Path, create: bool, private: bool) -> Result<File> {
    ensure!(path.is_absolute(), "state path must be absolute");
    let mut dir = File::open("/")?;
    let parts: Vec<_> = path.components().collect();
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(name) = part else {
            ensure!(matches!(part, Component::RootDir), "ambiguous state path");
            continue;
        };
        let name = cstr(name)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0
            && create
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
        {
            let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) };
            if rc != 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(std::io::Error::last_os_error().into());
            }
            fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        }
        dir = fd_result(fd)?;
        let meta = dir.metadata()?;
        let uid = unsafe { libc::geteuid() };
        ensure!(
            (meta.uid() == 0 || meta.uid() == uid) && meta.mode() & 0o002 == 0,
            "untrusted state ancestor"
        );
        if index + 1 == parts.len() {
            ensure!(
                meta.uid() == uid && (!private || meta.mode() & 0o077 == 0),
                "state directory must be private and user owned"
            );
        }
    }
    Ok(dir)
}

fn parent(path: &Path) -> Result<(File, CString)> {
    Ok((
        directory(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("missing parent"))?,
            false,
        )?,
        cstr(
            path.file_name()
                .ok_or_else(|| anyhow::anyhow!("missing filename"))?,
        )?,
    ))
}

fn check_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0,
        "state file must be private, user owned, regular and single linked"
    );
    Ok(())
}

fn linked(dir: &File, name: &CString, file: &File) -> Result<()> {
    check_file(file)?;
    let other = fd_result(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    check_file(&other)?;
    let a = file.metadata()?;
    let b = other.metadata()?;
    ensure!(
        a.dev() == b.dev() && a.ino() == b.ino(),
        "state file changed identity"
    );
    Ok(())
}

pub(crate) fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    let (dir, name) = parent(path)?;
    read_at(&dir, &name)
}

fn read_at(dir: &File, name: &CString) -> Result<Vec<u8>> {
    let file = fd_result(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    linked(dir, name, &file)?;
    ensure!(
        file.metadata()?.len() <= MAX_BYTES as u64,
        "state file exceeds 64 KiB"
    );
    let mut bytes = Vec::new();
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "state file exceeds 64 KiB");
    linked(dir, name, &file)?;
    Ok(bytes)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&read_bytes(path)?)?)
}

/// Compare the planned old bytes immediately before the atomic replacement.
pub(crate) fn write_bytes(path: &Path, bytes: &[u8], expected: Option<&[u8]>) -> Result<()> {
    let (dir, name) = parent(path)?;
    write_at(&dir, &name, bytes, expected)
}

pub(crate) fn write_at(
    dir: &File,
    name: &CString,
    bytes: &[u8],
    expected: Option<&[u8]>,
) -> Result<()> {
    ensure!(bytes.len() <= MAX_BYTES, "state file exceeds 64 KiB");
    let temp = CString::new(format!(".hive-{}.tmp", crate::nonce()?))?;
    let mut file = fd_result(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    })?;
    let result = (|| {
        check_file(&file)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        linked(dir, &temp, &file)?;
        match read_at(dir, name) {
            Ok(old) => ensure!(
                expected == Some(old.as_slice()),
                "state changed since planning"
            ),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                ensure!(expected.is_none(), "state disappeared since planning")
            }
            Err(error) => return Err(error),
        }
        if unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                temp.as_ptr(),
                dir.as_raw_fd(),
                name.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        dir.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0);
        }
    }
    result
}

pub(crate) fn existing_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    let (dir, name) = match parent(path) {
        Ok(value) => value,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    existing_at(&dir, &name)
}

pub(crate) fn existing_at(dir: &File, name: &CString) -> Result<Option<Vec<u8>>> {
    match read_at(dir, name) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let old = existing_bytes(path)?;
    write_bytes(path, &serde_json::to_vec(value)?, old.as_deref())
}

pub(crate) fn remove_bytes(path: &Path, expected: &[u8]) -> Result<()> {
    let (dir, name) = parent(path)?;
    remove_at(&dir, &name, expected)
}

pub(crate) fn remove_at(dir: &File, name: &CString, expected: &[u8]) -> Result<()> {
    ensure!(
        read_at(dir, name)? == expected,
        "state changed since planning"
    );
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    dir.sync_all()?;
    Ok(())
}

pub struct FileLock {
    file: File,
}

impl FileLock {
    pub fn acquire(path: &Path, shared: bool, wait: Duration) -> Result<Self> {
        let (dir, name) = parent(path)?;
        let file = fd_result(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NONBLOCK
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        linked(&dir, &name, &file)?;
        let start = Instant::now();
        loop {
            let rc = unsafe {
                libc::flock(
                    file.as_raw_fd(),
                    (if shared { libc::LOCK_SH } else { libc::LOCK_EX }) | libc::LOCK_NB,
                )
            };
            if rc == 0 {
                linked(&dir, &name, &file)?;
                return Ok(Self { file });
            }
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(libc::EWOULDBLOCK) | Some(libc::EINTR)
            ) {
                return Err(error.into());
            }
            if start.elapsed() >= wait {
                bail!("state lock wait expired");
            }
            std::thread::sleep(Duration::from_millis(10).min(wait.saturating_sub(start.elapsed())));
        }
    }

    /// Explicit opt-in only. Normal acquisition always sets close-on-exec.
    pub fn make_inheritable(&self) -> Result<()> {
        let flags = unsafe { libc::fcntl(self.as_raw_fd(), libc::F_GETFD) };
        ensure!(flags >= 0, "cannot read lock descriptor flags");
        if unsafe { libc::fcntl(self.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

// Closing the last inherited descriptor releases flock; an explicit LOCK_UN
// here would prematurely release a lock still held by a child.
impl AsRawFd for FileLock {
    fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}

pub fn lease_path(paths: &Paths, cycle_id: &str) -> Result<PathBuf> {
    ensure!(
        cycle_id.len() == 32
            && cycle_id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid cycle identity"
    );
    Ok(paths.leases.join(format!("{cycle_id}.json")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    pub(crate) fn fixture() -> tempfile::TempDir {
        let temp =
            tempfile::tempdir_in(std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap())
                .unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        temp
    }

    #[test]
    fn private_bounded_json_and_links() {
        let temp = fixture();
        let paths = Paths::in_root(temp.path().join("state"));
        paths.ensure().unwrap();
        write_json(&paths.status, &vec![1, 2]).unwrap();
        assert_eq!(read_json::<Vec<u8>>(&paths.status).unwrap(), [1, 2]);
        assert_eq!(
            std::fs::metadata(&paths.status).unwrap().mode() & 0o777,
            0o600
        );
        assert!(write_bytes(&paths.status, b"[]", Some(b"stale")).is_err());
        assert!(write_json(&paths.config, &"x".repeat(MAX_BYTES)).is_err());
        write_json(&paths.config, &0).unwrap();
        std::fs::write(&paths.config, vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(read_json::<serde_json::Value>(&paths.config).is_err());
        std::fs::remove_file(&paths.config).unwrap();
        std::fs::hard_link(&paths.status, paths.root.join("hard")).unwrap();
        assert!(read_bytes(&paths.status).is_err());
        symlink(&paths.status, &paths.config).unwrap();
        assert!(write_json(&paths.config, &1).is_err());
        assert!(FileLock::acquire(&paths.config, false, Duration::ZERO).is_err());
        assert!(FileLock::acquire(&paths.status, false, Duration::ZERO).is_err());
        std::fs::set_permissions(&paths.leases, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(paths.ensure().is_err());
    }

    #[test]
    fn lock_sharing_persistence_and_flags() {
        let temp = fixture();
        let path = temp.path().join("worker.lock");
        let first = FileLock::acquire(&path, true, Duration::ZERO).unwrap();
        let second = FileLock::acquire(&path, true, Duration::ZERO).unwrap();
        assert!(FileLock::acquire(&path, false, Duration::from_millis(20)).is_err());
        assert_ne!(
            unsafe { libc::fcntl(first.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        first.make_inheritable().unwrap();
        assert_eq!(
            unsafe { libc::fcntl(first.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        let inode = std::fs::metadata(&path).unwrap().ino();
        drop(first);
        drop(second);
        let _exclusive = FileLock::acquire(&path, false, Duration::ZERO).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    }

    #[test]
    fn fixture_child() {
        let Some(path) = std::env::var_os("HIVE_STATE_TEST_LOCK") else {
            return;
        };
        if let Some(fd) = std::env::var_os("HIVE_STATE_TEST_FD") {
            let fd: RawFd = fd.to_str().unwrap().parse().unwrap();
            assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
            println!("hive-lock-ready");
            std::io::stdout().flush().unwrap();
            let mut byte = [0];
            std::io::stdin().read_exact(&mut byte).unwrap();
        } else {
            assert!(FileLock::acquire(Path::new(&path), false, Duration::ZERO).is_err());
        }
    }

    #[test]
    fn separate_process_and_inherited_lifetime() {
        use std::{
            io::{BufRead, BufReader},
            os::unix::process::CommandExt,
            process::{Command, Stdio},
        };
        let temp = fixture();
        let path = temp.path().join("worker.lock");
        let lock = FileLock::acquire(&path, false, Duration::ZERO).unwrap();
        let mut probe = Command::new(std::env::current_exe().unwrap());
        probe
            .args(["--exact", "state::tests::fixture_child", "--nocapture"])
            .env("HIVE_STATE_TEST_LOCK", &path)
            .env_remove("HIVE_STATE_TEST_FD");
        assert!(probe.status().unwrap().success());
        let fd = lock.as_raw_fd();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "state::tests::fixture_child", "--nocapture"])
            .env("HIVE_STATE_TEST_LOCK", &path)
            .env("HIVE_STATE_TEST_FD", fd.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "fixture child exited before ready"
            );
            if line.contains("hive-lock-ready") {
                break;
            }
        }
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        drop(lock);
        assert!(FileLock::acquire(&path, false, Duration::ZERO).is_err());
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        assert!(FileLock::acquire(&path, false, Duration::ZERO).is_ok());
    }

    #[test]
    fn rejects_ancestor_symlinks_and_cycle_traversal() {
        let temp = fixture();
        symlink(temp.path(), temp.path().join("alias")).unwrap();
        assert!(
            Paths::in_root(temp.path().join("alias/state"))
                .ensure()
                .is_err()
        );
        let paths = Paths::in_root(temp.path().join("missing"));
        assert!(lease_path(&paths, &"a".repeat(32)).is_ok());
        assert!(lease_path(&paths, &"A".repeat(32)).is_err());
        assert!(lease_path(&paths, "../status").is_err());
        assert!(read_json::<serde_json::Value>(&paths.status).is_err());
        assert!(!paths.root.exists());
    }
}
