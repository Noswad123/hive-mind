# Product Architecture Roadmap

This document captures the architectural direction for turning Hive Mind from a
working sync stack into a product that helps people sync notes between devices and
view/edit notes and todos safely.

## Product goal

**First personal vertical slice:** the desktop-local Google Tasks inbound
foundation is implemented, not live-activated. Start with the
implemented [Desktop OAuth setup](google-auth.md) (next input: private client JSON
path only), then the
[capture runbook and activation gates](google-tasks-capture.md): Google retains
phone tasks while the desktop is off; user-invoked Hive polling stages a private
queue and delegates canonical Inbox creation to `mw`. This does not require a
Postgres rewrite, hosted transport, or PWA migration. The broader bidirectional
sync architecture below remains future work; it is not a gate for this narrow lane.

Hive Mind should let a user:

1. keep MindWeaver Markdown notes as their trusted desktop source of truth,
2. sync notes and todos between desktop and mobile/browser devices,
3. view and edit notes/todos from a PWA or future app surface,
4. recover from conflicts without silent data loss, and
5. operate the system reliably across restarts, deployments, and multiple devices.

The later desktop UX may be a MindWeaver integration, but the sync protocol,
server, and mobile/web companion should remain owned by Hive.

## Core architecture

```text
MindWeaver desktop
  Markdown source of truth
  local SQLite projections/sync outbox
  optional Hive sync worker / UI integration

Hive sync API
  durable operation log
  device auth
  cursor/pull stream
  conflict detection and records

Hive PWA / mobile client
  IndexedDB local-first state
  note/todo editor
  queued operations
  manual/online sync

Review/import layer
  staged remote notes
  generated inbox files
  safe Markdown materialization policies
```

## Canonical data model

Use one explicit truth model:

- **MindWeaver Markdown** is the desktop canonical source for local notes.
- **Hive operation log** is the remote sync transport source.
- **Desktop SQLite** is a projection/sync cache for outbox, cursors, entity
  versions, staged note imports, todos, and conflicts.
- **PWA IndexedDB** is a browser-local projection/cache with queued operations.

Hive should sync operations, not raw filesystem state.

Current core entities:

- `note`
- `todo`

Possible future entities:

- `tag`
- `link`
- `attachment`
- `workspace` / `vault`
- `notebook` / collection metadata

## Required architecture for real user sync

### 1. Durable server backend

Current server backends:

- `memory`: demos/tests/smoke only
- `sqlite`: local durable smoke/dev, or constrained single-instance experiments

For real users, Hive needs a cloud-suitable durable backend:

- Postgres / Cloud SQL store in `hive-server`
- server migrations
- operation log table
- devices table
- entity versions table
- conflicts table
- idempotency table
- backup/restore procedure

Without this, users can lose sync state when the service restarts or redeploys.

### 2. Desktop sync agent

Users should not have to run manual commands for routine sync.

Target behavior:

```text
hive sync worker
  reads local MindWeaver DB/outbox
  pushes pending operations
  pulls remote operations
  applies todos
  stages or safely imports notes
  reports health/conflicts
```

Needed deliverables:

- long-running or bounded worker mode suitable for launchd/systemd
- config file for endpoint, device id, token source, notes DB path, and import
  policy
- launchd/systemd templates
- local status surface for last sync, pending outbox, conflicts, staged notes,
  and last error
- clear retry/backoff and failure policy

MindWeaver can eventually wrap this worker rather than reimplement sync.

### 3. Safe note materialization

The current behavior is intentionally safe: pulled note operations are staged in
`sync_note_imports`, and `hive sync notes review` can export generated inbox
files.

The product needs policy-based materialization:

```text
remote note operation arrives
  ├─ safe: apply to Markdown
  ├─ risky: stage for review
  └─ conflicting: create conflict record/artifact
```

Recommended import policies:

- `manual`: stage all remote note operations. This is the current safest mode.
- `inbox`: write remote notes to a generated inbox folder for manual review.
- `auto-safe`: auto-apply only when the local file does not exist or has not
  changed since the last synced version.
- `auto`: advanced/risky mode; should not be default.

Required safety metadata:

- last synced file hash/content hash
- local file modified time or content hash
- source device and operation id
- local base version at enqueue time
- reviewed/imported timestamp

Rule: Hive must never silently overwrite canonical Markdown.

### 4. First-class PWA/mobile client

The PWA should become a local-first client, not just a demo surface.

Current foundation:

- IndexedDB state
- manual sync engine
- note/todo editing baseline
- sync health surface
- conflict visibility baseline

Needed improvements:

- better Markdown editor and preview
- note search/sort/filter polish
- stronger todo editing and list views
- queued-operation inspection and retry UI
- sync error recovery UI
- conflict detail and resolution UI
- token/device onboarding polish
- import/export/debug screen
- mobile install validation on iOS and Android
- background sync where browser support allows it

PWA storage architecture should remain:

```text
IndexedDB
  config/settings
  notes
  todos
  queued sync operations
  sync cursor/metadata
  entity versions
  conflicts
```

### 5. Auth and onboarding

Device-token maps are acceptable for a self-hosted/power-user MVP. A product for
other people needs a clearer auth and onboarding model.

Self-hosted MVP:

- device-token map
- token check command
- Keychain support on desktop
- documented token rotation
- clear PWA setup instructions

Hosted/SaaS path:

- user accounts
- device registration flow
- refreshable device credentials
- token revocation and rotation
- rate limiting and abuse controls
- privacy/security documentation
- serious evaluation of end-to-end encryption

Because notes are personal and sensitive, hosted sync should not be offered to
others without an explicit encryption/privacy design.

## MindWeaver integration path

MindWeaver should integrate with Hive rather than duplicating Hive's sync logic.

Target integration:

```text
MindWeaver app
  owns Markdown UX
  shows sync health/status
  lists conflicts and staged imports
  starts/stops Hive sync worker
  invokes reviewed note import

Hive crates/CLI/server
  own protocol, API, auth, PWA, sync engine
```

Integration points:

- reuse `hive-core` protocol types
- reuse or expose `hive-desktop` sync/review APIs
- let MindWeaver UI call:
  - sync once
  - start/stop worker
  - list staged notes
  - export/import reviewed notes
  - list conflicts
  - mark conflicts resolved

Avoid creating a separate competing sync implementation inside MindWeaver. If
MindWeaver exposes sync, it should be a UX wrapper over Hive behavior.

## Minimum viable architecture for the goal

To let people sync notes between devices and view/edit notes/todos, the minimum
architecture is:

```text
Desktop MindWeaver
  Markdown + local SQLite + Hive sync worker

Hive Sync API
  durable Postgres operation log

Hive PWA
  IndexedDB local-first notes/todos editor

Sync protocol
  note/todo operations
  device auth
  idempotency
  entity versions
  conflict records

Review/import layer
  safe note materialization into Markdown
```

## Roadmap

### Phase 1: Reliable personal sync

- Add Postgres backend to `hive-server`.
- Add database migrations.
- Deploy Cloud Run + Cloud SQL for real durability.
- Add desktop worker config.
- Add launchd/systemd examples.
- Keep note imports staged/manual by default.

### Phase 2: Comfortable mobile editing

- Improve PWA note editor and Markdown preview.
- Improve todo UX.
- Add queue/error screen.
- Add conflict detail screen.
- Improve mobile install/onboarding docs and UX.
- Validate on iOS Safari and Android Chrome.

### Phase 3: Safe desktop materialization

- Implement `auto-safe` note import policy.
- Track file hashes / last synced content.
- Apply remote notes automatically only when the local file is unchanged or
  absent.
- Stage every risky change for review.

### Phase 4: MindWeaver integration

- Add sync status panel.
- Add staged note review UI.
- Add conflict review UI.
- Start/stop sync worker from MindWeaver.
- Surface sync errors and recovery actions in the desktop app.

### Phase 5: Multi-user or public product readiness

- Decide self-hosted-only vs hosted product.
- Add accounts/auth/device lifecycle if hosted.
- Decide and implement encryption model.
- Add observability, backups, restore drills, rate limits, and migration stories.
- Publish packaged releases/installers.

## Key architectural decision

The most important unresolved product decision is:

> Is Hive a personal self-hosted sync companion, or a hosted multi-user sync
> product?

If personal/self-hosted, device tokens plus a durable Postgres backend can ship
much sooner.

If hosted for others, Hive needs accounts, device lifecycle, encryption/privacy
guarantees, abuse controls, backups, and operational support before it is
trustworthy.

## Resume here later

When returning to this work, start with:

For the immediate personal workflow, first follow the
[Google Tasks activation gates](google-tasks-capture.md#example-usage), not a new
local MindWeaver cutover. The following steps concern broader hosted sync:

1. implement the Postgres `hive-server` backend,
2. add server migrations and backup/restore validation,
3. add desktop worker config/templates,
4. improve PWA editing/sync recovery UX,
5. design `auto-safe` Markdown materialization.
