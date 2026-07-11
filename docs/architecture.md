# Hive Mind Architecture

## Boundary

Hive Mind is a companion system around MindWeaver, not a replacement for
MindWeaver's local Markdown workflow.

- `mw` owns local notes, todos, querying, projections, and Markdown writeback.
- `hive` owns remote operation exchange, device auth, sync diagnostics, PWA
  orchestration, and eventually the desktop sync bridge.
- `hive-pwa` owns mobile/web capture, lightweight editing, local IndexedDB state,
  and sync trust/conflict visibility.

## Current Rust workspace

```text
crates/hive
  CLI shell and command routing.

crates/hive-core
  Shared project diagnostics, sync API response types, and future protocol/client
  logic.

crates/hive-desktop
  Desktop bridge for MindWeaver-compatible SQLite sync tables: reads pending
  `sync_outbox`, pushes operations, records ack/reject/conflict state, pulls by
  cursor, persists `sync_state`, updates `sync_entity_versions`, and applies
  pulled todo operations into `sync_todos`. Pulled note operations are staged in
  `sync_note_imports` and optionally exported as generated inbox files for manual
  Markdown review. Also owns local conflict review, export, artifact writing, and
  mark-resolved workflows.

crates/hive-server
  Rust sync API server. Backends include in-memory and SQLite. Behavior is ported
  from the legacy Go API: push/pull cursors, device token auth, stale
  base-version conflict detection, LWW conflict resolution, and CORS support for
  PWA dev.

hive-pwa
  Svelte/Vite PWA.

legacy/go-reference
  Copied Go oracle/reference for parity checks while Rust absorbs the old Hive
  sync behavior.
```

## Initial CLI surface

```bash
hive doctor
hive version --short
hive sync doctor --endpoint http://127.0.0.1:8080
hive sync register --endpoint http://127.0.0.1:8080 --device-id desktop --token token
hive sync push-file --endpoint http://127.0.0.1:8080 --token token --file push-request.json
hive sync pull --endpoint http://127.0.0.1:8080 --token token --cursor 0
hive sync local-doctor --db-path /path/to/notes.db
hive sync run --endpoint http://127.0.0.1:8080 --db-path /path/to/notes.db --device-id desktop --token token
hive sync notes review --db-path /path/to/notes.db --export-dir .local/remote-note-inbox --mark-reviewed
hive sync conflicts review --db-path /path/to/notes.db --export-dir .local/conflict-reviews --artifacts-dir .local/conflicts
hive serve --addr 127.0.0.1:8080 --device-tokens desktop=token --permissive-cors
hive serve --addr 127.0.0.1:8080 --store sqlite --sqlite-path .local/hive-sync.db --device-tokens desktop=token --permissive-cors
hive pwa check
hive pwa build
hive pwa dev
hive pwa preview
```

## Migration path

1. Keep legacy Go sync behavior as the oracle.
2. Stabilize the protocol contract in `docs/protocol.md` and Rust types.
3. Continue porting desktop sync client behavior into `hive`:
   - done: list/push pending outbox operations from MindWeaver's local SQLite DB
   - done: pull remote operations by cursor
    - done: persist cursor and entity versions
    - done: apply pulled todo operations into `sync_todos`
    - done: conflict artifact files and review workflow
    - done: stage pulled note operations and export generated review inbox files
 4. Keep automatic canonical Markdown writeback disabled until the reviewed import
    UX is proven safe in daily use.
5. Remaining server hardening: evaluate whether Cloud deployment should use
   SQLite with a mounted volume or a future Postgres/Cloud SQL backend.
