# Hive Mind

Hive Mind is the optional sync/API/mobile companion project for MindWeaver.

- Project folder/repository name: `hive-mind`
- Command/binary name: `hive`
- Source-of-truth MindWeaver workflow: local Markdown managed by Rust `mw`

## Architecture

Hive Mind is now split around a dedicated `hive` command boundary:

```text
hive-mind/
  crates/hive          Rust CLI entrypoint: `hive`
  crates/hive-core     shared diagnostics, protocol/client types, and future sync logic
  crates/hive-desktop  desktop SQLite outbox/cursor/todo bridge
  crates/hive-server   Rust sync API server with in-memory store
  hive-pwa/            Svelte/Vite mobile-web companion
  docs/                protocol, runbook, deployment, migration notes
  legacy/go-reference  copied Go oracle for parity checks during the port
```

The Rust CLI starts with operational glue rather than a full sync rewrite:

- `hive doctor` checks local project layout.
- `hive sync doctor --endpoint <url>` checks `/v1/sync/state`.
- `hive sync register --endpoint <url> --device-id <id>` registers a device.
- `hive sync push-file --endpoint <url> --file request.json` pushes operations.
- `hive sync pull --endpoint <url> --cursor 0` pulls operations.
- `hive sync local-doctor --db-path <notes.db>` inspects local sync tables.
- `hive sync run --db-path <notes.db> --endpoint <url> --device-id <id>` runs
  one desktop sync cycle.
- `hive serve` runs the Rust `hive-sync-api` replacement.
- `hive pwa check|build|dev|preview` runs the PWA package scripts.
- `hive version --short` prints the CLI version.

Longer-term, `hive` should absorb the legacy desktop sync client and possibly the
sync API server. Keep MindWeaver `mw` focused on local Markdown workflows.

## Current contents

- `plan.md` — product boundary, current capabilities, and resume checklist.
- `docs/architecture.md` — current architecture and migration path.
- `docs/protocol.md` — sync operation wire contract.
- `docs/hive-sync-progress.md` — implementation status and next steps.
- `docs/hive-sync-runbook.md` — operational runbook for the legacy sync stack.
- `docs/hive-sync-cloud-deploy.md` — Cloud Run / Cloud SQL deployment notes.
- `docs/hive-sync-mobile-onboarding.md` — PWA mobile setup and smoke checks.
- `docs/hive-sync-monitoring.md` — monitoring setup notes.
- `docs/hive-sync-backup-recovery.md` — backup/recovery notes.
- `docs/hive-sync-token-rotation-checklist.md` — token rotation checklist.

## Current implementation location

The PWA has moved into this project at `hive-pwa/`. A copied Go oracle lives in
`legacy/go-reference/` for parity checks. Some broader MindWeaver DB/client code
still lives in MindWeaver until the desktop bridge is fully ported:

- `legacy/go-reference/cmd/hiveSyncAPI`
- `legacy/go-reference/internal/features/syncapi`
- `legacy/go-reference/internal/features/syncclient`
- `hive-pwa/`

## Boundary

MindWeaver `mw` owns local Markdown notes, todos, querying, projections, and TUI workflows.
Hive owns optional remote operation sync, device auth, cloud API, mobile/PWA surfaces, and conflict transport.

The intended long-term CLI entrypoint is `hive`, not top-level `mw sync`.

## Development

```bash
cargo run -p hive -- doctor
cargo run -p hive -- version --short
cargo run -p hive -- serve --addr 127.0.0.1:8080 --device-tokens desktop=token --permissive-cors
cargo run -p hive -- pwa check
cargo run -p hive -- sync doctor --endpoint http://127.0.0.1:8080
cargo run -p hive -- sync register --endpoint http://127.0.0.1:8080 --device-id desktop --token token
cargo run -p hive -- sync pull --endpoint http://127.0.0.1:8080 --token token --cursor 0
cargo run -p hive -- sync local-doctor --db-path /path/to/notes.db
cargo run -p hive -- sync run --endpoint http://127.0.0.1:8080 --db-path /path/to/notes.db --device-id desktop --token token
```

Validate the Rust side with:

```bash
cargo fmt --all --check
cargo check --workspace
cargo test --workspace
```
