# Hive Sync Runbook (MVP)

> Status: parked legacy Hive Sync notes. The Rust `mw` CLI no longer supports
> top-level `mw sync`; local notes use `mw notes sync` / `mw seal`. Revisit these
> commands only if Hive Sync returns under the future `hive` command boundary.

This runbook covers day-to-day operation for `hive-sync-api`. The active command
boundary is now Rust `hive`; old `mw sync` examples are legacy references until
the desktop sync client is fully ported.

## 1) Start the sync API locally

```bash
cargo run -p hive -- serve --addr 127.0.0.1:8080
```

For local durable persistence across restarts:

```bash
cargo run -p hive -- serve \
  --addr 127.0.0.1:8080 \
  --store sqlite \
  --sqlite-path .local/hive-sync.db
```

Optional auth-hardening env:

```bash
export HIVE_SYNC_DEVICE_TOKENS="desktop=token-desktop,phone=token-phone"
export HIVE_SYNC_PERMISSIVE_CORS=true

cargo run -p hive -- serve --addr 127.0.0.1:8080
```

Check the running API:

```bash
hive sync doctor --endpoint http://127.0.0.1:8080 --token token-desktop
hive sync register --endpoint http://127.0.0.1:8080 --device-id desktop --token token-desktop
hive sync pull --endpoint http://127.0.0.1:8080 --token token-desktop --cursor 0
```

Run one desktop bridge cycle against a MindWeaver-compatible notes database:

```bash
hive sync local-doctor --db-path /path/to/notes.db
hive sync run \
  --endpoint http://127.0.0.1:8080 \
  --db-path /path/to/notes.db \
  --device-id desktop \
  --device-name "Desktop" \
  --token token-desktop
```

Current safety boundary: pulled `todo` operations are applied into `sync_todos`.
Pulled `note` operations are staged in `sync_note_imports` and can be exported as
generated review inbox files; they are not written to canonical Markdown
automatically.

Review staged remote note imports:

```bash
hive sync notes review --db-path /path/to/notes.db
```

Export generated inbox files for manual review, then mark the staged imports
reviewed:

```bash
hive sync notes review \
  --db-path /path/to/notes.db \
  --export-dir .local/remote-note-inbox \
  --mark-reviewed
```

Review/export stale local conflicts:

```bash
hive sync conflicts review \
  --db-path /path/to/notes.db \
  --export-dir .local/conflict-reviews \
  --artifacts-dir .local/conflicts
```

After successful export/triage, mark included unresolved conflicts resolved:

```bash
hive sync conflicts review \
  --db-path /path/to/notes.db \
  --export-dir .local/conflict-reviews \
  --mark-resolved
```

## 2) Run one bounded sync cycle

```bash
hive sync run \
  --endpoint http://127.0.0.1:8080 \
  --db-path /path/to/notes.db \
  --device-id desktop \
  --token token-desktop
```

Expected observability logs include:
- push/pull counters
- conflict counts + conflict rate
- local/server cursor + lag

## 3) Run bounded worker mode

```bash
hive sync run \
  --endpoint http://127.0.0.1:8080 \
  --db-path /path/to/notes.db \
  --device-id desktop \
  --token token-desktop \
  --worker-iterations 12 \
  --worker-interval 10s \
  --retry-attempts 4 \
  --retry-base-delay 500ms \
  --retry-max-delay 8s
```

This runs a finite worker loop and exits after the requested iteration count.

To drain the currently pending local outbox in bounded batches, use:

```bash
hive sync run \
  --endpoint http://127.0.0.1:8080 \
  --db-path /path/to/notes.db \
  --device-id desktop \
  --token token-desktop \
  --until-empty
```

`--until-empty` calculates the required iterations from current pending outbox
count and `--outbox-limit`; raise `--until-empty-max-iterations` if the safety
cap blocks a large drain. Text output includes completed/requested worker cycles
and aggregate counters. JSON output returns `{ iterations_requested,
iterations_completed, aggregate, last }`.

### Token input options for `hive sync` commands

You can provide bearer tokens in four ways:

1. `--token <value>`
2. `HIVE_SYNC_TOKEN` env var
3. `--token-command '<shell command that prints token>'`
4. `--token-from-keychain` (macOS Keychain)

`HIVE_SYNC_API_URL`, `HIVE_SYNC_DEVICE_ID`, `HIVE_SYNC_TOKEN_COMMAND`,
`HIVE_SYNC_TOKEN_FROM_KEYCHAIN`, `HIVE_SYNC_TOKEN_KEYCHAIN_SERVICE`, and
`HIVE_SYNC_TOKEN_KEYCHAIN_ACCOUNT` are also supported on matching flags.

If you use Keychain mode:

- default service: `mw/hive-sync`
- default account: `--device-id`

Store token in Keychain safely from stdin:

```bash
printf '%s' '<token-value>' | hive sync token store --token-stdin --device-id desktop
```

Use token from Keychain:

```bash
hive sync run \
  --endpoint https://<service-url> \
  --db-path /path/to/notes.db \
  --device-id desktop \
  --token-from-keychain
hive sync doctor --endpoint https://<service-url> --device-id desktop --token-from-keychain
```

Validate token/device mapping explicitly:

```bash
hive sync token check \
  --endpoint https://<service-url> \
  --device-id desktop \
  --token-from-keychain
```

Fetch token dynamically at runtime with `--token-command` (no token stored in shell history):

```bash
hive sync doctor \
  --endpoint https://<service-url> \
  --token-command "gcloud secrets versions access latest --secret hive-sync-api-device-tokens --project <project-id> | tr ',' '\\n' | awk -F= '$1==\"desktop\"{print $2}'"
```

## 4) Retry and failure policy (current behavior)

- Transient failures (`429` / `5xx` / network) trigger exponential backoff retry.
- Non-transient failures fail fast.
- Outbox failures increment attempt counts and preserve pending status for next cycle.
- Default retry policy is `--retry-attempts 3 --retry-base-delay 500ms
  --retry-max-delay 5s`.

## 5) Conflict handling

- Server push conflict events are persisted to local `sync_conflicts`.
- Conflict artifacts are written during review when `--artifacts-dir` is set.

### Periodic stale-conflict review

Review unresolved conflicts older than 7 days:

```bash
hive sync conflicts review --db-path /path/to/notes.db
```

Export the current stale set to JSON:

```bash
hive sync conflicts review \
  --db-path /path/to/notes.db \
  --export-dir .local/conflict-reviews \
  --artifacts-dir .local/conflicts
```

After export/triage, mark the exported set resolved:

```bash
hive sync conflicts review \
  --db-path /path/to/notes.db \
  --export-dir .local/conflict-reviews \
  --mark-resolved
```

Notes:

- `--older-than-hours` defaults to `168` (7 days)
- `--include-resolved` widens the report without affecting default hygiene workflow
- `--mark-resolved` requires an export destination for safer review bookkeeping

## 6) Version/base-version behavior

- Local entity versions are tracked in `sync_entity_versions`.
- New local outbox entries use the current local entity version as `base_version`.
- Pulled, applied operations increment local entity version counters.

## 7) Todo sync parity behavior

- Pulled `todo` operations are applied into local `sync_todos`.
- This provides convergence for canonical todo entities independent of note-derived task projections.
- Stored todo shape is task-index aligned (`todo_section`, checkbox state, task text, source note metadata).

## 7a) Current phone-to-desktop visibility expectation

- The PWA pushes note/todo operations into the shared sync stream.
- Desktop `hive sync run` applies pulled todos into `sync_todos` and stages pulled
  notes into `sync_note_imports`.
- At the moment, this does **not** create visible markdown file changes in the
  main notes workspace until you review/export/import the generated inbox files.
- For validation, treat cursor advancement, applied-operation counts, and staged
  note import counts as stronger confirmation than filesystem diffs.

## 8) Troubleshooting quick checks

1. API health:
   - `curl http://127.0.0.1:8080/healthz`
2. API sync state:
   - `curl -H "Authorization: Bearer <token>" http://127.0.0.1:8080/v1/sync/state`
3. Local test suite:
   - `go test ./internal/features/syncclient ./internal/features/syncapi ./internal/infra/db`
4. Full repo tests:
   - `go test ./...`

### Common auth gotcha

If your secret value is formatted like:

```text
desktop=abc123,phone=def456
```

then the actual bearer token for `desktop` is just `abc123` (not `desktop=abc123`).

## 9) Deferred hardening (tracked in project future file)

- Device token rotation workflow.
- Secret Manager-backed device token loading.

Manual token rotation checklist:

- `docs/hive-sync-token-rotation-checklist.md`

## 10) Cloud deployment playbook

For Cloud Run deployment steps and scripts, see:

- `docs/hive-sync-cloud-deploy.md`
- `scripts/cloud/bootstrap-hive-sync-infra.sh`
- `scripts/cloud/deploy-hive-sync-api.sh`

## 11) Monitoring and backups

For operational guardrails and data recovery:

- Monitoring + alerts:
  - `docs/hive-sync-monitoring.md`
  - `scripts/cloud/setup-hive-sync-monitoring.sh`
- Backup + recovery:
  - `docs/hive-sync-backup-recovery.md`
  - `scripts/cloud/setup-hive-sync-backups.sh`
  - `scripts/cloud/export-hive-sync-backup.sh`
