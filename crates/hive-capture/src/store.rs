//! Desktop-local inbound observations and delivery queue. Never opens the notes DB.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    BootstrapPolicy, CaptureJob, DeliveryState, DestinationBinding, GoogleTask, IngestReport,
};

const APPLICATION_ID: i64 = 0x48494350; // HICP, distinct from MindWeaver databases.
const SCHEMA_VERSION: i64 = 2;
const IDENTITY_BINDINGS_SCHEMA: &str = "CREATE TABLE identity_bindings (
    namespace TEXT PRIMARY KEY NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    UNIQUE (issuer, subject)
);";
const MAX_ID_BYTES: usize = 4096;
const PROVIDER_CHANGED_REASON: &str = "Google task changed after capture intent was frozen; recording a source receipt does not acknowledge the provider change";

pub struct CaptureStore {
    conn: Connection,
}

// Option<Value> alone cannot round-trip Some(Value::Null) through JSON.
#[derive(Serialize, Deserialize)]
struct StoredJob {
    job: CaptureJob,
    receipt_recorded: bool,
}

fn encode_job(job: &CaptureJob) -> Result<String> {
    Ok(serde_json::to_string(&StoredJob {
        job: job.clone(),
        receipt_recorded: job.receipt.is_some(),
    })?)
}

fn decode_job(json: &str) -> Result<CaptureJob> {
    let mut stored: StoredJob = serde_json::from_str(json).context("decode capture job")?;
    if stored.receipt_recorded && stored.job.receipt.is_none() {
        stored.job.receipt = Some(serde_json::Value::Null);
    }
    Ok(stored.job)
}

/// Hash an unambiguous, versioned encoding of the entire operator/provider tuple.
pub fn deterministic_capture_id(account: &str, list: &str, id: &str) -> Result<String> {
    for (name, value) in [("account key", account), ("list ID", list), ("task ID", id)] {
        validate_id(name, value)?;
    }
    let mut hash = Sha256::new();
    hash.update(b"hive-capture/google-tasks/v1\0");
    for value in [account, list, id] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    Ok(hash.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_id(name: &str, value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.len() <= MAX_ID_BYTES
            && !value.chars().any(char::is_control),
        "{name} must be nonblank, control-free, and at most {MAX_ID_BYTES} bytes"
    );
    Ok(())
}

// Check every component, not just the final filename: SQLite must not follow a
// symlink into a notes directory. Missing parents are deliberately not created.
fn checked_path(path: &Path) -> Result<(PathBuf, bool)> {
    ensure!(!path.as_os_str().is_empty(), "empty capture database path");
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = path.parent().context("database path has no parent")?;
    for ancestor in parent.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)
            .with_context(|| format!("capture database parent {}", ancestor.display()))?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "capture database parent must be a real directory"
        );
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            validate_file_metadata(&metadata)?;
            Ok((path, true))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((path, false)),
        Err(error) => Err(error.into()),
    }
}

fn validate_file_metadata(metadata: &fs::Metadata) -> Result<()> {
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "capture database must be a regular, non-symlink file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // geteuid has no side effects and returns the effective Unix identity.
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "capture database must be owned by the current user"
        );
        ensure!(
            metadata.nlink() == 1,
            "hard-linked capture database refused"
        );
        ensure!(
            metadata.mode() & 0o077 == 0,
            "capture database permissions must be owner-private (no group/other access)"
        );
    }
    Ok(())
}

// SQL reads may require writable hot-journal recovery. Identify the committed
// database format without SQLite before granting it any recovery/write access.
fn validate_header(path: &Path) -> Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).context("read capture database header")?;
    validate_file_metadata(&file.metadata()?)?;
    let mut header = [0; 100];
    file.read_exact(&mut header)
        .context("capture database header is truncated")?;
    ensure!(
        &header[..16] == b"SQLite format 3\0",
        "not a SQLite capture database"
    );
    let application_id = u32::from_be_bytes(header[68..72].try_into()?);
    let version = u32::from_be_bytes(header[60..64].try_into()?);
    ensure!(
        i64::from(application_id) == APPLICATION_ID,
        "not a Hive capture database"
    );
    ensure!(
        (1..=SCHEMA_VERSION).contains(&i64::from(version)),
        "unsupported capture schema version {version}"
    );
    Ok(())
}

fn validate_schema(conn: &Connection) -> Result<()> {
    let application_id: i64 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    ensure!(
        application_id == APPLICATION_ID,
        "not a Hive capture database"
    );
    ensure!(
        (1..=SCHEMA_VERSION).contains(&version),
        "unsupported capture schema version {version}"
    );
    // Verify the tables before enabling any writable connection settings.
    conn.prepare("SELECT account_key, list_id, policy FROM connections")?;
    conn.prepare("SELECT capture_id, account_key, list_id, provider_id, job FROM jobs")?;
    if version == 2 {
        conn.prepare("SELECT namespace, issuer, subject FROM identity_bindings")?;
    }
    Ok(())
}

impl CaptureStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_impl(path.as_ref(), true)
    }

    /// Open an identified queue, allowing SQLite crash recovery but never creation.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_impl(path.as_ref(), false)
    }

    fn open_impl(path: &Path, allow_create: bool) -> Result<Self> {
        let (path, exists) = checked_path(path)?;
        if exists {
            validate_header(&path)?;
        } else {
            ensure!(allow_create, "capture database does not exist");
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(&path)
                .context("create private capture database")?;
        }
        let mut conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;
        if exists {
            validate_schema(&conn)?;
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        if !exists {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "CREATE TABLE connections (
                    account_key TEXT NOT NULL,
                    list_id TEXT NOT NULL,
                    policy TEXT NOT NULL,
                    PRIMARY KEY (account_key, list_id)
                );
                CREATE TABLE jobs (
                    capture_id TEXT PRIMARY KEY NOT NULL,
                    account_key TEXT NOT NULL,
                    list_id TEXT NOT NULL,
                    provider_id TEXT NOT NULL,
                    job TEXT NOT NULL,
                    UNIQUE (account_key, list_id, provider_id)
                );",
            )?;
            tx.execute_batch(IDENTITY_BINDINGS_SCHEMA)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        } else {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Recheck under the writer lock in case another opener migrated first.
            validate_schema(&tx)?;
            let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if version == 1 {
                tx.execute_batch(IDENTITY_BINDINGS_SCHEMA)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    /// Status never creates a database, journal, schema, or missing directory.
    pub fn inspect(path: impl AsRef<Path>) -> Result<Vec<CaptureJob>> {
        let (path, exists) = checked_path(path.as_ref())?;
        ensure!(exists, "capture database does not exist");
        validate_header(&path)?;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        if let Err(error) = validate_schema(&conn) {
            if matches!(
                error.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(code, _))
                    if code.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK
            ) {
                return Err(error.context("capture database has a hot rollback journal; recover explicitly with CaptureStore::open_existing before inspecting"));
            }
            return Err(error);
        }
        read_jobs(&conn)
    }

    /// Stage only complete provider snapshots. Absence never implies deletion.
    /// The first policy is durable per (operator account namespace, list ID).
    pub fn ingest(
        &mut self,
        account_key: &str,
        list_id: &str,
        tasks: &[GoogleTask],
        bootstrap: BootstrapPolicy,
    ) -> Result<IngestReport> {
        ensure!(
            !account_key.starts_with(hive_core::google_identity::VERIFIED_NAMESPACE_PREFIX),
            "verified Google namespaces require ingest_verified"
        );
        self.ingest_common(account_key, list_id, tasks, bootstrap, None)
    }

    /// Stage a verified subject's snapshot without accepting an operator alias.
    /// Only the issuer and subject binding is persisted, never OAuth credentials.
    pub fn ingest_verified(
        &mut self,
        identity: &hive_core::GoogleIdentity,
        list_id: &str,
        tasks: &[GoogleTask],
        bootstrap: BootstrapPolicy,
    ) -> Result<IngestReport> {
        let account_key = identity.namespace()?;
        self.ingest_common(&account_key, list_id, tasks, bootstrap, Some(identity))
    }

    fn ingest_common(
        &mut self,
        account_key: &str,
        list_id: &str,
        tasks: &[GoogleTask],
        bootstrap: BootstrapPolicy,
        identity: Option<&hive_core::GoogleIdentity>,
    ) -> Result<IngestReport> {
        validate_id("account key", account_key)?;
        validate_id("list ID", list_id)?;
        let mut snapshot = BTreeMap::new();
        for task in tasks {
            validate_id("task ID", &task.id)?;
            if let Some(previous) = snapshot.insert(task.id.as_str(), task) {
                ensure!(
                    previous == task,
                    "conflicting duplicate Google task ID {}",
                    task.id
                );
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Even an empty legacy baseline cannot prove which subject owns a list.
        // Do not create a second capture namespace without explicit migration.
        let mixed: bool = tx.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM connections c
                LEFT JOIN identity_bindings b ON b.namespace = c.account_key
                WHERE c.list_id = ?1 AND (b.namespace IS NOT NULL) != ?2
            )",
            params![list_id, identity.is_some()],
            |row| row.get(0),
        )?;
        ensure!(
            !mixed,
            "mixing verified and legacy connections for the same list requires explicit legacy binding migration"
        );
        if let Some(identity) = identity {
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT issuer, subject FROM identity_bindings WHERE namespace = ?1",
                    [account_key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((issuer, subject)) = existing {
                ensure!(
                    issuer == identity.issuer() && subject == identity.subject(),
                    "verified Google namespace binding collision"
                );
            } else {
                let populated: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM connections WHERE account_key = ?1)
                        OR EXISTS (SELECT 1 FROM jobs WHERE account_key = ?1)",
                    [account_key],
                    |row| row.get(0),
                )?;
                ensure!(
                    !populated,
                    "unbound reserved Google namespace requires explicit legacy binding migration"
                );
                tx.execute(
                    "INSERT INTO identity_bindings (namespace, issuer, subject) VALUES (?1, ?2, ?3)",
                    params![account_key, identity.issuer(), identity.subject()],
                )?;
            }
        }
        let first = tx
            .query_row(
                "SELECT policy FROM connections WHERE account_key = ?1 AND list_id = ?2",
                params![account_key, list_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_none();
        if first {
            tx.execute(
                "INSERT INTO connections (account_key, list_id, policy) VALUES (?1, ?2, ?3)",
                params![account_key, list_id, serde_json::to_string(&bootstrap)?],
            )?;
        }
        let mut report = IngestReport::default();
        for task in snapshot.into_values() {
            report.observed += 1;
            let id = deterministic_capture_id(account_key, list_id, &task.id)?;
            let existing = load_job(&tx, &id)?;
            // Both uniqueness constraints matter, including an unexpected hash
            // collision or a tuple stored with a different ID by a broken writer.
            let tuple_id: Option<String> = tx.query_row(
                "SELECT capture_id FROM jobs WHERE account_key = ?1 AND list_id = ?2 AND provider_id = ?3",
                params![account_key, list_id, task.id], |row| row.get(0),
            ).optional()?;
            ensure!(
                tuple_id.as_ref().is_none_or(|stored| stored == &id),
                "external tuple has a different capture ID"
            );
            if let Some(mut job) = existing {
                ensure!(
                    job.account_key == account_key
                        && job.list_id == list_id
                        && job.provider_id == task.id,
                    "capture ID collision"
                );
                if meaningful_change(&job.observation, task)
                    && matches!(
                        job.state,
                        DeliveryState::Pending
                            | DeliveryState::Delivering
                            | DeliveryState::Delivered
                            | DeliveryState::NeedsReview
                    )
                {
                    job.provider_changed = true;
                    job.state = DeliveryState::NeedsReview;
                    job.last_error = Some(PROVIDER_CHANGED_REASON.into());
                    report.needs_review += 1;
                } else {
                    report.unchanged += 1;
                }
                job.observation = task.clone();
                write_job(&tx, &job)?;
            } else {
                let mut state = initial_state(task);
                if state == DeliveryState::Pending
                    && first
                    && bootstrap == BootstrapPolicy::Baseline
                {
                    state = DeliveryState::Baseline;
                }
                match state {
                    DeliveryState::Pending => report.staged += 1,
                    DeliveryState::Baseline => report.baselined += 1,
                    DeliveryState::CalendarPending => report.deferred += 1,
                    DeliveryState::Unsupported => report.needs_review += 1,
                    _ => report.unchanged += 1,
                }
                let job = CaptureJob {
                    capture_id: id,
                    account_key: account_key.into(),
                    list_id: list_id.into(),
                    provider_id: task.id.clone(),
                    title: task.title.clone(),
                    state,
                    attempts: 0,
                    binding: None,
                    receipt: None,
                    last_error: (state == DeliveryState::Unsupported)
                        .then(|| unsupported_reason(task).unwrap().into()),
                    provider_changed: false,
                    observation: task.clone(),
                };
                tx.execute(
                    "INSERT INTO jobs (capture_id, account_key, list_id, provider_id, job) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![job.capture_id, job.account_key, job.list_id, job.provider_id, encode_job(&job)?],
                )?;
            }
        }
        tx.commit()?;
        Ok(report)
    }

    pub fn jobs(&self) -> Result<Vec<CaptureJob>> {
        read_jobs(&self.conn)
    }

    pub fn claim(&mut self, capture_id: &str, binding: &DestinationBinding) -> Result<CaptureJob> {
        binding.validate()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job = load_job(&tx, capture_id)?.context("unknown capture ID")?;
        ensure!(
            job.binding
                .as_ref()
                .is_none_or(|previous| previous == binding),
            "capture destination is already frozen to a different binding"
        );
        ensure!(
            job.state == DeliveryState::Pending,
            "only pending captures can be claimed"
        );
        job.attempts = job
            .attempts
            .checked_add(1)
            .context("capture attempt count overflow")?;
        job.binding = Some(binding.clone());
        job.state = DeliveryState::Delivering;
        write_job(&tx, &job)?;
        tx.commit()?;
        Ok(job)
    }

    /// Reconciliation may record a receipt, but must never call the source writer
    /// again. A receipt confirms creation, not acceptance of a provider change.
    /// An existing receipt is immutable even after provider-side changes.
    pub fn complete(&mut self, id: &str, receipt: &serde_json::Value) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job = load_job(&tx, id)?.context("unknown capture ID")?;
        if let Some(previous) = &job.receipt {
            ensure!(
                previous == receipt,
                "capture already has a different receipt"
            );
            return Ok(());
        }
        ensure!(
            matches!(
                job.state,
                DeliveryState::Delivering | DeliveryState::NeedsReview
            ),
            "only delivering or review captures can be completed"
        );
        job.receipt = Some(receipt.clone());
        if job.provider_changed {
            job.state = DeliveryState::NeedsReview;
            job.last_error = Some(PROVIDER_CHANGED_REASON.into());
        } else {
            job.state = DeliveryState::Delivered;
            job.last_error = None;
        }
        write_job(&tx, &job)?;
        tx.commit()?;
        Ok(())
    }

    pub fn block(&mut self, id: &str, reason: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job = load_job(&tx, id)?.context("unknown capture ID")?;
        // A poll can have blocked an in-flight writer before its failure arrives.
        // Only this known race is idempotent; other invalid transitions still fail.
        if job.state == DeliveryState::NeedsReview && job.provider_changed {
            return Ok(());
        }
        ensure!(
            matches!(
                job.state,
                DeliveryState::Pending | DeliveryState::Delivering
            ),
            "only pending or delivering captures can be blocked"
        );
        job.state = DeliveryState::NeedsReview;
        job.last_error = Some(reason.into());
        write_job(&tx, &job)?;
        tx.commit()?;
        Ok(())
    }
}

fn initial_state(task: &GoogleTask) -> DeliveryState {
    if task.deleted || task.hidden || task.status == "completed" {
        DeliveryState::Ignored
    } else if unsupported_reason(task).is_some() {
        DeliveryState::Unsupported
    } else if task.due.is_some() {
        DeliveryState::CalendarPending
    } else {
        DeliveryState::Pending
    }
}

fn unsupported_reason(task: &GoogleTask) -> Option<&'static str> {
    if task.status != "needsAction" {
        return Some("unsupported Google task status");
    }
    if task.title.trim().is_empty()
        || task
            .title
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Some("capture title must be nonblank and single-line without controls");
    }
    if ["mw-task", "mw-dashboard", "mw-archive"]
        .iter()
        .any(|marker| task.title.contains(marker))
    {
        return Some("capture title contains a reserved mw-task/mw-dashboard/mw-archive marker");
    }
    let title = task.title.trim();
    let checkbox = if title.starts_with(['-', '*', '+']) {
        Some(&title[1..])
    } else {
        let rest = title.trim_start_matches(|c: char| c.is_ascii_digit());
        if rest.len() < title.len() {
            rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))
        } else {
            None
        }
    };
    if checkbox.is_some_and(|rest| {
        let rest = rest.trim_start();
        ["[ ]", "[x]", "[X]"].iter().any(|prefix| {
            rest.strip_prefix(prefix)
                .is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
        })
    }) {
        return Some("capture title has nested checkbox syntax");
    }
    // mw treats these tokens as metadata, so its displayed text is not literal.
    if title.split_whitespace().any(|token| {
        token
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("area:"))
    }) {
        return Some(
            "inline area: metadata in capture titles is unsupported without literal-title evidence",
        );
    }
    if task.notes.as_ref().is_some_and(|v| !v.is_empty()) {
        return Some("Google task notes cannot be flattened into a capture title");
    }
    if task.parent.as_ref().is_some_and(|v| !v.is_empty()) {
        return Some("Google subtasks are unsupported for automatic capture");
    }
    if task.assignment_info.as_ref().is_some_and(|v| !v.is_null()) {
        return Some("assigned Google tasks are unsupported for automatic capture");
    }
    None
}

fn meaningful_change(old: &GoogleTask, new: &GoogleTask) -> bool {
    old.title != new.title
        || old.due != new.due
        || old.status != new.status
        || old.notes != new.notes
        || old.parent != new.parent
        || old.assignment_info != new.assignment_info
        || old.deleted != new.deleted
        || old.hidden != new.hidden
}

fn load_job(conn: &Connection, id: &str) -> Result<Option<CaptureJob>> {
    let json: Option<String> = conn
        .query_row("SELECT job FROM jobs WHERE capture_id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    json.map(|json| decode_job(&json)).transpose()
}

fn read_jobs(conn: &Connection) -> Result<Vec<CaptureJob>> {
    let mut stmt = conn.prepare("SELECT job FROM jobs ORDER BY capture_id")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|row| decode_job(&row?)).collect()
}

fn write_job(conn: &Connection, job: &CaptureJob) -> Result<()> {
    let changed = conn.execute(
        "UPDATE jobs SET job = ?1 WHERE capture_id = ?2 AND account_key = ?3 AND list_id = ?4 AND provider_id = ?5",
        params![encode_job(job)?, job.capture_id, job.account_key, job.list_id, job.provider_id],
    )?;
    if changed != 1 {
        bail!("capture identity changed or disappeared");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn setup() -> (TempDir, CaptureStore) {
        let dir = tempfile::tempdir().unwrap();
        // macOS's /var and /tmp aliases are symlinks; use the actual directory.
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let store = CaptureStore::open(path).unwrap();
        (dir, store)
    }

    fn task(id: &str) -> GoogleTask {
        GoogleTask {
            id: id.into(),
            title: "Buy milk".into(),
            status: "needsAction".into(),
            ..Default::default()
        }
    }

    fn binding() -> DestinationBinding {
        DestinationBinding {
            notes_dir: "/notes".into(),
            inbox_path: "/notes/inbox.md".into(),
            dashboard_path: "/notes/dashboard.md".into(),
        }
    }

    fn ingest(store: &mut CaptureStore, tasks: &[GoogleTask]) -> IngestReport {
        store
            .ingest("operator", "inbox", tasks, BootstrapPolicy::ImportOpen)
            .unwrap()
    }

    fn make_v1(store: &mut CaptureStore) {
        let tx = store.conn.transaction().unwrap();
        tx.execute_batch("DROP TABLE identity_bindings; PRAGMA user_version=1;")
            .unwrap();
        tx.commit().unwrap();
    }

    fn row_count(store: &CaptureStore, table: &str) -> i64 {
        store
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn verified_subject_binding_persists_without_aliases_or_duplicate_jobs() {
        let (dir, mut store) = setup();
        let identity = hive_core::GoogleIdentity::new("subject-one").unwrap();
        let namespace = identity.namespace().unwrap();
        let report = store
            .ingest_verified(
                &identity,
                "inbox",
                &[task("1")],
                BootstrapPolicy::ImportOpen,
            )
            .unwrap();
        assert_eq!(report.staged, 1);
        let before = store.jobs().unwrap();
        assert_eq!(before[0].account_key, namespace);
        assert_eq!(
            before[0].capture_id,
            deterministic_capture_id(&namespace, "inbox", "1").unwrap()
        );
        drop(store);
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let mut store = CaptureStore::open_existing(&path).unwrap();
        let same_subject = hive_core::GoogleIdentity::new("subject-one").unwrap();
        assert_eq!(same_subject.namespace().unwrap(), namespace);
        let report = store
            .ingest_verified(
                &same_subject,
                "inbox",
                &[task("1")],
                BootstrapPolicy::Baseline,
            )
            .unwrap();
        assert_eq!(report.unchanged, 1);
        assert_eq!(store.jobs().unwrap(), before);
        let persisted: (String, String, String) = store
            .conn
            .query_row(
                "SELECT namespace, issuer, subject FROM identity_bindings",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            persisted,
            (
                namespace,
                identity.issuer().into(),
                identity.subject().into()
            )
        );
        assert_eq!(row_count(&store, "connections"), 1);
        assert_eq!(row_count(&store, "identity_bindings"), 1);
    }

    #[test]
    fn legacy_ingest_and_invalid_identities_cannot_enter_verified_namespace() {
        let (_dir, mut store) = setup();
        let identity = hive_core::GoogleIdentity::new("subject-one").unwrap();
        for key in [
            identity.namespace().unwrap(),
            "google-sub-v1:operator".into(),
            "google-sub-v1:".into(),
        ] {
            assert!(
                store
                    .ingest(&key, "inbox", &[task("1")], BootstrapPolicy::ImportOpen)
                    .is_err()
            );
        }
        let invalid: hive_core::GoogleIdentity = serde_json::from_value(json!({
            "issuer": "https://evil.invalid", "subject": "subject-one"
        }))
        .unwrap();
        assert!(
            store
                .ingest_verified(&invalid, "inbox", &[task("1")], BootstrapPolicy::ImportOpen)
                .is_err()
        );
        for table in ["connections", "jobs", "identity_bindings"] {
            assert_eq!(row_count(&store, table), 0);
        }
    }

    #[test]
    fn mismatched_or_duplicate_identity_bindings_fail_before_snapshot_writes() {
        for corruption in ["issuer", "subject", "namespace"] {
            let (_dir, mut store) = setup();
            let identity = hive_core::GoogleIdentity::new("subject-one").unwrap();
            store
                .ingest_verified(
                    &identity,
                    "inbox",
                    &[task("1")],
                    BootstrapPolicy::ImportOpen,
                )
                .unwrap();
            let before = store.jobs().unwrap();
            let (namespace, issuer, subject) = match corruption {
                "issuer" => (
                    identity.namespace().unwrap(),
                    "https://evil.invalid",
                    identity.subject(),
                ),
                "subject" => (
                    identity.namespace().unwrap(),
                    identity.issuer(),
                    "different-subject",
                ),
                _ => (
                    "google-sub-v1:wrong-namespace".into(),
                    identity.issuer(),
                    identity.subject(),
                ),
            };
            store
                .conn
                .execute(
                    "UPDATE identity_bindings SET namespace=?1, issuer=?2, subject=?3",
                    params![namespace, issuer, subject],
                )
                .unwrap();
            let mut changed = task("1");
            changed.title = "Changed".into();
            assert!(
                store
                    .ingest_verified(
                        &identity,
                        "inbox",
                        &[changed, task("2")],
                        BootstrapPolicy::Baseline,
                    )
                    .is_err()
            );
            assert!(
                store
                    .ingest_verified(
                        &identity,
                        "new-list",
                        &[task("3")],
                        BootstrapPolicy::Baseline,
                    )
                    .is_err()
            );
            assert_eq!(store.jobs().unwrap(), before);
            assert_eq!(row_count(&store, "connections"), 1);
            assert_eq!(row_count(&store, "identity_bindings"), 1);
        }
        // The issuer/subject unique constraint also rejects a second namespace
        // when there are no old observations to trigger the adoption guard.
        let (_dir, mut store) = setup();
        let identity = hive_core::GoogleIdentity::new("subject-one").unwrap();
        store
            .conn
            .execute(
                "INSERT INTO identity_bindings VALUES ('other', ?1, ?2)",
                params![identity.issuer(), identity.subject()],
            )
            .unwrap();
        assert!(
            store
                .ingest_verified(
                    &identity,
                    "inbox",
                    &[task("1")],
                    BootstrapPolicy::ImportOpen
                )
                .is_err()
        );
        assert_eq!(row_count(&store, "connections"), 0);
        assert_eq!(row_count(&store, "jobs"), 0);
    }

    #[test]
    fn verified_snapshot_failure_rolls_back_binding_bootstrap_and_observations() {
        let (_dir, mut store) = setup();
        let identity = hive_core::GoogleIdentity::new("subject-one").unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_second BEFORE INSERT ON jobs
                 WHEN NEW.provider_id = '2' BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
        assert!(
            store
                .ingest_verified(
                    &identity,
                    "inbox",
                    &[task("1"), task("2")],
                    BootstrapPolicy::Baseline,
                )
                .is_err()
        );
        for table in ["connections", "jobs", "identity_bindings"] {
            assert_eq!(row_count(&store, table), 0);
        }
        let mut conflict = task("1");
        conflict.title = "Conflicting duplicate".into();
        assert!(
            store
                .ingest_verified(
                    &identity,
                    "inbox",
                    &[task("1"), conflict],
                    BootstrapPolicy::Baseline,
                )
                .is_err()
        );
        assert_eq!(row_count(&store, "identity_bindings"), 0);
        store
            .conn
            .execute_batch("DROP TRIGGER reject_second")
            .unwrap();
        let report = store
            .ingest_verified(
                &identity,
                "inbox",
                &[task("1")],
                BootstrapPolicy::ImportOpen,
            )
            .unwrap();
        assert_eq!(report.staged, 1);
        let before = store.jobs().unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_second BEFORE INSERT ON jobs
                 WHEN NEW.provider_id = '2' BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
        let mut changed = task("1");
        changed.title = "Changed".into();
        assert!(
            store
                .ingest_verified(
                    &identity,
                    "inbox",
                    &[changed, task("2")],
                    BootstrapPolicy::ImportOpen,
                )
                .is_err()
        );
        assert_eq!(store.jobs().unwrap(), before);
        assert_eq!(row_count(&store, "connections"), 1);
        assert_eq!(row_count(&store, "identity_bindings"), 1);
    }

    #[test]
    fn v1_reserved_namespace_requires_explicit_binding_migration() {
        for keep in ["connections", "jobs", "both"] {
            let (dir, mut store) = setup();
            let identity = hive_core::GoogleIdentity::new("subject-one").unwrap();
            // Simulate a v1 operator key that happens to equal the verified hash.
            store
                .ingest_verified(
                    &identity,
                    "inbox",
                    &[task("1")],
                    BootstrapPolicy::ImportOpen,
                )
                .unwrap();
            make_v1(&mut store);
            if keep == "connections" {
                store.conn.execute("DELETE FROM jobs", []).unwrap();
            } else if keep == "jobs" {
                store.conn.execute("DELETE FROM connections", []).unwrap();
            }
            let before = store.jobs().unwrap();
            let connections = row_count(&store, "connections");
            drop(store);
            let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
            let mut store = CaptureStore::open_existing(path).unwrap();
            let error = store
                .ingest_verified(
                    &identity,
                    "new-list",
                    &[task("2")],
                    BootstrapPolicy::ImportOpen,
                )
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("explicit legacy binding migration")
            );
            assert_eq!(store.jobs().unwrap(), before);
            assert_eq!(row_count(&store, "connections"), connections);
            assert_eq!(row_count(&store, "identity_bindings"), 0);
        }
    }

    #[test]
    fn upgraded_v1_operator_list_cannot_be_automatically_bound() {
        let (dir, mut store) = setup();
        store
            .ingest(
                "personal-google",
                "L",
                &[task("1")],
                BootstrapPolicy::ImportOpen,
            )
            .unwrap();
        make_v1(&mut store);
        let before = store.jobs().unwrap();
        drop(store);
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let mut store = CaptureStore::open_existing(path).unwrap();
        let identity = hive_core::GoogleIdentity::new("contrived-subject").unwrap();
        let error = store
            .ingest_verified(&identity, "L", &[task("1")], BootstrapPolicy::ImportOpen)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("explicit legacy binding migration")
        );
        assert_eq!(store.jobs().unwrap(), before);
        assert_eq!(row_count(&store, "connections"), 1);
        assert_eq!(row_count(&store, "identity_bindings"), 0);
    }

    #[test]
    fn legacy_list_blocks_verified_cutover_even_for_empty_baseline() {
        for tasks in [vec![], vec![task("1")]] {
            let (_dir, mut store) = setup();
            store
                .ingest("personal-google", "L", &tasks, BootstrapPolicy::Baseline)
                .unwrap();
            let before = store.jobs().unwrap();
            let identity = hive_core::GoogleIdentity::new("contrived-subject").unwrap();
            assert!(
                store
                    .ingest_verified(&identity, "L", &[task("1")], BootstrapPolicy::ImportOpen)
                    .is_err()
            );
            assert_eq!(store.jobs().unwrap(), before);
            assert_eq!(row_count(&store, "connections"), 1);
            assert_eq!(row_count(&store, "identity_bindings"), 0);
            // The rejected OAuth mapping does not disturb the legacy policy.
            assert_eq!(
                store
                    .ingest(
                        "personal-google",
                        "L",
                        &[task("later")],
                        BootstrapPolicy::Baseline
                    )
                    .unwrap()
                    .staged,
                1
            );
        }
    }

    #[test]
    fn verified_list_blocks_creation_and_update_of_legacy_shadow() {
        let (_dir, mut store) = setup();
        let identity = hive_core::GoogleIdentity::new("contrived-subject").unwrap();
        store
            .ingest_verified(&identity, "L", &[task("1")], BootstrapPolicy::ImportOpen)
            .unwrap();
        let before = store.jobs().unwrap();
        for existing_shadow in [false, true] {
            if existing_shadow {
                // Simulate an already mixed queue from an older writer.
                store.conn.execute(
                    "INSERT INTO connections (account_key, list_id, policy) VALUES (?1, ?2, ?3)",
                    params!["env-operator", "L", serde_json::to_string(&BootstrapPolicy::Baseline).unwrap()],
                ).unwrap();
            }
            let connections = row_count(&store, "connections");
            assert!(
                store
                    .ingest(
                        "env-operator",
                        "L",
                        &[task("1"), task("2")],
                        BootstrapPolicy::ImportOpen
                    )
                    .is_err()
            );
            assert_eq!(store.jobs().unwrap(), before);
            assert_eq!(row_count(&store, "connections"), connections);
            assert_eq!(row_count(&store, "identity_bindings"), 1);
        }
    }

    #[test]
    fn separate_lists_and_multiple_verified_subjects_can_coexist() {
        let (_dir, mut store) = setup();
        store
            .ingest(
                "personal-google",
                "legacy-list",
                &[task("1")],
                BootstrapPolicy::ImportOpen,
            )
            .unwrap();
        for subject in ["contrived-subject-one", "contrived-subject-two"] {
            let identity = hive_core::GoogleIdentity::new(subject).unwrap();
            assert_eq!(
                store
                    .ingest_verified(&identity, "L", &[task("1")], BootstrapPolicy::ImportOpen)
                    .unwrap()
                    .staged,
                1
            );
            let same_subject = hive_core::GoogleIdentity::new(subject).unwrap();
            assert_eq!(
                store
                    .ingest_verified(&same_subject, "L", &[task("1")], BootstrapPolicy::Baseline)
                    .unwrap()
                    .unchanged,
                1
            );
        }
        store
            .ingest(
                "env-operator",
                "another-list",
                &[task("1")],
                BootstrapPolicy::Baseline,
            )
            .unwrap();
        assert_eq!(row_count(&store, "connections"), 4);
        assert_eq!(row_count(&store, "jobs"), 4);
        assert_eq!(row_count(&store, "identity_bindings"), 2);
    }

    #[test]
    fn v1_inspection_is_read_only_and_write_upgrade_preserves_history() {
        let (dir, mut store) = setup();
        store
            .ingest(
                "operator",
                "inbox",
                &[task("baseline")],
                BootstrapPolicy::Baseline,
            )
            .unwrap();
        ingest(&mut store, &[task("delivered"), task("inflight")]);
        let delivered = deterministic_capture_id("operator", "inbox", "delivered").unwrap();
        let inflight = deterministic_capture_id("operator", "inbox", "inflight").unwrap();
        store.claim(&delivered, &binding()).unwrap();
        store
            .complete(&delivered, &json!({"stableId": delivered}))
            .unwrap();
        store.claim(&inflight, &binding()).unwrap();
        make_v1(&mut store);
        let raw_jobs: Vec<(String, String)> = store
            .conn
            .prepare("SELECT capture_id, job FROM jobs ORDER BY capture_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        let before_jobs = store.jobs().unwrap();
        drop(store);
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let before = fs::read(&path).unwrap();
        assert_eq!(CaptureStore::inspect(&path).unwrap(), before_jobs);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        let mut store = CaptureStore::open_existing(&path).unwrap();
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(row_count(&store, "identity_bindings"), 0);
        let upgraded_raw: Vec<(String, String)> = store
            .conn
            .prepare("SELECT capture_id, job FROM jobs ORDER BY capture_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(upgraded_raw, raw_jobs);
        assert_eq!(store.jobs().unwrap(), before_jobs);
        assert!(store.claim(&inflight, &binding()).is_err());
        assert!(store.claim(&delivered, &binding()).is_err());
        let report = store
            .ingest(
                "operator",
                "inbox",
                &[task("later")],
                BootstrapPolicy::Baseline,
            )
            .unwrap();
        assert_eq!(report.staged, 1);
        let policy: String = store
            .conn
            .query_row("SELECT policy FROM connections", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            policy,
            serde_json::to_string(&BootstrapPolicy::Baseline).unwrap()
        );
        let identity = hive_core::GoogleIdentity::new("subject-one").unwrap();
        let report = store
            .ingest_verified(
                &identity,
                "verified-list",
                &[task("verified")],
                BootstrapPolicy::Baseline,
            )
            .unwrap();
        assert_eq!(report.baselined, 1);
    }

    #[test]
    fn v1_hot_journal_is_recovered_before_upgrade_without_retrying_claims() {
        let (dir, mut store) = setup();
        ingest(&mut store, &[task("inflight")]);
        let id = store.jobs().unwrap()[0].capture_id.clone();
        store.claim(&id, &binding()).unwrap();
        make_v1(&mut store);
        let before_jobs = store.jobs().unwrap();
        drop(store);
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let journal = make_hot_journal(&path, true);
        let before = fs::read(&path).unwrap();
        let journal_before = fs::read(&journal).unwrap();
        assert!(
            CaptureStore::inspect(&path)
                .unwrap_err()
                .to_string()
                .contains("hot rollback journal")
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(&journal).unwrap(), journal_before);
        let mut store = CaptureStore::open_existing(&path).unwrap();
        assert_eq!(store.jobs().unwrap(), before_jobs);
        assert_eq!(row_count(&store, "identity_bindings"), 0);
        assert!(!journal.exists());
        assert!(store.claim(&id, &binding()).is_err());
        assert_eq!(store.jobs().unwrap()[0].attempts, 1);
    }

    #[test]
    fn identical_polls_reopen_and_absence_preserve_pending() {
        let (dir, mut store) = setup();
        assert_eq!(ingest(&mut store, &[task("1")]).staged, 1);
        assert_eq!(ingest(&mut store, &[task("1"), task("1")]).observed, 1);
        ingest(&mut store, &[]);
        drop(store);
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let reopened = CaptureStore::open(&path).unwrap();
        let jobs = reopened.jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, DeliveryState::Pending);
        assert_eq!(CaptureStore::inspect(path).unwrap(), jobs);
    }

    #[test]
    fn first_policy_is_persistent_and_new_tasks_are_imported() {
        let (_dir, mut store) = setup();
        let report = store
            .ingest(
                "operator",
                "inbox",
                &[task("old")],
                BootstrapPolicy::Baseline,
            )
            .unwrap();
        assert_eq!(report.baselined, 1);
        ingest(&mut store, &[task("old"), task("new")]);
        let jobs = store.jobs().unwrap();
        assert_eq!(
            jobs.iter().find(|j| j.provider_id == "old").unwrap().state,
            DeliveryState::Baseline
        );
        assert_eq!(
            jobs.iter().find(|j| j.provider_id == "new").unwrap().state,
            DeliveryState::Pending
        );
        // Even a later Baseline request does not rebaseline an existing connection.
        let report = store
            .ingest(
                "operator",
                "inbox",
                &[task("newer")],
                BootstrapPolicy::Baseline,
            )
            .unwrap();
        assert_eq!(report.staged, 1);
        let report = store
            .ingest(
                "operator",
                "other-list",
                &[task("old")],
                BootstrapPolicy::ImportOpen,
            )
            .unwrap();
        assert_eq!(report.staged, 1);
    }

    #[test]
    fn identity_is_full_tuple_not_title_or_ambiguous_concatenation() {
        let (_dir, mut store) = setup();
        ingest(&mut store, &[task("1"), task("2")]);
        assert_eq!(store.jobs().unwrap().len(), 2);
        let a = deterministic_capture_id("a", "bc", "d").unwrap();
        let b = deterministic_capture_id("ab", "c", "d").unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(
            a.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(a, deterministic_capture_id("a", "bc", "d").unwrap());
        for invalid in ["", " \t", "bad\nID"] {
            assert!(deterministic_capture_id("a", "b", invalid).is_err());
        }
        assert!(deterministic_capture_id("a", "b", &"x".repeat(MAX_ID_BYTES + 1)).is_err());
    }

    #[test]
    fn concurrent_claim_has_exactly_one_winner_and_no_restart_retry() {
        let (dir, mut store) = setup();
        ingest(&mut store, &[task("1")]);
        let id = store.jobs().unwrap()[0].capture_id.clone();
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let second = CaptureStore::open(&path).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = [store, second]
            .into_iter()
            .map(|mut store| {
                let id = id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.claim(&id, &binding()).is_ok()
                })
            })
            .collect();
        let wins = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(wins, 1);
        let mut store = CaptureStore::open(path).unwrap();
        assert!(store.claim(&id, &binding()).is_err());
        assert_eq!(store.jobs().unwrap()[0].attempts, 1);
        store.block(&id, "unknown writer outcome").unwrap();
        assert!(store.claim(&id, &binding()).is_err());
        store.complete(&id, &json!({"stableId": id})).unwrap();
        assert_eq!(store.jobs().unwrap()[0].state, DeliveryState::Delivered);
    }

    #[test]
    fn lifecycle_changes_freeze_intent_and_retain_receipts() {
        for lifecycle in [
            DeliveryState::Pending,
            DeliveryState::Delivering,
            DeliveryState::Delivered,
        ] {
            let (_dir, mut store) = setup();
            let original = task("1");
            ingest(&mut store, std::slice::from_ref(&original));
            let id = store.jobs().unwrap()[0].capture_id.clone();
            if lifecycle != DeliveryState::Pending {
                store.claim(&id, &binding()).unwrap();
            }
            if lifecycle == DeliveryState::Delivered {
                store.complete(&id, &json!({"line": 12})).unwrap();
            }
            let mut changed = original;
            changed.title = "Changed title".into();
            changed.deleted = true;
            ingest(&mut store, &[changed.clone()]);
            let job = store.jobs().unwrap().remove(0);
            assert_eq!(job.state, DeliveryState::NeedsReview);
            assert!(job.provider_changed);
            assert_eq!(job.title, "Buy milk");
            assert_eq!(job.observation, changed);
            assert_eq!(job.receipt.is_some(), lifecycle == DeliveryState::Delivered);
            assert!(store.claim(&id, &binding()).is_err());
        }
    }

    #[test]
    fn provider_edit_racing_with_completion_keeps_review_and_receipt_after_reopen() {
        let (dir, mut store) = setup();
        let mut observed = task("1");
        ingest(&mut store, &[observed.clone()]);
        let id = store.jobs().unwrap()[0].capture_id.clone();
        assert!(!store.claim(&id, &binding()).unwrap().provider_changed);
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let mut poller = CaptureStore::open(&path).unwrap();
        observed.title = "Edited on phone while writing".into();
        assert_eq!(ingest(&mut poller, &[observed.clone()]).needs_review, 1);
        let receipt = json!({"captureId": id, "text": "Buy milk"});
        store.complete(&id, &receipt).unwrap();
        let reviewed = store.jobs().unwrap().remove(0);
        assert_eq!(reviewed.state, DeliveryState::NeedsReview);
        assert!(reviewed.provider_changed);
        assert_eq!(reviewed.title, "Buy milk");
        assert_eq!(reviewed.observation, observed);
        assert_eq!(reviewed.receipt, Some(receipt.clone()));
        assert_eq!(reviewed.binding, Some(binding()));
        assert_eq!(reviewed.attempts, 1);
        assert_eq!(
            reviewed.last_error.as_deref(),
            Some(PROVIDER_CHANGED_REASON)
        );
        store
            .block(&id, "late writer outcome must not hide the provider edit")
            .unwrap();
        assert_eq!(store.jobs().unwrap()[0], reviewed);
        assert_eq!(ingest(&mut poller, &[observed]).unchanged, 1);
        store.complete(&id, &receipt).unwrap();
        assert!(store.complete(&id, &json!({"replacement": true})).is_err());
        assert_eq!(store.jobs().unwrap()[0], reviewed);
        drop(poller);
        drop(store);
        let mut store = CaptureStore::open(&path).unwrap();
        assert_eq!(store.jobs().unwrap()[0], reviewed);
        assert!(store.claim(&id, &binding()).is_err());
        assert_eq!(CaptureStore::inspect(path).unwrap()[0], reviewed);
    }

    #[test]
    fn unknown_writer_block_then_provider_edit_sets_sticky_change_flag() {
        let (_dir, mut store) = setup();
        let mut observed = task("1");
        ingest(&mut store, &[observed.clone()]);
        let id = store.jobs().unwrap()[0].capture_id.clone();
        store.claim(&id, &binding()).unwrap();
        store.block(&id, "unknown writer result").unwrap();
        assert!(!store.jobs().unwrap()[0].provider_changed);
        assert!(
            store
                .block(&id, "unrelated repeated block must fail")
                .is_err()
        );
        observed.notes = Some("Added prose after the failed attempt".into());
        assert_eq!(ingest(&mut store, &[observed.clone()]).needs_review, 1);
        assert!(store.jobs().unwrap()[0].provider_changed);
        let receipt = json!({"stableId": id});
        store.complete(&id, &receipt).unwrap();
        ingest(&mut store, &[observed]);
        let reviewed = store.jobs().unwrap().remove(0);
        assert!(reviewed.provider_changed);
        assert_eq!(reviewed.state, DeliveryState::NeedsReview);
        assert_eq!(reviewed.receipt, Some(receipt.clone()));
        assert_eq!(
            reviewed.last_error.as_deref(),
            Some(PROVIDER_CHANGED_REASON)
        );
        store.complete(&id, &receipt).unwrap();
        assert_eq!(store.jobs().unwrap()[0], reviewed);
    }

    #[test]
    fn unsupported_titles_are_observed_without_claiming_or_poisoning_bootstrap() {
        for title in [
            "<!-- mw-task:v1:bad -->",
            "Mention mw-task without a comment",
            "<!-- mw-dashboard -->",
            "<!-- mw-archive-end:v1 -->",
            "- [ ] nested",
            "*[x] nested",
            "+ [X] nested",
            "1. [ ] nested",
            "123)[x] nested",
            "- [ ]",
            "  - [ ] nested  ",
            "area:Code Ship feature",
            "Ship feature ArEa:Code",
            "Ship feature AREA: Code",
            "Ship feature area:",
        ] {
            let (_dir, mut store) = setup();
            let mut observed = task("unsafe");
            observed.title = title.into();
            observed
                .extra
                .insert("preserved".into(), json!({"title": title}));
            let report = ingest(&mut store, &[observed.clone(), task("safe")]);
            assert_eq!(report.staged, 1, "{title}");
            assert_eq!(report.needs_review, 1, "{title}");
            let job = store
                .jobs()
                .unwrap()
                .into_iter()
                .find(|job| job.provider_id == "unsafe")
                .unwrap();
            assert_eq!(job.state, DeliveryState::Unsupported, "{title}");
            assert_eq!(job.title, title);
            assert_eq!(job.observation, observed);
            assert!(job.last_error.is_some());
            assert_eq!(job.attempts, 0);
            assert!(!job.provider_changed);
            assert!(store.claim(&job.capture_id, &binding()).is_err());
            let report = store
                .ingest(
                    "operator",
                    "inbox",
                    &[task("later")],
                    BootstrapPolicy::Baseline,
                )
                .unwrap();
            assert_eq!(report.staged, 1);
        }
        for title in [
            "--help",
            "--inbox=elsewhere",
            "  Buy milk  ",
            "Discuss codearea:Code",
            "- [ ]not-checkbox",
            "[ ] plain text",
        ] {
            let (_dir, mut store) = setup();
            let mut observed = task("safe");
            observed.title = title.into();
            assert_eq!(ingest(&mut store, &[observed.clone()]).staged, 1, "{title}");
            let job = store.jobs().unwrap().remove(0);
            assert_eq!(job.state, DeliveryState::Pending, "{title}");
            assert_eq!(job.title, title);
            assert_eq!(job.observation, observed);
            assert!(job.last_error.is_none());
        }
    }

    #[test]
    fn all_meaningful_fields_block_but_etag_updated_and_extra_do_not() {
        for field in [
            "title",
            "due",
            "status",
            "notes",
            "parent",
            "assignmentInfo",
            "deleted",
            "hidden",
        ] {
            let (_dir, mut store) = setup();
            let original = task("1");
            ingest(&mut store, std::slice::from_ref(&original));
            let mut value = serde_json::to_value(&original).unwrap();
            value[field] = match field {
                "deleted" | "hidden" => json!(true),
                "assignmentInfo" => json!({"spaceInfo": {"space": "s"}}),
                _ => json!("changed"),
            };
            ingest(&mut store, &[serde_json::from_value(value).unwrap()]);
            assert_eq!(
                store.jobs().unwrap()[0].state,
                DeliveryState::NeedsReview,
                "{field}"
            );
        }
        let (_dir, mut store) = setup();
        let mut original = task("1");
        ingest(&mut store, &[original.clone()]);
        original.etag = Some("new etag".into());
        original.updated = Some("2026-10-30T00:00:00Z".into());
        original.extra.insert("position".into(), json!("002"));
        ingest(&mut store, &[original.clone()]);
        let job = store.jobs().unwrap().remove(0);
        assert_eq!(job.state, DeliveryState::Pending);
        assert_eq!(job.observation, original);
    }

    #[test]
    fn unsafe_tasks_never_flatten_and_deferred_never_resurrect() {
        let (_dir, mut store) = setup();
        let mut dated = task("date");
        dated.due = Some("2026-11-01T00:00:00.000Z".into());
        dated
            .extra
            .insert("unknownFutureMetadata".into(), json!({"nested": [1, 2]}));
        let mut notes = task("notes");
        notes.notes = Some("paragraph\nmore prose".into());
        let mut subtask = task("sub");
        subtask.parent = Some("p".into());
        let mut assigned = task("assigned");
        assigned.assignment_info = Some(json!({}));
        let mut unknown = task("unknown");
        unknown.status = "futureStatus".into();
        let mut multiline = task("multiline");
        multiline.title = "one\ntwo".into();
        let mut blank = task("blank");
        blank.title = "   ".into();
        let mut completed = task("completed");
        completed.status = "completed".into();
        let mut deleted = task("deleted");
        deleted.deleted = true;
        let mut hidden = task("hidden");
        hidden.hidden = true;
        ingest(
            &mut store,
            &[
                dated.clone(),
                notes,
                subtask,
                assigned,
                unknown,
                multiline,
                blank,
                completed,
                deleted,
                hidden,
            ],
        );
        let jobs = store.jobs().unwrap();
        let date_job = jobs.iter().find(|j| j.provider_id == "date").unwrap();
        assert_eq!(date_job.state, DeliveryState::CalendarPending);
        assert_eq!(date_job.observation, dated);
        assert_eq!(
            jobs.iter()
                .filter(|j| j.state == DeliveryState::Unsupported)
                .count(),
            6
        );
        assert_eq!(
            jobs.iter()
                .filter(|j| j.state == DeliveryState::Ignored)
                .count(),
            3
        );
        let reopened: Vec<_> = jobs.iter().map(|j| task(&j.provider_id)).collect();
        ingest(&mut store, &reopened);
        assert!(
            store
                .jobs()
                .unwrap()
                .iter()
                .all(|j| j.state != DeliveryState::Pending)
        );
    }

    #[test]
    fn conflicting_duplicates_rollback_snapshot_and_bootstrap_marker() {
        let (_dir, mut store) = setup();
        let mut conflict = task("2");
        conflict.title = "different".into();
        assert!(
            store
                .ingest(
                    "operator",
                    "inbox",
                    &[task("1"), task("2"), conflict],
                    BootstrapPolicy::Baseline
                )
                .is_err()
        );
        assert!(store.jobs().unwrap().is_empty());
        assert_eq!(ingest(&mut store, &[task("1")]).staged, 1);
        let before = store.jobs().unwrap();
        assert!(
            store
                .ingest(
                    "operator",
                    "inbox",
                    &[task("3"), task("bad\nID")],
                    BootstrapPolicy::ImportOpen
                )
                .is_err()
        );
        assert_eq!(store.jobs().unwrap(), before);
    }

    #[test]
    fn failed_staging_rolls_back_observations_and_first_policy() {
        let (_dir, mut store) = setup();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_second BEFORE INSERT ON jobs
             WHEN NEW.provider_id = '2' BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
        assert!(
            store
                .ingest(
                    "operator",
                    "inbox",
                    &[task("1"), task("2")],
                    BootstrapPolicy::Baseline,
                )
                .is_err()
        );
        assert!(store.jobs().unwrap().is_empty());
        store
            .conn
            .execute_batch("DROP TRIGGER reject_second")
            .unwrap();
        assert_eq!(ingest(&mut store, &[task("1")]).staged, 1);
    }

    #[test]
    fn collision_and_external_tuple_mismatch_abort_without_updates() {
        for collision in [true, false] {
            let (_dir, mut store) = setup();
            ingest(&mut store, &[task("existing")]);
            let mut job = store.jobs().unwrap().remove(0);
            store.conn.execute("DELETE FROM jobs", []).unwrap();
            if collision {
                job.capture_id = deterministic_capture_id("operator", "inbox", "new").unwrap();
            } else {
                job.capture_id = "wrong-id-for-existing-tuple".into();
            }
            store
                .conn
                .execute(
                    "INSERT INTO jobs VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        job.capture_id,
                        job.account_key,
                        job.list_id,
                        job.provider_id,
                        encode_job(&job).unwrap()
                    ],
                )
                .unwrap();
            let before = store.jobs().unwrap();
            let incoming = task(if collision { "new" } else { "existing" });
            assert!(
                store
                    .ingest(
                        "operator",
                        "inbox",
                        &[incoming],
                        BootstrapPolicy::ImportOpen,
                    )
                    .is_err()
            );
            assert_eq!(store.jobs().unwrap(), before);
        }
    }

    #[test]
    fn null_receipt_is_still_an_immutable_recorded_receipt() {
        let (dir, mut store) = setup();
        ingest(&mut store, &[task("1")]);
        let id = store.jobs().unwrap()[0].capture_id.clone();
        store.claim(&id, &binding()).unwrap();
        store.complete(&id, &serde_json::Value::Null).unwrap();
        drop(store);
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let mut store = CaptureStore::open(path).unwrap();
        assert_eq!(
            store.jobs().unwrap()[0].receipt,
            Some(serde_json::Value::Null)
        );
        store.complete(&id, &serde_json::Value::Null).unwrap();
        assert!(store.complete(&id, &json!({"replacement": true})).is_err());
    }

    #[test]
    fn destination_receipt_and_transition_guards_do_not_write() {
        let (_dir, mut store) = setup();
        ingest(&mut store, &[task("1")]);
        let id = store.jobs().unwrap()[0].capture_id.clone();
        assert!(store.complete(&id, &json!({})).is_err());
        store.claim(&id, &binding()).unwrap();
        let mut other = binding();
        other.inbox_path = "/other/inbox.md".into();
        let before = store.jobs().unwrap();
        assert!(store.claim(&id, &other).is_err());
        assert_eq!(store.jobs().unwrap(), before);
        let receipt = json!({"id": id, "path": "/notes/inbox.md"});
        store.complete(&id, &receipt).unwrap();
        let before = store.jobs().unwrap();
        store.complete(&id, &receipt).unwrap();
        assert!(store.complete(&id, &json!({"different": true})).is_err());
        assert!(store.block(&id, "cannot block delivered").is_err());
        assert_eq!(store.jobs().unwrap(), before);
        let mut deleted = task("1");
        deleted.deleted = true;
        ingest(&mut store, &[deleted]);
        let before = store.jobs().unwrap();
        store.complete(&id, &receipt).unwrap();
        assert_eq!(store.jobs().unwrap(), before);
    }

    #[test]
    fn missing_read_only_status_and_bad_paths_create_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let missing = root.join("missing.sqlite");
        assert!(CaptureStore::inspect(&missing).is_err());
        assert!(CaptureStore::open_existing(&missing).is_err());
        assert!(!missing.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert!(CaptureStore::open(root.join("missing-parent/db.sqlite")).is_err());
        assert!(!root.join("missing-parent").exists());
        assert!(CaptureStore::open(&root).is_err());
        assert!(CaptureStore::inspect(&root).is_err());
    }

    // Run only in a child, so releasing OS locks leaves the rollback journal hot
    // without forking a multithreaded test process or running SQLite destructors.
    #[test]
    #[ignore = "child-process crash fixture, invoked explicitly by recovery tests"]
    fn hot_journal_child() {
        let path = std::env::var_os("HIVE_CAPTURE_TEST_HOT_DB").unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             PRAGMA cache_size=1;
             PRAGMA cache_spill=ON;
             BEGIN IMMEDIATE;",
        )
        .unwrap();
        if std::env::var_os("HIVE_CAPTURE_TEST_POISON_JOBS").is_some() {
            conn.execute("UPDATE jobs SET job = 'uncommitted poison'", [])
                .unwrap();
            conn.execute("DELETE FROM connections", []).unwrap();
        }
        conn.execute_batch("CREATE TABLE crash_spill (payload BLOB NOT NULL);")
            .unwrap();
        for _ in 0..32 {
            conn.execute("INSERT INTO crash_spill VALUES (zeroblob(65536))", [])
                .unwrap();
        }
        std::process::exit(73);
    }

    fn make_hot_journal(path: &Path, poison_jobs: bool) -> PathBuf {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "store::tests::hot_journal_child", "--ignored"])
            .env("HIVE_CAPTURE_TEST_HOT_DB", path)
            .env_remove("HIVE_CAPTURE_TEST_POISON_JOBS");
        if poison_jobs {
            child.env("HIVE_CAPTURE_TEST_POISON_JOBS", "1");
        }
        let output = child.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(73),
            "child did not reach synthetic crash: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let mut journal = path.as_os_str().to_os_string();
        journal.push("-journal");
        let journal = PathBuf::from(journal);
        assert_eq!(
            &fs::read(&journal).unwrap()[..8],
            &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
        );
        journal
    }

    #[test]
    fn hot_journal_recovery_preserves_committed_receipt_and_inflight_claim() {
        let (dir, mut store) = setup();
        ingest(&mut store, &[task("delivered"), task("inflight")]);
        let delivered = deterministic_capture_id("operator", "inbox", "delivered").unwrap();
        let inflight = deterministic_capture_id("operator", "inbox", "inflight").unwrap();
        store.claim(&delivered, &binding()).unwrap();
        store
            .complete(&delivered, &json!({"stableId": delivered}))
            .unwrap();
        store.claim(&inflight, &binding()).unwrap();
        let committed = store.jobs().unwrap();
        drop(store);
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let journal = make_hot_journal(&path, true);
        let before = fs::read(&path).unwrap();
        let journal_before = fs::read(&journal).unwrap();
        let error = CaptureStore::inspect(&path).unwrap_err();
        assert!(
            error.to_string().contains("hot rollback journal"),
            "{error:#}"
        );
        assert!(error.to_string().contains("open_existing"));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(&journal).unwrap(), journal_before);
        let mut store = CaptureStore::open_existing(&path).unwrap();
        assert_eq!(store.jobs().unwrap(), committed);
        assert!(!journal.exists());
        let spill_exists: bool = store
            .conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE name = 'crash_spill')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!spill_exists);
        assert!(store.claim(&inflight, &binding()).is_err());
        assert!(store.claim(&delivered, &binding()).is_err());
        // Recovery must not reset the connection's committed bootstrap policy.
        let report = store
            .ingest(
                "operator",
                "inbox",
                &[task("later")],
                BootstrapPolicy::Baseline,
            )
            .unwrap();
        assert_eq!(report.staged, 1);
        drop(store);
        assert_eq!(CaptureStore::inspect(path).unwrap().len(), 3);
    }

    #[test]
    fn unrelated_hot_journal_is_never_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("notes.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('precious');",
        )
        .unwrap();
        drop(conn);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let journal = make_hot_journal(&path, false);
        let before = fs::read(&path).unwrap();
        let journal_before = fs::read(&journal).unwrap();
        assert!(CaptureStore::open(&path).is_err());
        assert!(CaptureStore::open_existing(&path).is_err());
        assert!(CaptureStore::inspect(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(journal).unwrap(), journal_before);
    }

    #[test]
    fn invalid_or_truncated_raw_headers_are_refused_without_writes() {
        let (dir, store) = setup();
        drop(store);
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        let original = fs::read(&path).unwrap();
        let mut invalid_headers = vec![Vec::new(), original[..99].to_vec(), vec![0; 100]];
        for (offset, value) in [(0, 0_u32), (68, 0), (68, 0x4e4f5445), (60, 0), (60, 3)] {
            let mut invalid = original.clone();
            invalid[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
            invalid_headers.push(invalid);
        }
        for invalid in invalid_headers {
            fs::write(&path, &invalid).unwrap();
            assert!(CaptureStore::open(&path).is_err());
            assert!(CaptureStore::open_existing(&path).is_err());
            assert!(CaptureStore::inspect(&path).is_err());
            assert_eq!(fs::read(&path).unwrap(), invalid);
            assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        }
    }

    #[test]
    fn unrelated_database_and_unknown_versions_are_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("notes.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('precious');",
        )
        .unwrap();
        drop(conn);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let before = fs::read(&path).unwrap();
        assert!(CaptureStore::open(&path).is_err());
        assert!(CaptureStore::inspect(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let (dir, store) = setup();
        let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
        drop(store);
        for version in [0, SCHEMA_VERSION + 1] {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
            drop(conn);
            let before = fs::read(&path).unwrap();
            assert!(CaptureStore::open(&path).is_err());
            assert!(CaptureStore::inspect(&path).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[cfg(unix)]
    #[test]
    fn exposed_existing_queue_is_refused_read_write_and_read_only_without_changes() {
        use std::os::unix::fs::PermissionsExt;
        for mode in [0o644, 0o666] {
            let (dir, store) = setup();
            drop(store);
            let path = dir.path().canonicalize().unwrap().join("capture.sqlite");
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            let before = fs::read(&path).unwrap();
            let error = CaptureStore::open(&path).err().unwrap();
            assert!(error.to_string().contains("owner-private"));
            assert!(CaptureStore::inspect(&path).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let mut store = CaptureStore::open(path).unwrap();
            assert!(store.jobs().unwrap().is_empty());
            let report = store
                .ingest("operator", "inbox", &[task("1")], BootstrapPolicy::Baseline)
                .unwrap();
            assert_eq!(report.baselined, 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_permissions_and_symlink_hardlink_rejection() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (dir, store) = setup();
        drop(store);
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("capture.sqlite");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = root.join("symlink.sqlite");
        symlink(&path, &link).unwrap();
        assert!(CaptureStore::open(&link).is_err());
        assert!(CaptureStore::inspect(&link).is_err());
        let parent_link = root.join("alias");
        symlink(&root, &parent_link).unwrap();
        assert!(CaptureStore::open(parent_link.join("new.sqlite")).is_err());
        assert!(!root.join("new.sqlite").exists());
        let hardlink = root.join("hardlink.sqlite");
        fs::hard_link(&path, &hardlink).unwrap();
        assert!(CaptureStore::open(&hardlink).is_err());
        assert!(CaptureStore::open(&path).is_err());
        assert!(CaptureStore::inspect(&hardlink).is_err());
    }
}
