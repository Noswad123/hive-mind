# Hive Mind Port Checklist

Use this checklist as the chunk queue. When you say **next**, take the next
unchecked chunk from top to bottom.

## Active chunks

- [x] Persistent server backend for `hive serve`
  - [x] Add a durable store abstraction behind the Rust sync API.
  - [x] Add SQLite persistent backend as the local/dev durable store.
  - [x] Keep in-memory backend available for tests/smoke runs.
  - [x] Update `hive serve` backend selection and docs.
- [x] Conflict artifact/review workflow
  - [x] Write conflict JSON artifacts.
  - [x] List stale unresolved conflicts.
  - [x] Export review snapshots.
  - [x] Mark reviewed conflicts resolved.
- [x] Token and secret workflow
  - [x] Add `hive sync token check`.
  - [x] Add `--token-command` support.
  - [x] Add macOS Keychain store/read support.
- [x] Retry and worker mode
  - [x] Add transient retry/backoff around sync cycles.
  - [x] Add bounded worker mode with interval/iteration options.
  - [x] Add aggregate worker result output.
- [x] Remote note materialization design and implementation
  - [x] Define safe import/staging behavior for pulled note operations.
  - [x] Avoid silent overwrites of canonical Markdown.
  - [x] Add reviewed import flow or generated inbox target.
- [x] PWA integration pass
  - [x] Point PWA defaults/docs at Rust `hive serve`.
  - [x] Run automated PWA check/build and Rust API endpoint smoke.
  - [x] Run browser manual sync against Rust API.
  - [x] Run physical-device smoke checklist.
- [x] Cloud deployment hardening
  - [x] Verify Docker build for Rust `hive serve`.
    - Verified after installing/running a Docker-compatible runtime via Colima.
  - [x] Reconcile Cloud Run scripts with selected persistent backend.
  - [x] Update monitoring/backup docs for the Rust server.
- [x] Package/repo setup
  - [x] Make initial git commit.
    - Existing initial commit: `1181bc4 first commit`. Current chunk changes are
      intentionally uncommitted until an explicit commit request.
  - [x] Add install/build convenience target.
  - [x] Decide release/package path for the `hive` binary.

## Current safety boundary

- `hive sync run` may push local `sync_outbox`, pull remote operations, update
  local cursor/entity versions, and apply pulled todos to `sync_todos`.
- Pulled notes are staged in `sync_note_imports` and can be exported to a
  generated review inbox; they are not written to canonical Markdown automatically.
- Use `legacy/go-reference/` as the oracle when behavior is unclear.
