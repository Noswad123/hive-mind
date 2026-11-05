use anyhow::{Result, bail, ensure};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Component, Path},
    sync::Mutex,
    time::{Duration, Instant},
};

pub(super) const RECORD_LIMIT: usize = 64 * 1024;
const TOTAL_LIMIT: usize = 1024 * 1024;

pub(super) trait Sink: Send {
    fn append(&mut self, value: &Value) -> Result<()>;
}

struct FileSink {
    file: File,
    bytes: usize,
}

impl Drop for FileSink {
    fn drop(&mut self) {
        // The descriptor remains locked for the whole invocation, including failures.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl Sink for FileSink {
    fn append(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= RECORD_LIMIT, "journal record limit");
        ensure!(
            self.bytes + bytes.len() <= TOTAL_LIMIT,
            "journal size limit"
        );
        self.file.write_all(&bytes)?;
        self.file.sync_all()?;
        self.bytes += bytes.len();
        Ok(())
    }
}

pub(super) fn validate_path(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "journal must be an absolute new path");
    ensure!(
        path.components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "journal path must be canonical"
    );
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("journal parent required"))?;
    ensure!(
        parent.canonicalize().ok().as_deref() == Some(parent),
        "journal parents must be canonical and exist"
    );
    ensure!(
        std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "journal already exists or is unavailable; never reuse or replay"
    );
    Ok(())
}

pub(super) struct Journal {
    sink: Mutex<(Box<dyn Sink>, bool)>,
    started: Instant,
    #[cfg(test)]
    clock: Option<Box<dyn Fn() -> Instant + Send + Sync>>,
}

impl Journal {
    pub(super) fn create(path: &Path) -> Result<Self> {
        validate_path(path)?;
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| anyhow::anyhow!("journal exclusive creation failed"))?;
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "journal lock failed"
        );
        Ok(Self::new(Box::new(FileSink { file, bytes: 0 })))
    }

    pub(super) fn new(sink: Box<dyn Sink>) -> Self {
        Self {
            sink: Mutex::new((sink, false)),
            started: Instant::now(),
            #[cfg(test)]
            clock: None,
        }
    }

    #[cfg(test)]
    pub(super) fn set_clock(&mut self, clock: impl Fn() -> Instant + Send + Sync + 'static) {
        self.clock = Some(Box::new(clock));
    }

    pub(super) fn remaining(&self) -> Result<Duration> {
        let now = Instant::now();
        #[cfg(test)]
        let now = self.clock.as_ref().map_or(now, |clock| clock());
        let remaining = (self.started + Duration::from_secs(120))
            .checked_duration_since(now)
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                anyhow::anyhow!("inconclusive: admission/network budget expired; stop; no replay")
            })?;
        Ok(remaining)
    }

    pub(super) fn record(&self, value: Value) -> Result<()> {
        let mut state = self
            .sink
            .lock()
            .map_err(|_| anyhow::anyhow!("journal unavailable; stop, outcome may be unknown"))?;
        if state.1 {
            bail!("journal failed; stop, outcome may be unknown; do not replay");
        }
        if state.0.append(&value).is_err() {
            state.1 = true;
            bail!("journal append/sync failed; stop, outcome may be unknown; do not replay");
        }
        Ok(())
    }

    pub(super) fn request<T>(
        &self,
        stage: &'static str,
        operation: impl FnOnce(Duration) -> Result<T>,
    ) -> Result<T> {
        self.remaining()?;
        self.record(serde_json::json!({"stage":stage,"event":"attempted","ifNoDurableOutcome":"unknown; never replay"}))?;
        // Sync and native UI are not wall-clock bounded. Never admit a request
        // using the budget sampled before its durable attempt marker.
        let result = self.remaining().and_then(operation);
        self.record(serde_json::json!({"stage":stage,"event":"outcome","budgetExpired":self.remaining().is_err(),"result":if result.is_ok() { "responseReceived" } else { "unknownOrUnavailable; stop" }}))?;
        // Persist the outcome even if the request or this sync finished late.
        self.remaining()?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Never;
    impl Sink for Never {
        fn append(&mut self, _: &Value) -> Result<()> {
            panic!("no request after deadline")
        }
    }
    #[test]
    fn flow_deadline_prevents_request() {
        let mut journal = Journal::new(Box::new(Never));
        journal.started = Instant::now() - Duration::from_secs(121);
        assert!(
            journal
                .request("test", |_| -> Result<()> { panic!("no network") })
                .is_err()
        );
    }
}
