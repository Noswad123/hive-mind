# Hive Mind Status and Roadmap

Last updated: 2026-07-11

Hive Mind is the optional sync/API/PWA companion for MindWeaver. MindWeaver keeps
canonical local Markdown workflows; Hive exchanges operations between devices and
offers mobile/web companion surfaces.

## Current production boundary

- `mw` owns local Markdown notes, local todo projection, ingestion, search, and
  desktop source-of-truth workflows.
- `hive` owns remote operation sync, device bearer-token auth, sync diagnostics,
  conflict transport, and cloud/PWA/mobile integration.
- `hive-pwa` owns browser/mobile capture, lightweight editing, local IndexedDB
  state, manual sync, and sync health/conflict visibility.

## Implemented capabilities

### CLI and server

- `hive serve` runs the Rust sync API.
- Server endpoints:
  - `GET /healthz`
  - `GET /v1/sync/state`
  - `POST /v1/devices/register`
  - `POST /v1/sync/push`
  - `GET /v1/sync/pull`
- Server store backends:
  - in-memory store for smoke/dev usage
  - SQLite store for local durable smoke/dev usage
- Device bearer-token auth.
- Permissive CORS mode for local browser/PWA development.
- Last-write-wins conflict resolution for stale `base_version` pushes.

### Desktop sync bridge

- `hive sync run` pushes local `sync_outbox` rows.
- Pulls remote operations by cursor.
- Persists local cursor in `sync_state`.
- Tracks local entity versions in `sync_entity_versions`.
- Applies pulled todo operations into `sync_todos`.
- Stages pulled note operations into `sync_note_imports`.
- Supports transient retry/backoff, bounded worker iterations, and `--until-empty`.
- `hive sync notes review` exports staged note imports to generated Markdown inbox
  files and can mark them reviewed.
- `hive sync conflicts review` exports conflict review snapshots, writes
  per-conflict artifacts, and can mark conflicts resolved.
- Token sources: explicit token, env var, command output, and macOS Keychain.

### PWA

- Svelte/Vite app shell with service worker and manifest.
- Endpoint/device/token setup UI.
- Token validation and remote sync-state checks.
- IndexedDB-backed settings, notes, todos, queue, sync metadata, and conflicts.
- Manual push/pull sync against Rust `hive serve`.
- Mobile note/todo creation and editing baseline.
- Sync health and conflict visibility surfaces.

### Cloud/deployment

- Dockerfile for `hive serve` container image.
- Cloud Build config and Cloud Run deploy script.
- Monitoring setup for `/healthz` uptime and Cloud Run 5xx alerting.
- Backup docs describe current limitations and deferred durable backend work.

## Safety boundaries

- Pulled notes are not written directly to canonical Markdown.
- Remote notes are staged for review and optional generated inbox export.
- `HIVE_STORE=memory` is not durable.
- Cloud Run SQLite mode is a single-instance smoke/development option unless a
  real persistent volume and backup strategy are added.
- Cloud SQL/Postgres support is not implemented in Rust `hive serve` yet.

## Roadmap

For the full product architecture and phased roadmap toward real multi-device
note/todo sync, see `docs/product-architecture-roadmap.md`.

1. Add a cloud-suitable durable server backend, likely Postgres/Cloud SQL.
2. Improve reviewed remote-note import UX from generated inbox into canonical
   Markdown workflows.
3. Add richer server/client observability, including cursor lag and queue aging.
4. Add launchd/systemd examples for periodic desktop sync workers.
5. Add package-manager distribution once the CLI surface stabilizes.
6. Expand PWA conflict review and background/offline retry behavior where browser
   support allows it.

## Reference implementation

`legacy/go-reference/` contains the copied Go implementation used as a parity
oracle when Rust behavior is unclear. New active work should land in this repo
under the `hive` command boundary unless it is a MindWeaver local-Markdown concern.
