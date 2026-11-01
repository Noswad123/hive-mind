# Hive Mind Status and Roadmap

Last updated: 2026-11-01

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

### Google Tasks inbound capture: scheduled polling enabled and verified

`hive-capture` and `hive captures` now provide the first personal vertical slice:
user-invoked full paginated read-only Google snapshot → dedicated private SQLite
queue → existing `mw todos add` with stable ID → durable canonical receipt.
Google holds phone captures while the desktop is off; there is no Hive/server ACK
then. Legacy LWW sync remains unchanged and unused for this lane.

The requested next sign-in/refresh slice is also implemented: `hive-google-auth`
provides Desktop PKCE login, verified Google subject binding, once-per-command
refresh, and native non-synchronizing macOS Keychain storage. Queue schema V2 adds
transactional identity bindings without rewriting existing capture IDs or receipts.
Same-list verified/legacy mixing is refused in both directions, including empty
legacy baselines; explicit legacy-binding migration is future work, not automatic adoption.

See [Google sign-in setup](docs/google-auth.md) for connection/security details and
[the capture runbook](docs/google-tasks-capture.md) for bootstrap/reconciliation.
Latest validation after the inventory fixes: 270 normal workspace tests passed,
five ignored by default; strict all-targets Clippy passed for background/auth/capture.
Both ignored real isolated Neovim tests passed after the fixes. Both real-writer
integrations (real mw CLI; fake Google with real Hive/mw) passed earlier and were
not rerun after these fixes. The fifth ignored crash fixture is exercised by
normal recovery tests. Earlier format and Neovim background/safety checks (32/54
tests) passed on 2026-10-31. These are not unattended live proof.
Earlier manual activation verified live browser authorization, native Keychain
persistence, and refresh/account verification for the `personal` connection.

Google Tasks list access now succeeds after API enablement. The dedicated
`MindWeaver Inbox` list was baselined empty; its private queue is initialized under
`~/Library/Application Support/Hive/`. One new undated Android task has now been
delivered into `introspection/inbox.md` exactly once. A repeat complete poll/delivery
observed the same task unchanged, staged/delivered zero tasks, and left Inbox and
dashboard bytes and mtimes unchanged. The stored receipt retains one writer attempt;
Neovim's guard was released without buffer conflicts. The task remains open.
The provided root Desktop client file is Git-ignored. The stable release was
successfully built/installed at `/Users/jdawson/.local/bin/hive` on 2026-10-31 and
rebuilt/installed there after the 2026-11-01 inventory fixes. The user reconfirmed
latest-binary foreground `auth status` and `lists --connection personal` success.
Latest-build no-UI background Keychain access is positively proven while unlocked,
not for denial/locked scenarios. Queue confirmation retained one
delivered receipt and one writer attempt; preserve the existing connection,
queue/list/baseline and manual capture/repeat proof.

`hive-background` implements frozen public routing, fixed OS-user-home state,
no-UI background auth with generation-based refresh pause, cooperative editor
admission/guarded delivery, supervised shutdown, and owned launchd lifecycle.
Defaults are a 60-second interval, 45-second cycle budget, and delivery limit 1.
The existing personal queue/list/baseline is configured with fixed public
environment and MindWeaver cwd; `configValid` and `queueScopeValid` are true. After
clean/idle checks, `mw_ops` was hotloaded in place preserving module identity and
the active upvalue guard across four shared references; the live editor bridge
registered. SDK inventory fixes narrowly verify zombies via SDK-typed C `sysctl`
only on the inventory-error path and normalize Darwin signed UID `-2` with checked
32-bit conversion. They do not ignore generic errors or change the group-death
guard; review found no P1/P2 issues. The inherited-FD test uses an isolated
single-test child to avoid parallel fork-test leakage; original assertions and
production semantics are unchanged.

Installed editor readiness is `ready: true`, `noEditor: false`, `deferred: null`.
A live readiness probe using an owned unsaved hidden read-only normal buffer
verified `dirty_buffers` deferral without an on-disk file or actual note edits.
The guard remained unchanged with `busy: false`; after changed-tick, name, and
visibility checks the probe was removed and clean readiness restored. This proves
readiness deferral, not live full pending-job dirty-buffer delivery deferral.

Two controlled `run-once` cycles completed in about 0.5 seconds each, observing
one unchanged task, staging/delivering zero, with `authActionRequired: false` and
generation 1. Explicit background install/start then enabled the owned LaunchAgent:
`serviceActive: true`, `serviceState: loaded_owned`. Its actual startup and next
scheduled cycle completed about 60.7 seconds apart, each with no pending work,
one unchanged observation, zero staged/delivered, and no auth action required.
Native LaunchAgent ownership/status parsing and scheduled polling are live-verified.
Inbox/dashboard hashes, bytes, and mtimes remain unchanged; the receipt is still
attempt 1 with no duplicates. Earlier 45-second `unknown_outcome` and 12-second
local status stall did not reproduce; bounded inherited/minimal-environment local
status diagnostics succeeded in 0.054/0.028 seconds. The credential-operation
failure is resolved operationally, not root-caused; the same build/credentials work
and no speculative source fix was made. No additional tests ran during activation.

Polling is running in the GUI user session at 60 seconds, with a 45-second timeout
and limit 1. Google holds work while the computer is off; no server is required.
Use stable-path `background status`, `stop`, `start`, and `uninstall` controls.
Stop retains config/queue; uninstall removes only the owned plist and retains all
state. New phone-task-to-background-source-write delivery and live computer-off
catch-up remain unverified; source-write/guarded delivery has isolated integration
and earlier manual capture proof only. Calendar, export/completion writeback,
lifelog, and PWA migration remain pending.

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

1. Retain enabled, verified [scheduled polling](docs/google-tasks-capture.md) and
   the existing connection, queue, baseline, and receipts. Separately verify a new
   phone task reaching Markdown through background delivery, full pending-job
   dirty-buffer deferral, and live computer-off catch-up. No Postgres prerequisite.
2. For broader hosted sync, add a cloud-suitable durable backend, likely Postgres/Cloud SQL.
3. Improve reviewed remote-note import UX from generated inbox into canonical
   Markdown workflows.
4. Add richer server/client observability, including cursor lag and queue aging.
5. Add systemd/broader desktop sync worker guidance; the narrow macOS Google Tasks
   LaunchAgent lifecycle is implemented and activated live.
6. Add package-manager distribution once the CLI surface stabilizes.
7. Expand PWA conflict review and background/offline retry behavior where browser
   support allows it.

## Reference implementation

`legacy/go-reference/` contains the copied Go implementation used as a parity
oracle when Rust behavior is unclear. New active work should land in this repo
under the `hive` command boundary unless it is a MindWeaver local-Markdown concern.
