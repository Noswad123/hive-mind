use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, anyhow, bail};
use hive_core::{
    ConflictEvent, EntityType, OperationType, PullResponse, PushRequest, PushResponse,
    SyncOperation,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub const LAST_SERVER_CURSOR_KEY: &str = "last_server_cursor";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutboxRow {
    pub id: i64,
    pub op_id: String,
    pub idempotency_key: String,
    pub entity_type: String,
    pub entity_key: String,
    pub op_type: String,
    pub payload: String,
    pub payload_hash: String,
    pub base_version: i64,
    pub status: String,
    pub attempt_count: i64,
    pub last_error: String,
    pub created_at: String,
    pub updated_at: String,
    pub acked_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LocalDiagnostics {
    pub db_path: String,
    pub local_cursor: String,
    pub pending_outbox_count: i64,
    pub unresolved_conflict_count: i64,
    pub sync_entity_version_count: i64,
    pub synced_todo_count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DesktopSyncResult {
    pub pushed_accepted: usize,
    pub pushed_rejected: usize,
    pub conflicts_logged: usize,
    pub pulled_applied: usize,
    pub pulled_note_operations_skipped: usize,
    pub final_cursor: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TodoPayload {
    id: Option<String>,
    source_id: Option<String>,
    source_path: Option<String>,
    task_scope: Option<String>,
    task_area: Option<String>,
    todo_section: Option<String>,
    text: Option<String>,
    done: Option<bool>,
    meta: Option<String>,
    order: Option<i64>,
    line: Option<i64>,
    updated_at: Option<String>,
    title: Option<String>,
    status: Option<String>,
}

pub struct DesktopStore {
    conn: Connection,
    db_path: String,
}

impl DesktopStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        let conn = Connection::open(path_ref)
            .with_context(|| format!("open local sync database {}", path_ref.display()))?;
        Ok(Self {
            conn,
            db_path: path_ref.display().to_string(),
        })
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self> {
        Ok(Self {
            conn: Connection::open_in_memory()?,
            db_path: ":memory:".to_string(),
        })
    }

    pub fn diagnostics(&self) -> Result<LocalDiagnostics> {
        Ok(LocalDiagnostics {
            db_path: self.db_path.clone(),
            local_cursor: self
                .get_sync_state(LAST_SERVER_CURSOR_KEY)?
                .unwrap_or_else(|| "0".to_string()),
            pending_outbox_count: self.count_where("sync_outbox", "status = 'pending'")?,
            unresolved_conflict_count: self.count_where("sync_conflicts", "resolved_at IS NULL")?,
            sync_entity_version_count: self.count_where("sync_entity_versions", "1 = 1")?,
            synced_todo_count: self.count_where("sync_todos", "1 = 1")?,
        })
    }

    pub fn list_pending_outbox(&self, limit: i64) -> Result<Vec<SyncOutboxRow>> {
        let limit = if limit <= 0 { 100 } else { limit };
        let mut stmt = self.conn.prepare(
            "SELECT id, op_id, idempotency_key, entity_type, entity_key, op_type,
                    COALESCE(payload,''), payload_hash, base_version, status, attempt_count,
                    COALESCE(last_error,''), COALESCE(created_at,''), COALESCE(updated_at,''), acked_at
             FROM sync_outbox
             WHERE status = 'pending'
             ORDER BY id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok(SyncOutboxRow {
                id: row.get(0)?,
                op_id: row.get(1)?,
                idempotency_key: row.get(2)?,
                entity_type: row.get(3)?,
                entity_key: row.get(4)?,
                op_type: row.get(5)?,
                payload: row.get(6)?,
                payload_hash: row.get(7)?,
                base_version: row.get(8)?,
                status: row.get(9)?,
                attempt_count: row.get(10)?,
                last_error: row.get(11)?,
                created_at: row.get(12)?,
                updated_at: row.get(13)?,
                acked_at: row.get(14)?,
            })
        })?;
        collect_rows(rows)
    }

    pub fn build_push_request(
        &self,
        device_id: &str,
        device_name: &str,
        platform: &str,
        app_version: &str,
        outbox_limit: i64,
    ) -> Result<(PushRequest, Vec<SyncOutboxRow>)> {
        let rows = self.list_pending_outbox(outbox_limit)?;
        let operations = rows
            .iter()
            .map(sync_operation_from_outbox)
            .collect::<Result<Vec<_>>>()?;
        Ok((
            PushRequest {
                device_id: device_id.trim().to_string(),
                device_name: device_name.trim().to_string(),
                platform: platform.trim().to_string(),
                app_version: app_version.trim().to_string(),
                operations,
            },
            rows,
        ))
    }

    pub fn apply_push_response(
        &self,
        rows: &[SyncOutboxRow],
        response: &PushResponse,
    ) -> Result<usize> {
        let pending: HashSet<&str> = rows.iter().map(|row| row.op_id.as_str()).collect();
        for reject in &response.rejected {
            if pending.contains(reject.op_id.as_str()) {
                self.mark_outbox_attempt_failed(&reject.op_id, &reject.reason)?;
            }
        }
        let rejected: HashSet<&str> = response
            .rejected
            .iter()
            .map(|reject| reject.op_id.as_str())
            .collect();
        let mut accepted = 0;
        for op_id in &response.accepted {
            if pending.contains(op_id.as_str()) && !rejected.contains(op_id.as_str()) {
                self.mark_outbox_acked(op_id)?;
                accepted += 1;
            }
        }
        for conflict in &response.conflicts {
            self.insert_sync_conflict(conflict)?;
        }
        Ok(accepted)
    }

    pub fn apply_pull_response(&self, response: &PullResponse) -> Result<(usize, usize)> {
        let tx = self.conn.unchecked_transaction()?;
        let mut applied = 0;
        let mut skipped_notes = 0;
        for op in &response.operations {
            match op.entity_type {
                EntityType::Todo => {
                    apply_todo_operation(&tx, op)?;
                    increment_entity_version_tx(&tx, op.entity_type, &op.entity_id)?;
                    applied += 1;
                }
                EntityType::Note => {
                    // Deliberately do not materialize remote note operations into Markdown yet.
                    increment_entity_version_tx(&tx, op.entity_type, &op.entity_id)?;
                    skipped_notes += 1;
                }
            }
        }
        if !response.next_cursor.trim().is_empty() {
            set_sync_state_tx(&tx, LAST_SERVER_CURSOR_KEY, &response.next_cursor)?;
        }
        tx.commit()?;
        Ok((applied, skipped_notes))
    }

    pub fn get_sync_state(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM sync_state WHERE key = ?1",
                [key.trim()],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_sync_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_state (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key.trim(), value.trim(), now_rfc3339()],
        )?;
        Ok(())
    }

    fn mark_outbox_acked(&self, op_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_outbox
             SET status = 'acked', acked_at = ?2, updated_at = ?2, last_error = NULL
             WHERE op_id = ?1",
            params![op_id.trim(), now_rfc3339()],
        )?;
        Ok(())
    }

    fn mark_outbox_attempt_failed(&self, op_id: &str, reason: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_outbox
             SET status = 'pending', attempt_count = attempt_count + 1, last_error = ?2, updated_at = ?3
             WHERE op_id = ?1",
            params![op_id.trim(), reason.trim(), now_rfc3339()],
        )?;
        Ok(())
    }

    fn insert_sync_conflict(&self, event: &ConflictEvent) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_conflicts (entity_type, entity_key, local_payload, remote_payload, reason, created_at)
             VALUES (?1, ?2, '', '', ?3, ?4)",
            params![entity_type_wire(event.entity_type), event.entity_id.trim(), event.reason.trim(), now_rfc3339()],
        )?;
        Ok(())
    }

    fn count_where(&self, table: &str, predicate: &str) -> Result<i64> {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE {predicate}");
        self.conn
            .query_row(&sql, [], |row| row.get(0))
            .map_err(Into::into)
    }
}

pub fn run_desktop_sync_once(
    store: &DesktopStore,
    endpoint: &str,
    token: Option<&str>,
    device_id: &str,
    device_name: &str,
    platform: &str,
    app_version: &str,
    outbox_limit: i64,
    pull_limit: usize,
) -> Result<DesktopSyncResult> {
    let mut result = DesktopSyncResult::default();
    let cursor = store
        .get_sync_state(LAST_SERVER_CURSOR_KEY)?
        .unwrap_or_else(|| "0".to_string());
    result.final_cursor = cursor.clone();

    let (push_req, outbox_rows) =
        store.build_push_request(device_id, device_name, platform, app_version, outbox_limit)?;
    if !push_req.operations.is_empty() {
        let push_resp = hive_core::push_operations(endpoint, token, &push_req)?;
        result.pushed_rejected = push_resp.rejected.len();
        result.conflicts_logged = push_resp.conflicts.len();
        result.pushed_accepted = store.apply_push_response(&outbox_rows, &push_resp)?;
    }

    let pull_resp = hive_core::pull_operations(endpoint, token, &cursor, pull_limit)?;
    let next_cursor = pull_resp.next_cursor.clone();
    let (applied, skipped_notes) = store.apply_pull_response(&pull_resp)?;
    result.pulled_applied = applied;
    result.pulled_note_operations_skipped = skipped_notes;
    result.final_cursor = next_cursor;
    Ok(result)
}

fn sync_operation_from_outbox(row: &SyncOutboxRow) -> Result<SyncOperation> {
    Ok(SyncOperation {
        op_id: row.op_id.trim().to_string(),
        idempotency_key: row.idempotency_key.trim().to_string(),
        entity_type: parse_entity_type(&row.entity_type)?,
        entity_id: row.entity_key.trim().to_string(),
        op_type: parse_operation_type(&row.op_type)?,
        payload: parse_payload(&row.payload)?,
        client_updated_at: first_non_empty(&[&row.updated_at, &row.created_at])
            .unwrap_or_else(now_rfc3339),
        base_version: row.base_version,
    })
}

fn apply_todo_operation(tx: &rusqlite::Transaction<'_>, op: &SyncOperation) -> Result<()> {
    match op.op_type {
        OperationType::Delete => {
            let id = op
                .payload
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or(&op.entity_id)
                .trim();
            if id.is_empty() {
                bail!("todo delete id is required");
            }
            tx.execute("DELETE FROM sync_todos WHERE id = ?1", [id])?;
            Ok(())
        }
        OperationType::Upsert => {
            let payload: TodoPayload = serde_json::from_value(op.payload.clone())?;
            let id = payload
                .id
                .as_deref()
                .unwrap_or(&op.entity_id)
                .trim()
                .to_string();
            if id.is_empty() {
                bail!("todo id is required");
            }
            let source_id = first_non_empty_opt(&[
                payload.source_id.as_deref(),
                payload.source_path.as_deref(),
            ])
            .unwrap_or_else(|| id.clone());
            let text = first_non_empty_opt(&[payload.text.as_deref(), payload.title.as_deref()])
                .ok_or_else(|| anyhow!("todo text is required"))?;
            let section =
                normalize_todo_section(payload.todo_section.as_deref(), payload.status.as_deref())?;
            let done = payload.done.unwrap_or(false)
                || payload.status.as_deref().is_some_and(|status| {
                    matches!(
                        status.trim().to_ascii_lowercase().as_str(),
                        "done" | "archived"
                    )
                });
            tx.execute(
                "INSERT INTO sync_todos (
                   id, source_id, source_path, task_scope, task_area, todo_section,
                   task_text, is_done, meta, task_order, line_number, updated_at, payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(id) DO UPDATE SET
                   source_id = excluded.source_id,
                   source_path = excluded.source_path,
                   task_scope = excluded.task_scope,
                   task_area = excluded.task_area,
                   todo_section = excluded.todo_section,
                   task_text = excluded.task_text,
                   is_done = excluded.is_done,
                   meta = excluded.meta,
                   task_order = excluded.task_order,
                   line_number = excluded.line_number,
                   updated_at = excluded.updated_at,
                   payload = excluded.payload",
                params![
                    id,
                    source_id,
                    trim_opt(payload.source_path),
                    trim_opt(payload.task_scope),
                    trim_opt(payload.task_area),
                    section,
                    text,
                    done,
                    trim_opt(payload.meta),
                    payload.order.unwrap_or(0),
                    payload.line.filter(|line| *line >= 0),
                    first_non_empty_opt(&[
                        payload.updated_at.as_deref(),
                        Some(op.client_updated_at.as_str())
                    ])
                    .unwrap_or_else(now_rfc3339),
                    serde_json::to_string(&op.payload)?,
                ],
            )?;
            Ok(())
        }
    }
}

fn increment_entity_version_tx(
    tx: &rusqlite::Transaction<'_>,
    entity_type: EntityType,
    entity_key: &str,
) -> Result<i64> {
    let current = tx
        .query_row(
            "SELECT version FROM sync_entity_versions WHERE entity_type = ?1 AND entity_key = ?2",
            params![entity_type_wire(entity_type), entity_key.trim()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0)
        .max(0);
    let next = current + 1;
    tx.execute(
        "INSERT INTO sync_entity_versions (entity_type, entity_key, version, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(entity_type, entity_key) DO UPDATE SET version = excluded.version, updated_at = excluded.updated_at",
        params![entity_type_wire(entity_type), entity_key.trim(), next, now_rfc3339()],
    )?;
    Ok(next)
}

fn set_sync_state_tx(tx: &rusqlite::Transaction<'_>, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO sync_state (key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key.trim(), value.trim(), now_rfc3339()],
    )?;
    Ok(())
}

fn parse_entity_type(value: &str) -> Result<EntityType> {
    match value.trim() {
        "note" => Ok(EntityType::Note),
        "todo" => Ok(EntityType::Todo),
        other => bail!("unsupported entity_type {other:?}"),
    }
}

fn parse_operation_type(value: &str) -> Result<OperationType> {
    match value.trim() {
        "upsert" => Ok(OperationType::Upsert),
        "delete" => Ok(OperationType::Delete),
        other => bail!("unsupported op_type {other:?}"),
    }
}

fn entity_type_wire(entity_type: EntityType) -> &'static str {
    match entity_type {
        EntityType::Note => "note",
        EntityType::Todo => "todo",
    }
}

fn parse_payload(payload: &str) -> Result<serde_json::Value> {
    let payload = payload.trim();
    if payload.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(payload).or_else(|_| Ok(serde_json::Value::String(payload.to_string())))
}

fn normalize_todo_section(section: Option<&str>, legacy_status: Option<&str>) -> Result<String> {
    let raw = section.unwrap_or("").trim();
    if raw.is_empty() {
        return Ok(
            match legacy_status
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "doing" => "Next".to_string(),
                "done" | "archived" => "Waiting".to_string(),
                _ => "Inbox".to_string(),
            },
        );
    }
    match raw.to_ascii_lowercase().as_str() {
        "inbox" => Ok("Inbox".to_string()),
        "next" => Ok("Next".to_string()),
        "waiting" => Ok("Waiting".to_string()),
        _ => bail!("todo_section must be Inbox, Next, or Waiting"),
    }
}

fn trim_opt(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn first_non_empty(values: &[&str]) -> Option<String> {
    values
        .iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn first_non_empty_opt(values: &[Option<&str>]) -> Option<String> {
    values
        .iter()
        .filter_map(|value| value.map(str::trim))
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn collect_rows<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_store() -> Result<DesktopStore> {
        let store = DesktopStore::open_in_memory()?;
        store.conn.execute_batch(
            "CREATE TABLE sync_outbox (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                op_id TEXT NOT NULL UNIQUE,
                idempotency_key TEXT NOT NULL UNIQUE,
                entity_type TEXT NOT NULL,
                entity_key TEXT NOT NULL,
                op_type TEXT NOT NULL,
                payload TEXT,
                payload_hash TEXT NOT NULL,
                base_version INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'pending',
                attempt_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                acked_at TEXT
             );
             CREATE TABLE sync_state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE sync_conflicts (id INTEGER PRIMARY KEY AUTOINCREMENT, entity_type TEXT NOT NULL, entity_key TEXT NOT NULL, local_payload TEXT, remote_payload TEXT, reason TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, resolved_at TEXT);
             CREATE TABLE sync_entity_versions (entity_type TEXT NOT NULL, entity_key TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 0, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(entity_type, entity_key));
             CREATE TABLE sync_todos (id TEXT PRIMARY KEY, source_id TEXT NOT NULL DEFAULT '', source_path TEXT, task_scope TEXT, task_area TEXT, todo_section TEXT NOT NULL DEFAULT 'Inbox', task_text TEXT NOT NULL DEFAULT '', is_done INTEGER NOT NULL DEFAULT 0, meta TEXT, task_order INTEGER NOT NULL DEFAULT 0, line_number INTEGER, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, payload TEXT NOT NULL DEFAULT '{}');",
        )?;
        Ok(store)
    }

    #[test]
    fn builds_push_request_from_pending_outbox() -> Result<()> {
        let store = create_store()?;
        store.conn.execute(
            r#"INSERT INTO sync_outbox (op_id, idempotency_key, entity_type, entity_key, op_type, payload, payload_hash, base_version)
             VALUES ('op-1', 'idem-1', 'note', 'notes/a.md', 'upsert', '{"path":"notes/a.md"}', 'hash', 3)"#,
            [],
        )?;
        let (req, rows) =
            store.build_push_request("desktop", "Desk", "darwin", "hive-test", 100)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(req.operations[0].entity_type, EntityType::Note);
        assert_eq!(req.operations[0].base_version, 3);
        Ok(())
    }

    #[test]
    fn apply_push_response_marks_acked_and_rejected() -> Result<()> {
        let store = create_store()?;
        store.conn.execute(
            "INSERT INTO sync_outbox (op_id, idempotency_key, entity_type, entity_key, op_type, payload, payload_hash, base_version)
             VALUES ('op-1', 'idem-1', 'note', 'notes/a.md', 'upsert', '{}', 'hash', 0),
                    ('op-2', 'idem-2', 'note', 'notes/b.md', 'upsert', '{}', 'hash2', 0)",
            [],
        )?;
        let rows = store.list_pending_outbox(100)?;
        store.apply_push_response(
            &rows,
            &PushResponse {
                accepted: vec!["op-1".to_string()],
                rejected: vec![hive_core::PushReject {
                    op_id: "op-2".to_string(),
                    reason: "bad".to_string(),
                }],
                server_cursor: "1".to_string(),
                conflicts: vec![],
            },
        )?;
        let status_1: String = store.conn.query_row(
            "SELECT status FROM sync_outbox WHERE op_id='op-1'",
            [],
            |row| row.get(0),
        )?;
        let attempts_2: i64 = store.conn.query_row(
            "SELECT attempt_count FROM sync_outbox WHERE op_id='op-2'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(status_1, "acked");
        assert_eq!(attempts_2, 1);
        Ok(())
    }

    #[test]
    fn apply_pull_response_upserts_todo_and_cursor() -> Result<()> {
        let store = create_store()?;
        let response = PullResponse {
            operations: vec![SyncOperation {
                op_id: "op-todo".to_string(),
                idempotency_key: "idem-todo".to_string(),
                entity_type: EntityType::Todo,
                entity_id: "todo-1".to_string(),
                op_type: OperationType::Upsert,
                payload: serde_json::json!({ "id": "todo-1", "text": "Buy milk", "todo_section": "Next", "done": true }),
                client_updated_at: "2026-07-10T00:00:00Z".to_string(),
                base_version: 0,
            }],
            next_cursor: "42".to_string(),
            has_more: false,
        };
        let (applied, skipped) = store.apply_pull_response(&response)?;
        assert_eq!(applied, 1);
        assert_eq!(skipped, 0);
        let text: String = store.conn.query_row(
            "SELECT task_text FROM sync_todos WHERE id='todo-1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(text, "Buy milk");
        assert_eq!(
            store.get_sync_state(LAST_SERVER_CURSOR_KEY)?,
            Some("42".to_string())
        );
        Ok(())
    }
}
