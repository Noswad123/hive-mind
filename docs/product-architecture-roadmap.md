# Product Architecture Roadmap

This document captures the architectural direction for turning Hive Mind from a
working sync stack into a product that helps people sync notes between devices and
view/edit notes and todos safely.

## Product goal

**First personal vertical slice is live-verified:** scheduled Google Tasks inbound
capture stages a private queue and delegates canonical Inbox creation to `mw`.
An actual phone capture passed scheduled editor-owned delivery and duplicate-free
repeat checks. See [Google authorization](google-auth.md) and the
[capture runbook](google-tasks-capture.md) for the exact proof and remaining limits.
Google retains phone tasks while the desktop is off. This lane does not require
a Postgres rewrite, hosted transport, or PWA migration. The broader bidirectional
sync architecture below remains future work; it is not a gate for this narrow lane.

Hive Mind should let a user:

1. keep MindWeaver Markdown notes as their trusted desktop source of truth,
2. sync notes and todos between desktop and mobile/browser devices,
3. view and edit notes/todos from a PWA or future app surface,
4. recover from conflicts without silent data loss, and
5. operate the system reliably across restarts, deployments, and multiple devices.

The later desktop UX may be a MindWeaver integration, but the sync protocol,
server, and mobile/web companion should remain owned by Hive.

## Accepted next slice: hosted Calendar publication after push

Decision accepted on 2026-11-01; **design selected, publisher not implemented or
deployed**. Use a GitHub-hosted Linux workflow after a notes push, targeting zero
additional monthly cost within the account's included allowance. Do not provision
a paid database or server for this slice. The notes repository is private, its
default branch is `main`, and Actions is enabled; no publication workflow currently
exists. Billing allowance and paid-overage blocking still require verification.

```text
Successful push to the designated notes branch
  -> read the exact published commit
  -> validate and plan explicit Calendar entries
  -> reconcile managed Google Calendar events
  -> acknowledge outcomes in a Git-backed publication ledger
```

Boundaries:

- Git carries published source revisions and a small versioned ledger on a
  separate machine-managed state branch. Do not Git-merge live SQLite databases,
  or use expiring Actions caches/artifacts as authoritative publication state.
- SQLite remains useful for local notes/indexing and integration work. This
  hosted path is not a replacement for the existing inbound capture ledger or
  an activation of the legacy HTTP/PWA sync service.
- The publisher reads committed blobs, never unsaved buffers or a mutable local
  working tree. Ledger identity, last acknowledged source revision, remote
  mappings, pending outcomes, and retirement evidence must be explicit.
- Serialize publication and check state revisions before advancing the ledger.
  Calendar writes and ledger commits are not one transaction: reconcile ambiguous
  creates and provider success followed by failed ledger persistence before retry.
- Scope Google changes to managed entries. Missing source is not automatic remote
  deletion, completion, or permission to resurrect an item. An absent ledger is
  not silently treated as an empty first export.
- Credentials belong in approved hosted secret storage, never source, ledger,
  artifacts, or logs. Calendar requires separate write authorization; do not reuse
  the Tasks-readonly grant as if it authorized event writes.
- Keep execution on trusted publication refs, cap job runtime and retries, avoid
  rebuilding the full workspace on every notes push, and verify paid-overage
  blocking before activation. Alerts alone are not a spending cap.

Date-only first slice:

- Use the existing task `due:YYYY-MM-DD` field. Do not require a start date or
  reinterpret the existing task `start` field as a Calendar scheduling date.
- Preview a due date as an all-day deadline reminder marked Available, not a
  full-day busy block. Google requires an internal start/end pair even for all-day
  events: use the due date and exclusive following calendar date. This does not
  introduce a user-facing start-date field or invent a time/duration.
- Start time and duration are important future metadata, deferred from v1. Their
  source syntax, timezone/DST policy, and relationship to the deadline remain a
  later decision. No timezone selection is needed for the date-only preview.
- Keep logical identity and remote mapping independent of date, start time, and
  duration. A later change from all-day to timed representation should update the
  same managed event rather than create a second task identity.
- This is one-way notes-to-Calendar publication after push, not bidirectional
  Calendar synchronization. Remote-edit import and automatic destructive deletion
  are outside this first slice.

**Implemented narrow milestone:** `hive-calendar` and `hive calendar preview` read
one full lowercase pinned commit, materializing bounded committed regular Markdown
in a private workspace. A private exact hashed copy of a trusted standalone `mw`
runs `todos snapshot --json` with cleared environment and private config/home, no DB
and no original-note mutations. Git 2.50+ at `/usr/bin/git` is required. See
[built-debug usage](../README.md#calendar-source-preview-local-only); the installed
release/background worker is unchanged. This preview remains source-only; the
separate ledger-relative planner is described below. macOS fixture proof exists;
Linux runtime is untested and explicitly deferred by the user until later before
deployment, not a blocker for local work.

Due validation scans the literal committed checkbox line (not the normalized title)
and metadata `raw`, requiring one valid calendar date matching MW's parsed due date.
Completed/undated tasks are skipped; invalid/duplicate dates or parser mismatches
produce diagnostics and no prospective event. Valid events use `start.date` = due,
exclusive `end.date` = next calendar date, `availability: "Available"`, and
`transparency: "transparent"`; start/estimate fields never schedule them.

Stdout JSON has `schemaVersion: "1"`, `kind: "calendar-due-preview"`,
`source: "committed-markdown"`, `commit`, `parserSha256`, `publicationReady`,
`diagnostics`, `counts`, and `items`. Counts are `total`, `completedSkipped`,
`undatedSkipped`, `validDue`, `invalidDue`, and `publicationBlocked`. Each retained
item carries `id`, optional `stableId`, `identityKind`, source provenance (`path`,
`sourceId`, `line`, `sourceRevision`, `gitBlobID`), diagnostics/readiness, and optional
`due`/`prospectiveEvent`. There are no Google event IDs or create/update/noop actions.
**Titles and paths are private; never paste preview stdout into public CI logs.**

`publicationReady` is always false at both levels, even for stable IDs. Global
blockers are `enrollment_not_configured`, `ledger_not_configured`, and
`identity_contract_unresolved`; items expose `identity_mapping_unconfirmed` for
existing stable IDs or `durable_identity_unresolved` otherwise. No new HTML comments
are assigned. Enrollment and durable identity require a separate explicit contract,
not just more metadata fields or identity inferred from title/path/line.

### Calendar ledger schema V1

**Implemented local advice, not a publisher:** built-debug `hive calendar plan`
consumes one explicit source commit and either an explicit committed ledger or
`--bootstrap-preview`. See [practical commands](../README.md#calendar-publication-plan-local-only).
Source/account/calendar bindings must be independently supplied, never inferred
from state. `provider` is `google_calendar`; `accountId` is `google-sub-v1:` plus
64 lowercase hex digits; `calendarId` is a concrete non-whitespace ID, not a
case-insensitive `primary`/`default` alias. `sourceRepoId` is an opaque portable
namespace of 1-256 ASCII alphanumeric or `._:-` characters, excluding `.` and `..`,
not a repository path/URL. Validating bindings is not credential or ownership proof.

The public Rust types in `crates/hive-calendar/src/ledger.rs` define strict camelCase
JSON. `schemaVersion` is the string `"1"`; unknown fields and duplicate keys are
rejected recursively. The committed regular JSON blob is at most 1 MiB, with at most
256 entries and eight distinct source-acknowledgement commits, including pending
intended sources. Missing, malformed, misbound, or unverifiable existing state is
an error, never an implicit empty ledger or bootstrap fallback.

| Object | Contract |
| --- | --- |
| Ledger | `schemaVersion`, `binding`, `sourceWatermark`, `entries`. The watermark is a source-repository commit, not a ledger-head assertion. |
| Entry | Nonzero lowercase hyphenated UUID `logicalId`, `lifecycle`, exact `sourceAck`, optional `providerAck`, `pending`, `tombstone`. Optional evidence may be absent or null. |
| Source acknowledgement | `commit`, `parserSha256`, relative Markdown `path`, `sourceId`, one-based `line`, `mwId`, `gitBlobID`, `sourceRevision`, optional `stableId`, and `expected`. |
| Expected task | Required `text`, `literalText`, `done`, `due`, `raw`, `start`, `estimate`, including empty strings where applicable. Literal committed checkbox bytes and full-file hash are evidence, not just a normalized title. |
| Provider acknowledgement | `eventId`, `etag`, `lastPayloadHash`: last recorded provider evidence, not a current remote read. |
| Pending operation | UUID `operationId`, `kind` (`create`/`update`/`delete`), `phase` (`prepared`/`uncertain`), `intendedSource`, `targetEventId`, optional `baseEtag` and `desiredPayloadHash`. These are recovery evidence, not planner actions. |
| Tombstone | `reason`, `sourceCommit`, optional `operationId`, `remoteDisposition` (`deleted`/`retained`/`unknown`). A deleted disposition requires an operation ID. |

`enrolled` has no provider acknowledgement or tombstone; `active` requires a
provider acknowledgement and no tombstone; `retired` requires provider evidence
and a tombstone, with no pending operation. Pending creates require enrolled state,
no base etag, and a desired hash. Pending updates/deletes require active state,
the same acknowledged event ID and base etag; only updates carry a desired hash.
Logical IDs, event IDs, operation IDs, source observations, and stable hints cannot
be duplicated across entries, including retired/pending evidence.

Source commits/blob IDs are full lowercase object IDs matching the source object
format. `parserSha256` is 64 lowercase hex; source and payload revisions use
`sha256:<64 lowercase hex>`. Source acknowledgements bind exact parser binary,
path/locator, committed blob/full-file hash, literal line, parsed state, and metadata.
CRLF is preserved, including the terminal `\r` in `literalText`; neither evidence
nor oversized fields are normalized/truncated. Bootstrap observations pass the same
source-acknowledgement shape/field-limit validation before output.

History validation is scoped to the source repository: the watermark must be an
ancestor of the target; acknowledged source and retirement commits must be ancestors
of the watermark; pending intended-source commits must be ancestors of the target.
Non-shallow, available history is required, with no fetch. With the same parser
digest, historical acknowledgements are rederived from committed snapshots; forged
evidence fails the whole plan. A different parser digest yields review, not proof
of old semantics. Different builds/platforms therefore require provenance review/
re-acknowledgement, even if they are expected to parse identically.

Logical UUIDs are independent of task dates and source HTML markers. Markerless
tasks can use an existing explicit map; `unenrolledObservations[].sourceAck`
exposes new evidence for future tooling. Bootstrap creates no authoritative ledger,
UUID, enrollment, or provider mapping. Candidate generation and reviewed local
persistence are implemented below; normal-user UI and integration activation remain
needed so users do not maintain IDs/JSON themselves. Any edit in an acknowledged
file, even an unrelated paragraph, or a move changes the exact basis and requires
review/new explicit acknowledgement. No title/fuzzy/comment-only auto-matching or
automatic enrollment/resurrection is provided.

Canonical managed payload hashing is SHA-256 over the domain bytes
`hive-calendar-managed-payload:v1\0`, followed in order by UTF-8 `summary`,
`start.date`, `end.date`, `transparency`, each prefixed by its unsigned 64-bit
big-endian byte length. The result is `sha256:<64 lowercase hex>`. UI availability,
identity, and source provenance are excluded; identity is not derived from dates.

Plan output uses `kind: "calendar-publication-plan"` and mode `ledger` or
`bootstrap-preview`. Decisions are `noop`, `update`, `review`, `blocked`, `retired`,
never create/delete. `noop` only compares the desired payload with the last
`providerAck`, not remote existence/currentness. An advisory `update` requires a
verified exact source basis and includes the same `eventId`, `baseEtag`,
`lastAcknowledgedPayloadHash`, `desiredPayloadHash`, and `prospectiveEvent`.
Every pending phase blocks retry until reconciliation. Retired entries never
auto-enroll/resurrect; completed, undated, invalid, missing, or moved source means
review, not deletion. Enrolled entries still require future initial-publication work.

Always `publicationReady: false` (including entries/observations),
`remoteVerified: false`, `ledgerHeadVerified: false`, and
`requiresLedgerCompareAndSwap: true`. An offline plan over an old ledger cannot know
the latest state. A future executor must verify state and enforce ledger-head
compare-and-swap, serialize writes, and reconcile ambiguous provider outcomes.

**Privacy:** ledger and output contain private content, paths, and IDs, never
credentials. Do not log or automatically upload output as CI artifacts, including
to Atlassian; the user explicitly declined Atlassian uploads.

**Remaining gates:** normal-user approval/UI and integration activation; durable
provider intents/results and reconciliation; Calendar OAuth write grant/executor;
hosted workflow serialization and remote lease/CAS push; account allowance and
verified paid-overage hard stop; pinned Linux packaging/runtime validation later
before deployment; and explicit approvals for deployment, real state-repository/ref
creation, Calendar writes, and hosted secrets. No paid service by default. Do not
probe VMs, repeat Docker probes, or install targets while Linux validation is
explicitly deferred. Local persistence below was tested only in temporary bare
repositories, including actual Git writes, not real user state. Installed binaries,
capture worker/private queue/auth, live notes/source/refs, credentials/grants,
network/provider state, workflows/cloud resources, secrets, and billing are unchanged.

### Calendar candidate generation

**Local candidate generation DONE, not durable enrollment:** implemented in
`crates/hive-calendar/src/candidates.rs` and `crates/hive/src/calendar.rs`, exposed
by built-debug `./target/debug/hive calendar enroll` and `reack`, not the installed
release. See [actual flags/examples](../README.md#calendar-ledger-candidates-local-only).
The Rust entry points take `EnrollmentOptions` / `ReackOptions` and return a
`LedgerCandidate`. No source, ledger, provider, auth, or Git-ref writes occur.

Both commands require `--repo`, full lowercase `--commit`, `--mw-bin`, independent
`--source-repo-id`, `--account-id`, concrete `--calendar-id`, and all of
`--ledger-repo`, full lowercase `--ledger-commit`, `--ledger-path`. Current-task
selection requires `--source-path` (exact relative Markdown path) and
`--source-line` (positive one-based checkbox line). Existing proof uses the same
strict ledger/history validation as planning, before any replacement.

Enroll requires an open task with valid due evidence and allocates an OS-entropy
UUIDv4. No HTML marker or note edit is needed. Only explicit `--initialize-ledger`
permits a new-ledger proposal, proving path absence at the selected base commit
with any existing parent components trees. This is not authoritative-HEAD or remote
absence proof; ordinary missing state never initializes. Reservations/duplicates
include retired and pending evidence. A conservative global guard requires no
pending operations and recognition of **all** old source bases, even retired ones.
Missing/moved/edited retired basis can block unrelated enrollment until a future
resolution workflow; no force override or friction-free restore is provided.

Reack requires either `--old-source-path` plus `--old-source-line`, or advanced
`--logical-id`, never both. It explicitly maps the selected new source observation
to the existing UUID across moves/due changes, with no fuzzy identity inference.
It changes the selected `sourceAck` and watermark, preserving provider ACK,
lifecycle, and unselected pending/tombstone evidence. Pending/retired targets refuse.
Entire old-ledger proof validation precedes replacement, so forged old ACKs cannot
be washed out and source rollback refuses. `--accept-parser-change` approves only
the selected digest migration; it does not establish old parser semantics. Done or
genuinely undated observations may be acknowledged, without provider completion or
deletion. Shared due rules still refuse invalid/duplicate/malformed dates even on
completed tasks, rather than skipping validation because they are done.

The camelCase envelope contains `schemaVersion: "1"`,
`kind: "calendar-ledger-candidate"`, `operation: "enroll" | "reack"`, `logicalId`,
`sourceCommit`, `parserSha256`, `base`, the whole validated V1 `candidateLedger`,
and `diagnostics`. `base` has `commit`, `path`, `gitBlobID` (value for existing state,
null for initialization), and `condition: "existing_blob_at_commit"` or
`"path_absent_at_commit"`. The CLI validates and buffers the full serialized
candidate before stdout. `publicationReady`, `remoteVerified`, and
`ledgerHeadVerified` remain false; `requiresLedgerCompareAndSwap` remains true.

Candidate generation persists nothing; enrollment IDs can differ on rerun until
accepted. Never publish from an unpersisted candidate. Reviewed local persistence
is implemented below; exact historical blob evidence alone is still not HEAD proof.
Normal-user UI and real-repository activation remain pending; these docs do not
approve actual user-repository commits. Calendar OAuth write/publisher, hosted
workflow, and billing-hard-stop activation remain pending. Linux follow-up is
required before hosted deployment, not local development.
See [recorded macOS validation](release.md#calendar-candidate-checkpoint-2026-11-02).
Candidate content is private under the same no-logging/no-Atlassian-upload boundary.

### Calendar local approval and persistence

**Local persistence code/test milestone DONE; integration activation not done.**
Implemented in `crates/hive-calendar/src/{approval,state_git,lib}.rs` and
`crates/hive/src/calendar.rs`. Public `verify_candidate` / `apply_candidate` take
`ApplyOptions`; built-debug `calendar verify-candidate` / `calendar apply` expose
the same explicit inputs. See [review-first examples](../README.md#calendar-local-persistence).
No real-repository apply is approved or executed by this milestone.

Both require source `--repo`, full lowercase `--commit`, trusted `--mw-bin`,
independent `--source-repo-id`, `--account-id`, concrete `--calendar-id`, absolute
`--ledger-repo`, relative `.json` `--ledger-path`, fixed
`--state-ref refs/heads/hive-calendar-state`, absolute regular JSON `--candidate`,
and `--candidate-sha256` containing 64 lowercase hex digits. The candidate supplies
its base commit, not a separate apply-time `--ledger-commit`. Initialization alone
requires `--create-ref`; selected reack old-parser migration requires independent
renewed `--accept-parser-change`, not just its generation-time acceptance.

Approval protocol:

1. Capture the candidate file once, with `O_NOFOLLOW` and an 8 MiB bound, and hash
   those exact captured bytes. Reject unknown/duplicate fields recursively and
   trailing JSON. The approved hash is exact-byte authorization, not a cryptographic
   signature, account ownership, or provider proof.
2. Validate the original committed ledger/history before replacement, and replay
   the original enroll/reack semantics against the pinned source/parser and supplied
   bindings. Require whole-ledger and whole-envelope equality. A matching digest
   alone never authorizes arbitrary ledger replacement or forged historical ACKs.
3. Replay enroll with the already approved UUID, never allocate it again. Encode
   the validated ledger as canonical compact JSON for the persisted blob.
4. Validate the destination and compute raw Git blob/tree/commit objects in memory.
   Verification only hashes them, without object writes or ref updates. Apply
   revalidates, writes the objects, and performs one atomic fixed-ref old-OID CAS.

V1 destination contract is an operator-controlled **dedicated bare repository with
files ref backend and no registered linked worktrees**, including unborn ones.
A normal notes worktree is refused. Only the fixed ref is supported; foreign
existing tips, symbolic/dangling refs, object-store/common-directory indirection,
and unsupported backends refuse. There is no adopt/reset/force/push operation.
**The operator MUST exclusively administer this repository throughout the operation.**
Preflight and pre-CAS checks cannot atomically prevent concurrent worktree/config
administration; ref CAS serializes ledger writers, not repository administrators.
This assumption is part of readiness, not a general deployment safety claim.

An update requires whole-commit state-ref HEAD equality with the candidate base
and exact old ledger blob, plus a strict canonical tool-shaped ownership receipt
bound to ref/path/binding/blob. Receipt checks also enforce the single parent,
canonical commit shape, and no unrelated tree changes. This is structural ownership
evidence, **not cryptographic signer proof**. Historical blob equality alone or an
arbitrary pre-existing ledger branch is insufficient for adoption.

Initialization additionally requires absent-ref CAS against zero. Candidate path
absence at the chosen seed proves neither branch absence nor remote absence. The
new commit's sole parent is the chosen base, and it inherits the **whole seed tree,
possibly including notes**, changing only the ledger path. It does not create an
orphan state branch or isolate seed contents automatically. Seeding/activation of a
real state repository needs separate review and authorization.

Raw-tree Git plumbing preserves unrelated entry modes and OIDs, without checkout,
index, config edits, hooks, signing, or maintenance. Deterministic commit identity
is `Hive Calendar <hive-calendar@localhost>`; author/committer time is UTC parent
committer time plus one second, **not approval wall-clock time**. Verification and
apply compute identical expected commit IDs for identical inputs.

Receipts expose the candidate digest, binding, base/source/commit, ledger blob ID,
logical ID, operation, parser acceptance, and local-ref result. Verify statuses are
`verified` or `already_applied`; apply statuses are `applied` or `already_applied`.
`localRefUpdated` is true only on an actual successful CAS. Always
`publicationReady: false`, `remoteVerified: false`, `remoteRefVerified: false`.
There is no remote-head proof, remote sync, or Calendar dispatch.

Exact retry at the reconstructed expected tip returns `already_applied` without
writes. A later advanced tip refuses; no rewind or content-only acceptance occurs.
Recovery must retain the **same approved candidate, destination, bindings, source
commit, parser artifact, and permission flags**. Pinning the parser is necessary:
an otherwise equivalent rebuild can change its digest and invalidate replay.

A root cancellation guard spans the entire capture/replay/writer/CAS operation,
including CPU/phase gaps between child helpers. SIGINT/SIGTERM observed before CAS
stop ref advancement. Object writes are not rolled back: failures can leave
unreachable objects, with no automatic GC. After CAS dispatch, cancellation or lost
output may mean success; report unknown and recover via exact retry, never rollback.
Tests include phase-boundary signals, cancellation after real object writes before
CAS, and a real successful CAS followed by injected cancellation/lost response.

See [latest macOS evidence](release.md#calendar-local-persistence-checkpoint-2026-11-02).
This is fixture-only local persistence readiness under exclusive administration.
Normal-user UI, actual integration activation, durable provider intent/result
protocol (remote APIs and Git are not one transaction), CI serialization, pinned
Linux runtime, account cost hard stop, OAuth grant/executor, and remote lease/CAS
push are still pending. Linux is explicitly deferred, not a local-work blocker.

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
