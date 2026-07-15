# Hive Mind Architecture

Hive Mind is a companion system around MindWeaver. It provides remote operation
exchange and mobile/web surfaces without replacing MindWeaver's local Markdown
source-of-truth workflow.

## Component boundaries

- `mw` owns canonical local Markdown notes, local indexing/projections, local
  todos, and desktop workflows.
- `hive` owns remote sync transport, device auth, the sync API, desktop sync
  bridge diagnostics, conflict transport, and PWA orchestration.
- `hive-pwa` owns browser/mobile capture, lightweight editing, IndexedDB state,
  and manual sync UX.

## Rust workspace

```text
crates/hive
  CLI entrypoint and command routing.

crates/hive-core
  Shared protocol types, blocking HTTP client helpers, token-check report types,
  and project diagnostics.

crates/hive-desktop
  Desktop bridge for MindWeaver-compatible SQLite sync tables.
  Reads pending `sync_outbox`, pushes operations, records ack/reject/conflict
  state, pulls by cursor, persists `sync_state`, updates `sync_entity_versions`,
  applies pulled todos into `sync_todos`, stages pulled notes in
  `sync_note_imports`, and exports note/conflict review artifacts.

crates/hive-server
  Rust sync API server. Backends include in-memory and SQLite. Implements
  push/pull cursors, device-token auth, stale base-version conflict detection,
  LWW conflict resolution, loser exclusion, and permissive CORS for PWA dev.

hive-pwa
  Svelte/Vite PWA backed by browser IndexedDB.

legacy/go-reference
  Copied Go oracle/reference for behavior parity.
```

## Sync data flow

```text
desktop MindWeaver SQLite sync tables
  sync_outbox ──push──▶ hive serve ──pull──▶ other devices / PWA
  sync_state ◀─cursor── hive serve
  sync_todos ◀─todo ops
  sync_note_imports ◀─note ops staged for review
```

The PWA has its own IndexedDB projections and draft queue. It pushes queued note
and todo operations to the same API and pulls operations by cursor.

## Server storage modes

- `memory`: simplest local/Cloud Run smoke mode; state is lost on restart.
- `sqlite`: local durable smoke/dev mode; Cloud Run usage requires single-instance
  constraints or a real persistent volume.
- Postgres/Cloud SQL: not implemented in Rust yet; deployment docs call this out
  as deferred work.

## Conflict model

- Clients include `base_version` in pushed operations.
- The server detects stale versions.
- Resolution is last-write-wins using `client_updated_at`, then `op_id`, then
  `device_id` tie-breaks.
- Losing operations are recorded as conflicts and excluded from pull streams.
- Desktop conflict review/export is handled by `hive sync conflicts review`.

## Note materialization safety

Pulled note operations are staged, not applied directly to Markdown. The desktop
bridge writes staged rows to `sync_note_imports`; operators can export generated
Markdown inbox files with `hive sync notes review --export-dir ...`. This keeps
remote/mobile changes reviewable before they affect canonical local notes.

## CLI surface

```bash
hive doctor
hive version --short
hive serve --addr 127.0.0.1:8080 --device-tokens desktop=token --permissive-cors
hive sync doctor --endpoint http://127.0.0.1:8080 --device-id desktop --token token
hive sync token check --endpoint http://127.0.0.1:8080 --device-id desktop --token token
hive sync run --endpoint http://127.0.0.1:8080 --db-path /path/to/notes.db --device-id desktop --token token
hive sync notes review --db-path /path/to/notes.db --export-dir .local/remote-note-inbox --mark-reviewed
hive sync conflicts review --db-path /path/to/notes.db --export-dir .local/conflict-reviews --artifacts-dir .local/conflicts
hive pwa check
hive pwa dev
```
