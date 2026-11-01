# Hive Mind

Hive Mind is the sync/API/mobile companion project for MindWeaver.

- Repository/folder: `hive-mind`
- CLI/binary: `hive`
- MindWeaver remains Markdown-first; Hive handles optional remote sync, device
  auth, the sync API, and the PWA companion.

## What is in this repo

```text
hive-mind/
  crates/hive          CLI entrypoint: `hive`
  crates/hive-capture  desktop-local Google Tasks inbound queue and mw delivery
  crates/hive-background bounded worker, editor coordination, and owned macOS LaunchAgent
  crates/hive-google-auth desktop Google OAuth, refresh, and native macOS Keychain storage
  crates/hive-core     shared protocol/client/diagnostic types
  crates/hive-desktop  desktop SQLite bridge for MindWeaver sync tables
  crates/hive-server   Rust sync API server used by `hive serve`
  hive-pwa/            Svelte/Vite browser/mobile companion
  scripts/cloud/       Cloud Run build/deploy/monitoring/backup helpers
  docs/                architecture, protocol, runbooks, deployment, release docs
  legacy/go-reference  copied Go implementation used as a behavior oracle
```

## Build and install

```bash
make validate
make build
make install        # installs to ~/.local/bin/hive by default
```

Without `make`:

```bash
cargo build --release -p hive
install -m 0755 target/release/hive ~/.local/bin/hive
```

See `docs/release.md` for the current release/package path.

## Google Tasks inbound capture (first personal slice)

The desktop-local `hive captures` lane and bounded background worker are implemented
and validated: on manual invocation or an explicitly activated worker cycle, read a
complete Google Tasks list → private dedicated SQLite queue → existing `mw todos add`
with stable capture ID → durable canonical receipt. Google retains phone captures
while the desktop is off; Hive does not acknowledge them until it runs.

See [Google Tasks capture](docs/google-tasks-capture.md) for commands, bootstrap
choices, unknown-outcome recovery, validation evidence, and activation gates.
Desktop OAuth login, account-bound refresh, and native macOS Keychain storage are
implemented; see [Google sign-in setup](docs/google-auth.md). Manual live capture
from the dedicated Android Google Tasks list into Markdown is verified, including
a repeat run with no duplicate or source rewrite. The stable release was built and
installed at `/Users/jdawson/.local/bin/hive` on 2026-10-31 and rebuilt/installed
there after the 2026-11-01 inventory fixes. On 2026-11-01 the user reconfirmed
latest-binary foreground `auth status` and `lists --connection personal` success.
The existing personal queue/list/baseline is configured with fixed public routing,
MindWeaver cwd, interval 60 seconds, timeout 45 seconds, and limit 1; configuration
and queue scope checks passed. The clean/idle live editor was hotloaded in place,
preserving `mw_ops` module/active-guard identity, and the bridge registered.
**Scheduled polling is enabled and verified:** two controlled `run-once` cycles
completed in about 0.5 seconds each, followed by explicit background install/start.
The owned LaunchAgent reports `serviceActive: true`, `serviceState: loaded_owned`;
its startup and next scheduled cycle completed about 60.7 seconds apart. Each of
these four cycles observed one unchanged task, staged/delivered zero, and required
no auth action (controlled cycles: generation 1). Latest-build no-UI Keychain access
is positively proven while unlocked, not for denial/locked scenarios. Earlier
45-second unknown outcome and 12-second local status stall did not reproduce;
their root cause is not established and no speculative source fix was made.
Editor readiness is `ready: true`, `noEditor: false`; a live unsaved hidden-buffer
probe verified `dirty_buffers` readiness deferral, then clean readiness was restored
without note edits or guard changes. The existing receipt remains at attempt 1,
with no duplicates or Inbox/dashboard hash, byte, or mtime changes.
New phone-task-to-background-source-write delivery, full pending-job dirty-buffer
delivery deferral, and live computer-off catch-up are not yet live-verified.
Source-write/guarded delivery has isolated integration and earlier manual capture
proof only. Polling runs in the GUI user session; Google holds work while the
computer is off, with no server required. See the runbook for stable-path
`background status`, `stop`, `start`, and `uninstall` controls; stop retains config
and queue, and uninstall removes only the owned plist while retaining all state.
Latest validation: 270 normal workspace tests passed (five ignored), strict
background/auth/capture Clippy passed, and both ignored real-Neovim tests passed
after the fixes. Real-writer integrations passed earlier, not rerun after the
fixes; earlier format and Neovim background/safety checks (32/54 tests) also passed.
No additional tests ran during activation; these isolated checks are separate from
the live scheduled-polling proof.
`lists`/`poll`/`run` now require exactly one explicit `--connection NAME` or
`--access-token-env HIVE_GOOGLE_ACCESS_TOKEN`; saved connections forbid `--account-key`.
Calendar, export, completion writeback, lifelog, and PWA migration remain pending.
This lane does not use or change legacy LWW sync/PWA transport and
does not require a hosted server or Postgres before first personal use.

## Start a local sync API

For local browser/PWA development:

```bash
hive serve \
  --addr 127.0.0.1:8080 \
  --device-tokens desktop=desktop-token,phone=phone-token \
  --permissive-cors
```

For local durable smoke testing, use SQLite:

```bash
hive serve \
  --addr 127.0.0.1:8080 \
  --store sqlite \
  --sqlite-path .local/hive-sync.db \
  --device-tokens desktop=desktop-token,phone=phone-token \
  --permissive-cors
```

Server endpoints:

- `GET /healthz`
- `GET /v1/sync/state`
- `POST /v1/devices/register`
- `POST /v1/sync/push`
- `GET /v1/sync/pull?cursor=<cursor>&limit=<limit>`

## Validate tokens and API health

```bash
hive sync token check \
  --endpoint http://127.0.0.1:8080 \
  --device-id desktop \
  --token desktop-token

hive sync doctor \
  --endpoint http://127.0.0.1:8080 \
  --device-id desktop \
  --token desktop-token
```

Token sources supported by sync commands:

- `--token <value>`
- `HIVE_SYNC_TOKEN`
- `--token-command '<command that prints token>'`
- macOS Keychain via `--token-from-keychain`

Store a token in macOS Keychain:

```bash
printf '%s' 'desktop-token' | hive sync token store \
  --device-id desktop \
  --token-stdin
```

## Run desktop sync

`hive sync run` bridges a MindWeaver-compatible SQLite database to the Hive sync
API. It reads pending `sync_outbox` rows, pushes them to the API, pulls remote
operations by cursor, updates local sync state/entity versions, applies pulled
todos to `sync_todos`, and stages pulled notes for manual review.

```bash
hive sync local-doctor --db-path /path/to/notes.db

hive sync run \
  --endpoint http://127.0.0.1:8080 \
  --db-path /path/to/notes.db \
  --device-id desktop \
  --token desktop-token
```

Run a bounded worker with retry/backoff:

```bash
hive sync run \
  --endpoint http://127.0.0.1:8080 \
  --db-path /path/to/notes.db \
  --device-id desktop \
  --token desktop-token \
  --worker-iterations 3 \
  --worker-interval 10s \
  --retry-attempts 4
```

Drain the currently pending local outbox:

```bash
hive sync run \
  --endpoint http://127.0.0.1:8080 \
  --db-path /path/to/notes.db \
  --device-id desktop \
  --token desktop-token \
  --until-empty
```

## Review staged remote notes

Hive does **not** overwrite canonical Markdown automatically. Pulled remote note
operations are staged in `sync_note_imports` and can be exported to a generated
inbox for manual review/import.

```bash
hive sync notes review --db-path /path/to/notes.db

hive sync notes review \
  --db-path /path/to/notes.db \
  --export-dir .local/remote-note-inbox \
  --mark-reviewed
```

## Review conflicts

```bash
hive sync conflicts review \
  --db-path /path/to/notes.db \
  --export-dir .local/conflict-reviews \
  --artifacts-dir .local/conflicts

hive sync conflicts review \
  --db-path /path/to/notes.db \
  --export-dir .local/conflict-reviews \
  --mark-resolved
```

`--mark-resolved` requires an export destination so review state has an artifact.

## Run the PWA

```bash
hive pwa check
hive pwa dev
```

Open:

- desktop browser: `http://localhost:5173`
- phone on same LAN: `http://<machine-lan-ip>:5173`

Use endpoint `http://127.0.0.1:8080` from the desktop browser, or
`http://<machine-lan-ip>:8080` from a phone. Use device ID `phone` and its mapped
token, for example `phone-token`.

The PWA stores notes/todos/settings/queued sync operations in browser IndexedDB.
Manual sync pushes queued phone edits and pulls remote operations.

## Cloud and containers

Build the local image when a Docker-compatible runtime is available:

```bash
make docker-build IMAGE=hive-sync-api:local
```

Cloud Run scripts live in `scripts/cloud/`. Current Rust `hive serve` supports:

- `HIVE_STORE=memory` for smoke checks; not durable.
- `HIVE_STORE=sqlite` for single-instance experiments; not a multi-instance
  production backend without a persistent volume and backup strategy.

Cloud SQL/Postgres support is documented as a future/deferred backend. See
`docs/hive-sync-cloud-deploy.md` and `docs/hive-sync-backup-recovery.md` for the
current operator guidance.

## Safety boundaries

- MindWeaver `mw` owns canonical local Markdown workflows.
- Hive owns remote operation exchange, device auth, sync API, and PWA/mobile
  surfaces.
- Pulled todos are applied to `sync_todos`.
- Pulled notes are staged for review and are not written to canonical Markdown
  automatically.
- SQLite/IndexedDB are projections and sync caches; Markdown remains the desktop
  source of truth.

## Documentation

- `docs/google-tasks-capture.md` — implemented local inbound lane and live activation gates
- `docs/google-auth.md` — Desktop OAuth setup, saved connections, refresh, and troubleshooting
- `docs/architecture.md` — architecture and component boundaries
- `docs/product-architecture-roadmap.md` — product architecture needed for real
  multi-device note/todo sync and future MindWeaver integration
- `docs/protocol.md` — sync API contract
- `docs/hive-sync-runbook.md` — operational examples
- `docs/hive-sync-mobile-onboarding.md` — PWA/mobile setup and smoke tests
- `docs/hive-sync-cloud-deploy.md` — Cloud Run deployment notes
- `docs/hive-sync-monitoring.md` — monitoring setup
- `docs/hive-sync-backup-recovery.md` — backup caveats and future durable-store notes
- `docs/hive-sync-token-rotation.md` — device bearer-token rotation procedure
- `docs/release.md` — build/install/release packaging path
- `legacy/go-reference/README.md` — Go oracle/reference notes
