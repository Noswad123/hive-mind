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

- `docs/architecture.md` — architecture and component boundaries
- `docs/protocol.md` — sync API contract
- `docs/hive-sync-runbook.md` — operational examples
- `docs/hive-sync-mobile-onboarding.md` — PWA/mobile setup and smoke tests
- `docs/hive-sync-cloud-deploy.md` — Cloud Run deployment notes
- `docs/hive-sync-monitoring.md` — monitoring setup
- `docs/hive-sync-backup-recovery.md` — backup caveats and future durable-store notes
- `docs/hive-sync-token-rotation.md` — device bearer-token rotation procedure
- `docs/release.md` — build/install/release packaging path
- `legacy/go-reference/README.md` — Go oracle/reference notes
