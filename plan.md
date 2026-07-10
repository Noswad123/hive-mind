# Hive Mind Plan

Last updated: 2026-07-10

## Direction

Hive Mind can be treated as its own optional app suite instead of being fully
folded into the Rust `mw` notes CLI.

MindWeaver remains the local-first notes system. Hive Mind provides optional
multi-device sync, cloud API, and mobile/web companion surfaces around that
local notes system.

## Current priority

Hive Mind has been split into its own project with a dedicated Rust `hive` CLI
boundary. The first priority is not a full sync rewrite; it is to stabilize the
project shape, keep the PWA runnable, and define the bridge from MindWeaver's
Markdown/source-of-truth workflow into Hive's operation sync model.

Resume from the existing Go sync API/client/PWA behavior rather than redesigning
from scratch.

## Current components

### 1. Desktop MindWeaver / `mw`

- Rust `mw` is now the primary local notes CLI.
- Legacy Go `mw` remains buildable under `legacy/go` and is still the full sync
  client oracle.
- Rust `mw` does not expose a top-level `mw sync` command; local notes use
  `mw notes sync` / `mw seal`.
- The Rust database layer still has sync/outbox primitives that can be reused if
  Hive Sync returns under a non-conflicting boundary.
- The new Rust `hive` CLI now owns the HTTP sync boundary and has initial
  server/client transport parity. Desktop local-DB sync/apply behavior now has
  an initial Rust bridge for outbox push, cursor pull, entity-version tracking,
  and `sync_todos` application. The legacy Go client remains the oracle for
  remaining conflict artifact/review and Markdown materialization behavior.

### 2. `hive-sync-api`

Current Rust API service capabilities, ported from the Go oracle:

- Entrypoint: `hive serve`.
- HTTP endpoints:
  - `GET /healthz`
  - `GET /v1/sync/state`
  - `POST /v1/sync/push`
  - `GET /v1/sync/pull`
  - `POST /v1/devices/register`
- Storage backends:
  - in-memory store for local/dev usage
  - Postgres store remains to be reintroduced
- Device bearer-token auth middleware.
- CORS/preflight support for browser/PWA clients.
- Conflict behavior:
  - detects stale `base_version`
  - resolves by last-write-wins using `client_updated_at`
  - tie-breaks by `op_id`, then `device_id`
  - persists conflict events
  - excludes losing operations from pull streams
- Cloud deployment support:
  - Cloud Run container file
  - Cloud SQL bootstrap/deploy scripts
  - monitoring setup script
  - backup/export scripts

### 3. Rust desktop sync client / legacy Go oracle

Current desktop sync client capabilities:

- Rust `hive sync run` pushes local `sync_outbox` operations to `hive-sync-api`.
- Rust `hive sync run` pulls remote operations by cursor.
- Rust `hive sync run` persists local cursor in `sync_state`.
- Rust `hive sync run` tracks entity versions in `sync_entity_versions`.
- Rust `hive sync run` applies pulled todo operations into `sync_todos`.
- Pulled note operations are intentionally not materialized into Markdown yet.
- Remaining Go-oracle behavior includes transient retry/backoff, bounded worker
  mode, conflict artifacts/review, and richer token/keychain workflows.
- Observability counters include:
  - pushed accepted/rejected
  - pulled applied
  - conflicts logged
  - conflict artifact count
  - cursor lag
- Supports token input through:
  - explicit `--token`
  - `HIVE_SYNC_TOKEN`
  - `--token-command`
  - macOS Keychain via `--token-from-keychain`
- Supports conflict hygiene:
  - local conflict persistence
  - artifact writing
  - stale conflict review/export/mark-resolved workflow

### 4. `hive-pwa`

Current PWA capabilities:

- Svelte + Vite app shell.
- Web app manifest and service worker registration baseline.
- Endpoint/device/token setup UI.
- Token/device validation against the sync API.
- IndexedDB persistence for:
  - settings/config
  - sync metadata
  - draft sync queue
  - note records
  - todo records
  - local sync entity versions
  - conflict log entries
- Minimal note list/editor.
- Minimal todo capture/toggle/delete/edit surfaces.
- Manual sync action:
  - pushes queued local note/todo operations
  - pulls remote operations by cursor
  - applies pulled operations into IndexedDB state
  - updates local cursor and last-success metadata
- Sync health UI:
  - online/offline status
  - local applied cursor
  - observed remote cursor
  - observed server time
  - last remote check
  - last successful sync
- Sync trust signals:
  - local vs remote source labels
  - local-only / queued / synced / conflict states
  - synced-content counters
- Mobile conflict visibility:
  - open conflict banner
  - recent conflict list
  - winner/loser device metadata
  - local mark-reviewed action
- Notes/todos usability uplift:
  - search/filter
  - selected item detail panes
  - editable todo section/done/meta fields

## Current sync model

- Entity types: `note`, `todo`.
- Operation types: `upsert`, `delete`.
- Desktop/source local changes enqueue outbox rows.
- Clients push operations to the API with idempotency keys and base versions.
- Clients pull operations from the API by cursor.
- Conflict detection is version-based.
- Markdown remains the canonical MindWeaver source of truth on desktop.
- SQLite/IndexedDB are projections and sync caches.

## Known limitations

- Rust `mw` does not yet implement full HTTP push/pull sync.
- Phone-originated operations can sync into desktop-side local sync persistence,
  but should not yet be expected to create obvious Markdown file changes in the
  main notes workspace.
- Physical-device PWA smoke tests still require manual execution and evidence
  capture.
- The PWA is useful as a companion capture/review app, not yet a complete mobile
  replacement for the desktop Markdown workflow.
- Launchd/periodic desktop sync templates are deferred until real usage hardens
  scheduler expectations.

## Product boundary

Treat Hive Mind as an optional app suite with a stable integration contract:

- `mw` owns local Markdown notes, ingestion, querying, todos, projections, and
  the desktop source-of-truth workflow.
- Hive Sync owns remote operation exchange, cursoring, conflict events, device
  auth, and cloud/mobile sync surfaces.
- Hive PWA owns mobile/web capture, review, lightweight editing, sync health, and
  conflict visibility.
- The bridge is the sync operation protocol plus local projection stores.

This means the Rust port does not need to absorb every Hive feature before the
Go sync stack can remain useful. Hive can evolve as a separate app while sharing
the same operation model and storage contracts.

## Recommended next steps

1. Keep the legacy Go sync stack buildable as the full sync oracle until Rust
   `hive` reaches protocol/client parity.
2. Define the stable sync protocol contract in one place:
   - operation JSON shapes
   - idempotency key rules
   - cursor semantics
   - conflict event shape
   - auth requirements
3. Decide remote-to-Markdown materialization semantics before enabling automatic
   desktop writeback from phone-originated operations.
4. Prioritize validation over porting breadth:
   - run iOS/Android PWA smoke tests
   - verify Cloud Run sync state
   - verify desktop cursor advancement
   - verify conflict visibility and review flow
5. Port the desktop sync client into Rust `hive` incrementally:
   - remote doctor/state check
   - push pending outbox operations
   - pull operations by cursor
   - conflict persistence/artifacts
   - reviewed remote-to-Markdown import

## Resume checklist for later

Before doing new Hive work, quickly re-check the current implementation in
`/Users/jdawson/Projects/mind-weaver`:

1. `legacy/go/internal/features/syncclient/client.go`
2. `legacy/go/internal/features/syncapi/`
3. `legacy/go/internal/infra/db/sync_*.go`
4. `legacy/go/internal/mwcli/buildSync*.go`
5. `apps/hive-pwa/src/hiveSyncEngine.ts`
6. `apps/hive-pwa/README.md`

New work should generally land in this project under the Rust `hive` command
boundary unless it is a MindWeaver local-Markdown concern.

## Reference docs

- `docs/hive-sync-progress.md`
- `docs/hive-sync-runbook.md`
- `docs/hive-sync-cloud-deploy.md`
- `docs/hive-sync-monitoring.md`
- `docs/hive-sync-backup-recovery.md`
- `docs/hive-sync-mobile-onboarding.md`
- `/Users/jdawson/Projects/mind-weaver/apps/hive-pwa/README.md`
