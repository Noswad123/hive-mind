use std::{
    collections::HashMap,
    net::SocketAddr,
    str::FromStr,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hive_core::{
    ConflictEvent, DeviceRegisterResponse, DeviceRegistration, EntityType, PullResponse,
    PushReject, PushRequest, PushResponse, SyncOperation, SyncStateResponse,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tower_http::cors::CorsLayer;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub addr: String,
    pub device_tokens: HashMap<String, String>,
    pub permissive_cors: bool,
    pub store: StoreConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:8080".to_string(),
            device_tokens: HashMap::new(),
            permissive_cors: false,
            store: StoreConfig::Memory,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreConfig {
    Memory,
    Sqlite { path: String },
}

#[derive(Clone)]
struct AppState {
    store: StoreBackend,
    auth: AuthConfig,
}

#[derive(Debug, Clone)]
enum StoreBackend {
    Memory(MemoryStore),
    Sqlite(SqliteStore),
}

#[derive(Debug, Clone, Default)]
struct AuthConfig {
    token_to_device: HashMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    inner: Arc<Mutex<MemoryStoreInner>>,
}

#[derive(Debug, Clone)]
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Default)]
struct MemoryStoreInner {
    next_cursor: i64,
    latest_cursor: i64,
    operations: Vec<StoredOperation>,
    idempotency_index: HashMap<String, i64>,
    devices: HashMap<String, DeviceRegistration>,
    entities: HashMap<String, EntityStateSnapshot>,
}

#[derive(Debug, Clone)]
struct StoredOperation {
    cursor: i64,
    applied: bool,
    op: SyncOperation,
}

#[derive(Debug, Clone, Default)]
struct EntityStateSnapshot {
    version: i64,
    last_op_id: String,
    last_device_id: String,
    last_client_updated: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushResult {
    accepted: Vec<String>,
    conflicts: Vec<ConflictEvent>,
    server_cursor: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct PullQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct ErrorBody {
    error: String,
}

#[derive(Debug, Clone)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

pub async fn serve(config: ServerConfig) -> Result<()> {
    let addr = SocketAddr::from_str(config.addr.trim()).context("parse listen address")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    let app = router(config);
    axum::serve(listener, app).await.context("serve hive API")
}

pub fn router(config: ServerConfig) -> Router {
    let store = StoreBackend::open(config.store).expect("initialize sync store");
    let state = AppState {
        store,
        auth: AuthConfig::from_device_tokens(config.device_tokens),
    };
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/sync/state", get(sync_state))
        .route("/v1/sync/push", post(sync_push))
        .route("/v1/sync/pull", get(sync_pull))
        .route("/v1/devices/register", post(device_register))
        .with_state(state);

    if config.permissive_cors {
        router.layer(CorsLayer::permissive())
    } else {
        router
    }
}

impl AuthConfig {
    fn from_device_tokens(device_tokens: HashMap<String, String>) -> Self {
        let token_to_device = device_tokens
            .into_iter()
            .filter_map(|(device, token)| {
                let device = device.trim().to_string();
                let token = token.trim().to_string();
                (!device.is_empty() && !token.is_empty()).then_some((token, device))
            })
            .collect();
        Self { token_to_device }
    }

    fn authenticated_device(&self, headers: &HeaderMap) -> Result<Option<String>, ApiError> {
        if self.token_to_device.is_empty() {
            return Ok(None);
        }
        let header = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;

        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ApiError::unauthorized("invalid authorization header"))?;

        self.token_to_device
            .get(token)
            .cloned()
            .map(Some)
            .ok_or_else(|| ApiError::forbidden("invalid bearer token"))
    }
}

impl MemoryStore {
    pub fn register_device(&self, req: DeviceRegistration) -> Result<()> {
        let mut inner = self.lock()?;
        inner.devices.insert(req.device_id.clone(), req);
        Ok(())
    }

    pub fn push(&self, device_id: &str, operations: Vec<SyncOperation>) -> Result<PushResult> {
        let mut inner = self.lock()?;
        let mut accepted = Vec::with_capacity(operations.len());
        let mut conflicts = Vec::new();

        for op in operations {
            let idem_key = format!("{}::{}", device_id.trim(), op.idempotency_key.trim());
            if inner.idempotency_index.contains_key(&idem_key) {
                accepted.push(op.op_id.clone());
                continue;
            }

            let entity_key = entity_map_key(op.entity_type, &op.entity_id);
            let current = inner.entities.get(&entity_key).cloned();
            let is_conflict = current
                .as_ref()
                .is_some_and(|snapshot| op.base_version != snapshot.version);
            let incoming_wins = current.as_ref().is_none_or(|snapshot| {
                !is_conflict || incoming_wins_by_lww(&op, device_id, snapshot)
            });
            let applied = !is_conflict || incoming_wins;

            inner.next_cursor += 1;
            inner.latest_cursor = inner.next_cursor;
            let cursor = inner.latest_cursor;
            inner.idempotency_index.insert(idem_key, cursor);
            inner.operations.push(StoredOperation {
                cursor,
                applied,
                op: op.clone(),
            });

            if applied {
                let next_version = current.as_ref().map_or(1, |snapshot| snapshot.version + 1);
                inner.entities.insert(
                    entity_key,
                    EntityStateSnapshot {
                        version: next_version,
                        last_op_id: op.op_id.clone(),
                        last_device_id: device_id.trim().to_string(),
                        last_client_updated: op.client_updated_at.clone(),
                    },
                );
            }

            if is_conflict {
                if let Some(snapshot) = current {
                    conflicts.push(conflict_event(&op, device_id, &snapshot, incoming_wins));
                }
            }

            accepted.push(op.op_id.clone());
        }

        Ok(PushResult {
            accepted,
            conflicts,
            server_cursor: inner.latest_cursor,
        })
    }

    pub fn pull(&self, cursor: i64, limit: usize) -> Result<PullResponse> {
        let inner = self.lock()?;
        let limit = limit.clamp(1, 500);
        let mut operations = Vec::with_capacity(limit);
        let mut next_cursor = cursor;
        let mut has_more = false;

        for item in &inner.operations {
            if item.cursor <= cursor || !item.applied {
                continue;
            }
            if operations.len() >= limit {
                has_more = true;
                break;
            }
            operations.push(item.op.clone());
            next_cursor = item.cursor;
        }

        Ok(PullResponse {
            operations,
            next_cursor: next_cursor.to_string(),
            has_more,
        })
    }

    pub fn latest_cursor(&self) -> Result<i64> {
        Ok(self.lock()?.latest_cursor)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, MemoryStoreInner>> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("store lock poisoned"))
    }
}

impl StoreBackend {
    fn open(config: StoreConfig) -> Result<Self> {
        match config {
            StoreConfig::Memory => Ok(Self::Memory(MemoryStore::default())),
            StoreConfig::Sqlite { path } => Ok(Self::Sqlite(SqliteStore::open(path)?)),
        }
    }

    fn register_device(&self, req: DeviceRegistration) -> Result<()> {
        match self {
            Self::Memory(store) => store.register_device(req),
            Self::Sqlite(store) => store.register_device(req),
        }
    }

    fn push(&self, device_id: &str, operations: Vec<SyncOperation>) -> Result<PushResult> {
        match self {
            Self::Memory(store) => store.push(device_id, operations),
            Self::Sqlite(store) => store.push(device_id, operations),
        }
    }

    fn pull(&self, cursor: i64, limit: usize) -> Result<PullResponse> {
        match self {
            Self::Memory(store) => store.pull(cursor, limit),
            Self::Sqlite(store) => store.pull(cursor, limit),
        }
    }

    fn latest_cursor(&self) -> Result<i64> {
        match self {
            Self::Memory(store) => store.latest_cursor(),
            Self::Sqlite(store) => store.latest_cursor(),
        }
    }
}

impl SqliteStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create sqlite store parent {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open sqlite sync store {}", path.display()))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.create_schema()?;
        Ok(store)
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Arc::new(Mutex::new(Connection::open_in_memory()?)),
        };
        store.create_schema()?;
        Ok(store)
    }

    fn create_schema(&self) -> Result<()> {
        self.lock()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS sync_devices (
                device_id TEXT PRIMARY KEY,
                device_name TEXT NOT NULL DEFAULT '',
                platform TEXT NOT NULL DEFAULT '',
                app_version TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );
             CREATE TABLE IF NOT EXISTS sync_operations (
                cursor INTEGER PRIMARY KEY AUTOINCREMENT,
                device_id TEXT NOT NULL,
                applied INTEGER NOT NULL DEFAULT 1,
                op_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );
             CREATE TABLE IF NOT EXISTS sync_idempotency (
                device_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                cursor INTEGER NOT NULL,
                PRIMARY KEY(device_id, idempotency_key)
             );
             CREATE TABLE IF NOT EXISTS sync_entities (
                entity_type TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 0,
                last_cursor INTEGER NOT NULL DEFAULT 0,
                last_op_id TEXT NOT NULL DEFAULT '',
                last_device_id TEXT NOT NULL DEFAULT '',
                last_client_updated TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY(entity_type, entity_id)
             );
             CREATE TABLE IF NOT EXISTS sync_conflicts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                entity_type TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                winner TEXT NOT NULL,
                loser TEXT NOT NULL,
                reason TEXT NOT NULL,
                winner_device_id TEXT NOT NULL,
                loser_device_id TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );",
        )?;
        Ok(())
    }

    pub fn register_device(&self, req: DeviceRegistration) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sync_devices (device_id, device_name, platform, app_version, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(device_id) DO UPDATE SET
               device_name = excluded.device_name,
               platform = excluded.platform,
               app_version = excluded.app_version,
               updated_at = excluded.updated_at",
            params![
                req.device_id.trim(),
                req.device_name.trim(),
                req.platform.trim(),
                req.app_version.trim(),
                current_utc_string(),
            ],
        )?;
        Ok(())
    }

    pub fn push(&self, device_id: &str, operations: Vec<SyncOperation>) -> Result<PushResult> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut accepted = Vec::with_capacity(operations.len());
        let mut conflicts = Vec::new();
        let device_id = device_id.trim();

        for op in operations {
            let exists = tx
                .query_row(
                    "SELECT cursor FROM sync_idempotency WHERE device_id = ?1 AND idempotency_key = ?2",
                    params![device_id, op.idempotency_key.trim()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if exists.is_some() {
                accepted.push(op.op_id.clone());
                continue;
            }

            let current = sqlite_entity_snapshot(&tx, op.entity_type, &op.entity_id)?;
            let is_conflict = current
                .as_ref()
                .is_some_and(|snapshot| op.base_version != snapshot.version);
            let incoming_wins = current.as_ref().is_none_or(|snapshot| {
                !is_conflict || incoming_wins_by_lww(&op, device_id, snapshot)
            });
            let applied = !is_conflict || incoming_wins;
            let op_json = serde_json::to_string(&op)?;

            tx.execute(
                "INSERT INTO sync_operations (device_id, applied, op_json, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![device_id, applied, op_json, current_utc_string()],
            )?;
            let cursor = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO sync_idempotency (device_id, idempotency_key, cursor)
                 VALUES (?1, ?2, ?3)",
                params![device_id, op.idempotency_key.trim(), cursor],
            )?;

            if applied {
                let next_version = current.as_ref().map_or(1, |snapshot| snapshot.version + 1);
                tx.execute(
                    "INSERT INTO sync_entities (
                       entity_type, entity_id, version, last_cursor, last_op_id,
                       last_device_id, last_client_updated, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(entity_type, entity_id) DO UPDATE SET
                       version = excluded.version,
                       last_cursor = excluded.last_cursor,
                       last_op_id = excluded.last_op_id,
                       last_device_id = excluded.last_device_id,
                       last_client_updated = excluded.last_client_updated,
                       updated_at = excluded.updated_at",
                    params![
                        entity_type_wire(op.entity_type),
                        op.entity_id.trim(),
                        next_version,
                        cursor,
                        op.op_id.trim(),
                        device_id,
                        op.client_updated_at.trim(),
                        current_utc_string(),
                    ],
                )?;
            }

            if is_conflict {
                if let Some(snapshot) = current {
                    let event = conflict_event(&op, device_id, &snapshot, incoming_wins);
                    tx.execute(
                        "INSERT INTO sync_conflicts (
                           entity_type, entity_id, winner, loser, reason,
                           winner_device_id, loser_device_id, created_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            entity_type_wire(event.entity_type),
                            event.entity_id,
                            event.winner,
                            event.loser,
                            event.reason,
                            event.winner_device_id,
                            event.loser_device_id,
                            current_utc_string(),
                        ],
                    )?;
                    conflicts.push(event);
                }
            }

            accepted.push(op.op_id.clone());
        }

        let latest_cursor = tx
            .query_row(
                "SELECT COALESCE(MAX(cursor), 0) FROM sync_operations",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0);
        tx.commit()?;
        Ok(PushResult {
            accepted,
            conflicts,
            server_cursor: latest_cursor,
        })
    }

    pub fn pull(&self, cursor: i64, limit: usize) -> Result<PullResponse> {
        let conn = self.lock()?;
        let limit = limit.clamp(1, 500) as i64;
        let mut stmt = conn.prepare(
            "SELECT cursor, op_json
             FROM sync_operations
             WHERE cursor > ?1 AND applied = 1
             ORDER BY cursor ASC
             LIMIT ?2",
        )?;
        let mut rows = stmt.query(params![cursor, limit + 1])?;
        let mut operations = Vec::new();
        let mut next_cursor = cursor;
        let mut has_more = false;
        while let Some(row) = rows.next()? {
            if operations.len() >= limit as usize {
                has_more = true;
                break;
            }
            next_cursor = row.get(0)?;
            let raw: String = row.get(1)?;
            operations.push(serde_json::from_str(&raw)?);
        }
        Ok(PullResponse {
            operations,
            next_cursor: next_cursor.to_string(),
            has_more,
        })
    }

    pub fn latest_cursor(&self) -> Result<i64> {
        self.lock()?
            .query_row(
                "SELECT COALESCE(MAX(cursor), 0) FROM sync_operations",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("sqlite store lock poisoned"))
    }
}

fn sqlite_entity_snapshot(
    tx: &rusqlite::Transaction<'_>,
    entity_type: EntityType,
    entity_id: &str,
) -> Result<Option<EntityStateSnapshot>> {
    tx.query_row(
        "SELECT version, last_op_id, last_device_id, last_client_updated
         FROM sync_entities
         WHERE entity_type = ?1 AND entity_id = ?2",
        params![entity_type_wire(entity_type), entity_id.trim()],
        |row| {
            Ok(EntityStateSnapshot {
                version: row.get(0)?,
                last_op_id: row.get(1)?,
                last_device_id: row.get(2)?,
                last_client_updated: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn entity_map_key(entity_type: EntityType, entity_id: &str) -> String {
    format!("{}::{}", entity_type_wire(entity_type), entity_id.trim())
}

fn entity_type_wire(entity_type: EntityType) -> &'static str {
    match entity_type {
        EntityType::Note => "note",
        EntityType::Todo => "todo",
    }
}

fn incoming_wins_by_lww(
    incoming: &SyncOperation,
    incoming_device_id: &str,
    current: &EntityStateSnapshot,
) -> bool {
    let incoming_time = parse_rfc3339(&incoming.client_updated_at);
    let current_time = parse_rfc3339(&current.last_client_updated);

    match (incoming_time, current_time) {
        (Some(left), Some(right)) if left > right => return true,
        (Some(left), Some(right)) if left < right => return false,
        (Some(_), None) => return true,
        (None, Some(_)) => return false,
        _ => {}
    }

    match incoming.op_id.cmp(&current.last_op_id) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => incoming_device_id.trim() >= current.last_device_id.trim(),
    }
}

fn parse_rfc3339(value: &str) -> Option<OffsetDateTime> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    OffsetDateTime::parse(trimmed, &Rfc3339).ok()
}

fn conflict_event(
    incoming: &SyncOperation,
    incoming_device_id: &str,
    current: &EntityStateSnapshot,
    incoming_wins: bool,
) -> ConflictEvent {
    if incoming_wins {
        ConflictEvent {
            entity_type: incoming.entity_type,
            entity_id: incoming.entity_id.clone(),
            winner: incoming.op_id.clone(),
            loser: current.last_op_id.clone(),
            reason: "base_version_mismatch".to_string(),
            winner_device_id: incoming_device_id.trim().to_string(),
            loser_device_id: current.last_device_id.clone(),
        }
    } else {
        ConflictEvent {
            entity_type: incoming.entity_type,
            entity_id: incoming.entity_id.clone(),
            winner: current.last_op_id.clone(),
            loser: incoming.op_id.clone(),
            reason: "base_version_mismatch".to_string(),
            winner_device_id: current.last_device_id.clone(),
            loser_device_id: incoming_device_id.trim().to_string(),
        }
    }
}

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn sync_state(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SyncStateResponse>, ApiError> {
    state.auth.authenticated_device(&headers)?;
    let latest_cursor = state
        .store
        .latest_cursor()
        .map_err(|err| ApiError::internal(err.to_string()))?;
    Ok(Json(SyncStateResponse {
        server_time: current_utc_string(),
        latest_cursor: latest_cursor.to_string(),
    }))
}

async fn sync_push(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PushRequest>,
) -> Result<Json<PushResponse>, ApiError> {
    let auth_device = state.auth.authenticated_device(&headers)?;
    let device_id = req.device_id.trim().to_string();
    if device_id.is_empty() {
        return Err(ApiError::bad_request("device_id is required"));
    }
    if let Some(auth_device) = auth_device {
        if !auth_device.eq_ignore_ascii_case(&device_id) {
            return Err(ApiError::forbidden(
                "token device_id does not match request device_id",
            ));
        }
    }
    state
        .store
        .register_device(DeviceRegistration {
            device_id: device_id.to_string(),
            device_name: req.device_name,
            platform: req.platform,
            app_version: req.app_version,
        })
        .map_err(|err| ApiError::internal(err.to_string()))?;

    let mut rejected = Vec::new();
    let mut valid = Vec::new();
    for op in req.operations {
        match validate_operation(op) {
            Ok(op) => valid.push(op),
            Err(reject) => rejected.push(reject),
        }
    }

    let result = state
        .store
        .push(&device_id, valid)
        .map_err(|err| ApiError::internal(err.to_string()))?;
    Ok(Json(PushResponse {
        accepted: result.accepted,
        rejected,
        server_cursor: result.server_cursor.to_string(),
        conflicts: result.conflicts,
    }))
}

async fn sync_pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PullQuery>,
) -> Result<Json<PullResponse>, ApiError> {
    state.auth.authenticated_device(&headers)?;
    let cursor = query
        .cursor
        .as_deref()
        .unwrap_or("0")
        .trim()
        .parse::<i64>()
        .map_err(|_| ApiError::bad_request("cursor must be a non-negative integer"))?;
    if cursor < 0 {
        return Err(ApiError::bad_request(
            "cursor must be a non-negative integer",
        ));
    }
    let limit = query.limit.unwrap_or(100);
    if limit == 0 {
        return Err(ApiError::bad_request("limit must be > 0"));
    }
    state
        .store
        .pull(cursor, limit)
        .map(Json)
        .map_err(|err| ApiError::internal(err.to_string()))
}

async fn device_register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DeviceRegistration>,
) -> Result<Json<DeviceRegisterResponse>, ApiError> {
    let auth_device = state.auth.authenticated_device(&headers)?;
    let device_id = req.device_id.trim().to_string();
    if device_id.is_empty() {
        return Err(ApiError::bad_request("device_id is required"));
    }
    if let Some(auth_device) = auth_device {
        if !auth_device.eq_ignore_ascii_case(&device_id) {
            return Err(ApiError::forbidden(
                "token device_id does not match request device_id",
            ));
        }
    }
    state
        .store
        .register_device(req)
        .map_err(|err| ApiError::internal(err.to_string()))?;
    Ok(Json(DeviceRegisterResponse {
        device_id,
        registered: true,
    }))
}

fn validate_operation(op: SyncOperation) -> Result<SyncOperation, PushReject> {
    let op_id = op.op_id.trim();
    if op_id.is_empty() {
        return Err(PushReject {
            op_id: String::new(),
            reason: "op_id is required".to_string(),
        });
    }
    let reject = |reason: &str| PushReject {
        op_id: op_id.to_string(),
        reason: reason.to_string(),
    };
    if op.idempotency_key.trim().is_empty() {
        return Err(reject("idempotency_key is required"));
    }
    if op.entity_id.trim().is_empty() {
        return Err(reject("entity_id is required"));
    }
    if op.base_version < 0 {
        return Err(reject("base_version must be >= 0"));
    }
    Ok(op)
}

fn current_utc_string() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub fn parse_device_tokens(raw: &str) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for pair in raw
        .split(',')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
    {
        let Some((device, token)) = pair.split_once('=') else {
            bail!("device token entry must be device=token");
        };
        let device = device.trim();
        let token = token.trim();
        if device.is_empty() || token.is_empty() {
            bail!("device token entry must include non-empty device and token");
        }
        out.insert(device.to_string(), token.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::OperationType;
    use tempfile::NamedTempFile;

    fn op(op_id: &str, device_suffix: &str, base_version: i64, updated: &str) -> SyncOperation {
        SyncOperation {
            op_id: op_id.to_string(),
            idempotency_key: format!("idem-{op_id}-{device_suffix}"),
            entity_type: EntityType::Note,
            entity_id: "notes/example.md".to_string(),
            op_type: OperationType::Upsert,
            payload: serde_json::json!({ "path": "notes/example.md" }),
            client_updated_at: updated.to_string(),
            base_version,
        }
    }

    #[test]
    fn memory_store_push_pull_round_trip() {
        let store = MemoryStore::default();
        let result = store
            .push(
                "desktop",
                vec![op("op-1", "desktop", 0, "2026-07-10T00:00:00Z")],
            )
            .unwrap();
        assert_eq!(result.accepted, vec!["op-1"]);
        assert_eq!(result.server_cursor, 1);

        let pulled = store.pull(0, 100).unwrap();
        assert_eq!(pulled.operations.len(), 1);
        assert_eq!(pulled.next_cursor, "1");
        assert!(!pulled.has_more);
    }

    #[test]
    fn stale_base_version_records_conflict_and_excludes_loser() {
        let store = MemoryStore::default();
        store
            .push(
                "desktop",
                vec![op("op-1", "desktop", 0, "2026-07-10T00:00:00Z")],
            )
            .unwrap();
        let result = store
            .push(
                "phone",
                vec![op("op-0", "phone", 0, "2026-07-09T00:00:00Z")],
            )
            .unwrap();
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(result.conflicts[0].winner, "op-1");
        assert_eq!(result.conflicts[0].loser, "op-0");

        let pulled = store.pull(0, 100).unwrap();
        assert_eq!(pulled.operations.len(), 1);
        assert_eq!(pulled.operations[0].op_id, "op-1");
    }

    #[test]
    fn parses_device_token_map() {
        let tokens = parse_device_tokens("desktop=abc,phone=def").unwrap();
        assert_eq!(tokens.get("desktop"), Some(&"abc".to_string()));
        assert_eq!(tokens.get("phone"), Some(&"def".to_string()));
    }

    #[test]
    fn sqlite_store_persists_operations_across_reopen() {
        let file = NamedTempFile::new().unwrap();
        {
            let store = SqliteStore::open(file.path()).unwrap();
            let result = store
                .push(
                    "desktop",
                    vec![op("op-sqlite-1", "desktop", 0, "2026-07-10T00:00:00Z")],
                )
                .unwrap();
            assert_eq!(result.server_cursor, 1);
        }

        let reopened = SqliteStore::open(file.path()).unwrap();
        assert_eq!(reopened.latest_cursor().unwrap(), 1);
        let pulled = reopened.pull(0, 100).unwrap();
        assert_eq!(pulled.operations.len(), 1);
        assert_eq!(pulled.operations[0].op_id, "op-sqlite-1");
    }

    #[test]
    fn sqlite_store_records_conflict_and_excludes_loser() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .push(
                "desktop",
                vec![op("op-sqlite-1", "desktop", 0, "2026-07-10T00:00:00Z")],
            )
            .unwrap();
        let result = store
            .push(
                "phone",
                vec![op("op-sqlite-0", "phone", 0, "2026-07-09T00:00:00Z")],
            )
            .unwrap();
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(result.conflicts[0].winner, "op-sqlite-1");

        let pulled = store.pull(0, 100).unwrap();
        assert_eq!(pulled.operations.len(), 1);
        assert_eq!(pulled.operations[0].op_id, "op-sqlite-1");
    }
}
