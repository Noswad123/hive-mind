# Hive Mind Status and Roadmap

Last updated: 2026-11-02

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

### Google Tasks inbound capture: scheduled phone-to-editor delivery live-verified

`hive-capture` and `hive captures` now provide the first personal vertical slice:
Manual or scheduled full paginated read-only Google snapshot → dedicated private SQLite
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
At the inbound inventory-fix checkpoint, 270 normal workspace tests passed,
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

At the earlier polling checkpoint, two controlled `run-once` cycles completed in about 0.5 seconds each, observing
one unchanged task, staging/delivering zero, with `authActionRequired: false` and
generation 1. Explicit background install/start then enabled the owned LaunchAgent:
`serviceActive: true`, `serviceState: loaded_owned`. Its actual startup and next
scheduled cycle completed about 60.7 seconds apart, each with no pending work,
one unchanged observation, zero staged/delivered, and no auth action required.
Native LaunchAgent ownership/status parsing and scheduled polling are live-verified.
At that checkpoint Inbox/dashboard hashes, bytes, and mtimes were unchanged; the receipt was still
attempt 1 with no duplicates. Earlier 45-second `unknown_outcome` and 12-second
local status stall did not reproduce; bounded inherited/minimal-environment local
status diagnostics succeeded in 0.054/0.028 seconds. The credential-operation
failure is resolved operationally, not root-caused; the same build/credentials work
and no speculative source fix was made. No additional tests ran during activation.

On 2026-11-01, the user added `Verify scheduled phone capture` on the phone, undated
and title-only. An automatic scheduled cycle, not manual `run-once`, completed with
observed 2, staged 1, unchanged 1, delivered 1, pending 0. The new durable
`todos-add` receipt has one attempt, `alreadyCaptured: false`, `done: false`,
`projectionUpdated: true`, and `sourcePath: introspection/inbox.md`, Todo / Inbox,
line 12; its `sourceRevision` matches the actual Inbox hash. Actual Markdown and a
recursive whole-notes-root search confirm exactly one unchecked stable-marker entry.
The same-cycle native Neovim bridge reached `phase: done`, delivered 1,
`writerExited: true`, conflicts 0; afterward `busy: false`, mode `n`, no dirty buffers.
The next actual scheduled cycle observed 2, unchanged 2, staged/delivered 0,
`no_pending`, `authActionRequired: false`, with the owned service still active.
Both captures retain one attempt and their receipts. Inbox (324 bytes) and dashboard
(4,651 bytes) bytes/sizes/mtimes were unchanged from post-delivery to follow-up;
dashboard content retained its earlier hash, although its mtime changed on delivery.
No Google/source completion or archival was performed. This is live scheduled
writer and repeat-nonduplication proof, not a new automated test result.

Polling is running in the GUI user session at 60 seconds, with a 45-second timeout
and limit 1. Google holds work while the computer is off; no server is required.
Use stable-path `background status`, `stop`, `start`, and `uninstall` controls.
Stop retains config/queue; uninstall removes only the owned plist and retains all
state. Live computer-off catch-up and full pending-job dirty-buffer delivery deferral
remain unverified; read-only dirty readiness is already proven. Calendar publication, export/completion writeback,
lifelog, and PWA migration remain pending.

### Accepted next slice: hosted Calendar publication

The user selected GitHub-hosted Linux Actions after notes pushes, with a small
Git-backed publication ledger on a separate state branch and no paid infrastructure
initially. The private notes repository uses `main`, Actions is enabled, and no
existing publication workflow was found. This is a selected architecture, not a
deployed capability. See the [publication contract](docs/product-architecture-roadmap.md#accepted-next-slice-hosted-calendar-publication-after-push).

**Local source-only preview retained:** `hive-calendar` plus
`./target/debug/hive calendar preview --repo /ABS/local/notes --commit FULL_LOWER_SHA --mw-bin /ABS/trusted/mw`.
This reads bounded committed regular Markdown through a private hashed copy of the
existing read-only MW snapshot parser, not a DB or mutable working tree. Due-only
all-day Available previews expose invalid dates and unresolved identities;
`publicationReady` is always false even with stable IDs. The prior source-preview
checkpoint remains recorded separately.

**Local ledger-relative planner implemented:** built-debug `hive calendar plan`
requires independently supplied source/account/concrete-calendar bindings and either
explicit observation-only bootstrap or all three committed-ledger inputs. Strict
V1 JSON maps logical UUIDs to exact source acknowledgements and optional provider,
pending, and retirement evidence. Source HTML markers are optional; no UUIDs or
enrollment are assigned. Missing/bad existing state fails, never bootstraps.
Decisions are `noop`/advisory `update`/`review`/`blocked`/`retired`, not create/delete.
No remote or latest-ledger-head verification occurs; publication is always disabled
and a future executor must compare-and-swap the ledger. Any edit in the source file
or move requires review/new explicit acknowledgement under the same logical ID;
different parser binary builds/platforms also require review/re-acknowledgement.
Candidate generation and reviewed local persistence are now implemented; integration
activation and normal-user UI remain needed so users do not maintain IDs/JSON by hand. No
title/fuzzy/comment-only matching or friction-free continuity is claimed.
See [practical usage](README.md#calendar-publication-plan-local-only),
[schema contract](docs/product-architecture-roadmap.md#calendar-ledger-schema-v1),
and [historical plan evidence](docs/release.md#calendar-plan-checkpoint-2026-11-02).

**Local candidate-generation milestone DONE:** built-debug, not installed,
`./target/debug/hive calendar enroll` / `reack` emit validated whole-ledger JSON
candidates only. All source/binding/ledger flags and exact relative Markdown path /
positive one-based line selection are required. Enrollment assigns OS-entropy
UUIDv4 without markers or note edits; initialization requires explicit
`--initialize-ledger` plus proof of path absence at the full ledger base commit,
not authoritative HEAD or remote absence. Normal missing state never initializes.
Duplicates/reservations including retired evidence refuse. Enrollment's global
guard requires all old bases recognized, including retired, and no pending work:
missing/moved/edited retired source can block unrelated enrollment until a future
resolution workflow. No force override or friction-free restore is promised.

Reack explicitly maps either an old path/line or advanced existing logical ID to
the new path/line, retaining UUID/provider ACK/lifecycle and unselected evidence.
Entire old proof is validated before replacement; pending/retired targets refuse.
Parser-change acceptance is selected-entry approval, not old-semantic proof.
Done/genuinely undated ACKs are allowed, not completion/deletion propagation;
malformed/duplicate due evidence refuses even for done tasks. Output is fully
serialized before stdout and remains not publication-ready, remotely verified,
or ledger-head verified; CAS is required. Candidate generation persists nothing, so
enrollment IDs can differ on rerun. Never dispatch from an unpersisted candidate.

At the candidate-only checkpoint, the approval/apply writer and persistence were
next work; the local implementation is now delivered below. Real-repository apply
is still not activated or approved by these docs. See
[candidate commands](README.md#calendar-ledger-candidates-local-only) and
[macOS candidate evidence](docs/release.md#calendar-candidate-checkpoint-2026-11-02):
363 normal workspace tests passed, ten ignored; Calendar 82 normal/two ignored;
CLI candidates four normal/one ignored. All four explicit real-MW gates passed;
strict Calendar all-targets Clippy, fmt/build passed; no local candidate P1/P2
remained. These are supplied implementation results, not rerun for this docs edit.

**Local persistence code/test milestone DONE; integration activation not done:**
built-debug `calendar verify-candidate` is read-only, computing blob/tree/commit
OIDs; `calendar apply` explicitly writes local Git objects and performs one fixed
state-ref HEAD CAS. Both require explicit source/MW, independent bindings, absolute
bare ledger repository, relative JSON path, `refs/heads/hive-calendar-state`, absolute
regular candidate, and independently approved exact-byte SHA-256. `--create-ref` is
initialization-only; selected old-parser reack migration requires renewed independent
`--accept-parser-change`. The 8 MiB single capture rejects symlinks via `O_NOFOLLOW`
and unknown/duplicate/trailing JSON. Approval is not a signature/provider proof.
Original ledger/history is validated before semantic replay and whole-candidate
equality; a digest is not authority for arbitrary replacement. Approved UUIDs persist
unchanged as canonical compact ledger JSON.

V1 only accepts a dedicated bare files-backend repository without linked worktrees,
not a notes worktree. Updates require a strict owned tool-shaped receipt bound to
ref/path/binding/blob, matching old blob, and whole-commit base HEAD equality; this
is not cryptographic signer proof. Foreign tips, symbolic/dangling refs, and object
indirection refuse. Init CAS requires ref absence against zero, not merely path
absence; the sole-parent commit inherits the whole seed tree, possibly notes, not
an orphan branch. No adopt/reset/force/push. Raw-tree plumbing preserves unrelated
modes/OIDs, without checkout/index/config edits/hooks/signing/maintenance. Fixed
identity and UTC parent-committer-time + 1 second make verify/apply OIDs identical.

Verify statuses are `verified`/`already_applied`; apply statuses are
`applied`/`already_applied`. Only successful CAS sets `localRefUpdated: true`;
publication/remote/remote-ref verification remain false. Exact expected-tip retry
writes nothing; advanced-tip retry refuses, with no rewind/content-only acceptance.
Root cancellation covers capture through replay/writer/CAS; pre-CAS cancellation
stops advancement, while post-dispatch cancellation/lost response may mean success
and requires exact retry, never rollback. Failed writes may leave unreachable
objects; no automatic GC. Recovery needs the same approved candidate, destination,
bindings, source commit, pinned parser artifact, and permission flags.

**Operator MUST exclusively administer the state repository during each operation.**
Preflight does not atomically exclude worktree/config administration races; CAS
handles ledger writers, not admins. Focused review closed the lost-cancellation P2:
ready for local persistence under that assumption, not general deployment.
Historical macOS persistence results after the root-cancellation fix: 389 normal workspace
tests passed/12 ignored; Calendar 105 normal/3 ignored; CLI apply 3 normal/1 ignored.
The real-MW apply fixture and four earlier gates rerun all passed (five gates).
Strict Calendar all-targets Clippy, fmt/build passed; the main CLI's four original
legacy warnings are unchanged. No tests/builds were rerun for these docs.
See [review-first usage](README.md#calendar-local-persistence) and
[persistence evidence](docs/release.md#calendar-local-persistence-checkpoint-2026-11-02).
All actual Git writes were confined to temporary bare test repositories: no actual
notes/source/ref changes, new real state repo, installed binary/worker, auth/grant,
provider, remote, workflow, credentials, or billing changes. No remote sync or Calendar writes.

**Offline operation code/test milestone DONE, not an actual publisher:** built-debug,
not installed, `calendar prepare` emits a persistable V2 frozen create/update intent
candidate, `calendar mark-uncertain` emits a monotonic frozen recovery candidate,
and `calendar simulate-ack` emits an operator-asserted **nonpersistable hypothetical
result**. See [flags/examples](README.md#calendar-offline-operations),
[schemas and encoding](docs/product-architecture-roadmap.md#calendar-offline-operation-contract),
and [historical offline evidence](docs/release.md#calendar-offline-operation-checkpoint-2026-11-02).

Prepare requires source repo/full commit/MW, independent bindings, all committed
ledger flags, and either logical ID or old acknowledged path/line. Selected source
must be nonretired, exactly acknowledged with current parser, open and valid-due;
**any pending entry blocks preparation**. Enrolled reserves deterministic `h1` plus
namespace/logical-UUID SHA-256 hex for create. Active update retains old event ID/
base etag and requires changed payload hash; equality refuses as a no-op. A fresh
bounded OS-entropy UUIDv4 identifies the operation. Only selected Pending Prepared
is added; source ACK/watermark/provider ACK/lifecycle are preserved, with no source
comments. Reservation is not remote absence/ownership/API-idempotence proof.

Mark-uncertain generation requires only binding/ledger flags, the same exclusive
entry selector, and exact persisted `--operation-id`, with no source/MW flags.
Only Prepared -> Uncertain is supported, preserving all immutable intent fields,
frozen sourceCommit/parser metadata and domain-versioned binary `intentDigest`.
Current edits/deleted notes/unavailable parser do not block it. Generating again
from Uncertain refuses; exact apply retry is idempotent. Generic verify/apply CLI
still requires SourceArgs: `--commit` must match frozen metadata, but repo/MW paths
are not read for this phase-only route, which proves no current eligibility.
There is no cancel/clear/reset/reprepare, delete, or retire operation in this layer.
Both phases block new preparation; Prepared is not retry/dispatch authorization.

Simulation requires frozen binding/ledger/selector/operation ID plus absolute
`--observation`, approved 64-lowercase-hex `--observation-sha256`, and required
`--assert-operator-observation`. Strict single-capture regular no-follow input is
bounded to 1 MiB and rejects unknown/duplicate/trailing JSON. Observation V1 has
`provenance: "operator_asserted"`, binding/logical/operation/managed IDs, intent
digest, event ID, valid asserted result etag, and exact managed payload
`{summary,startDate,endDate,transparency}`; all frozen bindings and recomputed
payload hash must match. Fake authenticated provenance/input `remoteVerified`
refuses. Current notes are not read and no trust override exists.

Output V2 `calendar-provider-simulation` embeds typed observation/hash and
`hypotheticalLedger`, with `simulationOnly: true`, `persistenceAllowed: false`,
and false remote/publication/head flags. Hypothetical success sets Active/provider
ACK/pending None but retains frozen source ACK/watermark, even after notes vanish.
**Ordinary verify and apply reject actual simulation output with its correct
approved SHA**; durable pending/provider ACK remain unchanged. At this offline
checkpoint there was no transport or authenticated-success constructor; the private
adapter below now exists, but provider-result persistence is still not done.

Ledger V1 and strict enroll/reack Candidate V1 remain unchanged. New strict V2
`calendar-operation-candidate` (`prepare`/`mark_uncertain`) uses full replay and
internal operation proof. V2 ownership binds operation/intent/committed phase/event/
payload; exact V1 marker/encoding remains unchanged. The current writer reads both;
old state writers refuse V2-owned tips, not necessarily every read-only ledger
reader. New-operation apply receipts are V2 with operation/intent fields; V1 receipt
fields stay unchanged. Same bare fixed-ref CAS/exact-retry/cancellation policy;
persist the prepared candidate before any future dispatch. Git CAS is not exclusive
API dispatch ownership or provider concurrency proof.

Historical offline macOS implementation-session results: **415 normal workspace passes,
13 ignored**, serial `cargo test --workspace --locked --offline -- --test-threads=1`;
Calendar **130 normal/3 ignored**; CLI `calendar_apply` **4 normal/2 ignored**.
**All six explicit real-MW gates ran and passed:** two apply gates including the
new offline operation flow, candidate, plan, preview, source-reader. The new flow
proves frozen recovery without source/parser, exact retry, true stale-ref rejection
with restored preparation inputs, required operator assertion, and actual simulation
rejection by both verify/apply without object/ref changes. Wrong observation
bindings/IDs/digest/event/etag/payload/provenance are covered. Strict Calendar
all-targets Clippy, fmt/build passed; no local offline P1/P2 remained and CLI e2e
follow-ups are now done. These were executed by the implementation session, not the
user or this docs pass. Older 389/363/339/303 counts remain historical.

All fixtures used temporary repositories, including actual temporary state Git
writes. No Google API/Keychain/live user state/hooks/CI/account-quota calls, actual
user-repository commit/ref/push, secret/billing/provider changes, installed binary
swap, or worker changes. This docs pass runs no tests/builds or activation commands.

**Dormant private HTTP adapter code/loopback milestone DONE, not publication or
activation:** compiled real request builders in private `calendar_transport`, with
no public export, new CLI/config/env lookup, credential provider/Keychain/refresh/
grant changes, or production consumer. POST/PATCH requires a private move-only
`DispatchPermit` bound to intent/IDs/digest/kind/origin and Prepared phase; only a
test factory exists. Prepared/local Git CAS is not authoritative dispatch ownership.
Same-bearer Userinfo subject validation precedes Calendar I/O; principal binding
does not prove scopes/calendar ownership/dispatch permission. Separate future
Calendar-write + OpenID grants are required, not existing Tasks-readonly authority.

Reconcile GET observes only the bound desired event, not mutation causality. Create
uses the reserved ID/managed tags; update validates prior ownership/hash/strong
quoted etag before managed-fields-only If-Match PATCH, preserving unrelated private
properties. Fixed HTTPS endpoints, no proxy/redirect/retry, shared 30-second budget,
10-second connect limit and bounded bodies constrain the adapter. Sent mutation
5xx or I/O/timeout/malformed/mismatched success is Uncertain; GET/Userinfo 5xx is
Unavailable with no write. 409 is collision for later explicit reconciliation;
412 is conflict, not retry. Other static errors, including 429, do not authorize retry.

Sealed validator-only results bind principal namespace, frozen intent/event/etag/
hash, cause and immutable origin, without serde/public constructor or durable ACK
authority. Literal loopback test evidence cannot become production authentication
proof. No ACK consumer/reducer/persistence/V3 receipt, exclusive worker or remote
Git coordination was added; operator simulation and V1/V2 formats remain unchanged.
See [contract](docs/product-architecture-roadmap.md#calendar-private-http-adapter).

Latest recorded main-session macOS validation: **430 normal workspace passed,
13 ignored**, serial `cargo test --workspace --locked --offline -- --test-threads=1`;
Calendar **145 normal/3 ignored**. All **15 actual reqwest loopback tests** passed
latest default-parallel and serial runs plus full suite; strict Calendar all-targets
Clippy, fmt/build passed. Sent-5xx P2 closed; builder-test delimiter corrected and
timing tests stabilized; final review found no dormant-adapter P1/P2. The six
real-MW gate passes remain **historical 415-checkpoint evidence, not rerun** for the
adapter; no new gated real-MW fixtures. Four legacy main-CLI warnings remain.
Only `127.0.0.1` HTTP was exercised, not live Google authentication/provider proof.
No actual notes/refs/state/provider/credentials/workflow/billing/installation or
capture-worker changes. Dependencies add declared references only, no lockfile
version upgrades/downgrades. This docs pass runs no tests/network/services or state
operations. See [adapter evidence](docs/release.md#calendar-private-http-adapter-checkpoint-2026-11-02).

Delivery gates:

- [x] Confirm notes repository host, visibility, default branch, and Actions availability.
- [x] Record committed-source, ledger, recovery, credential, and cost boundaries.
- [x] Scope v1 to the existing due date only; no start-date requirement. Preview an
      all-day Available deadline reminder. Defer start time, duration, timezone/DST,
      and bidirectional Calendar synchronization; preserve identity across future
      all-day/timed representation changes.
- [x] Define explicit ledger logical identity independent of source markers and dates.
- [x] Implement local enrollment/re-acknowledgement candidate generation, preserving
      logical IDs across explicit edits/moves without source markers or silent matching.
- [x] Implement reviewed local enrollment persistence; retain the approved UUID.
- [ ] Deliver normal-user approval/UI and integration activation; candidate JSON
      alone is not durable enrollment and hand-maintained IDs/JSON are not the workflow.
- [x] Implement the narrow credential-free source-only due-date preview over a full
      pinned Git commit, with visible identity/enrollment blockers and no publication.
- [x] Implement strict committed-ledger schema and advisory planning, including bad/
      missing-state refusal, ancestry, source changes/moves, pending retry blocks,
      retirement, markerless evidence, and byte-exact LF/CRLF acknowledgements.
- [x] Implement reviewed approval/apply semantic replay, dedicated bare state-ref
      whole-HEAD and path/blob-or-absence CAS, and exact-retry/cancellation recovery.
      Local code/test milestone only, under exclusive repository administration.
- [x] Implement offline frozen prepare/mark-uncertain V2 candidates, reviewed local
      persistence, and explicitly nonpersistable operator-asserted ACK simulation.
- [x] Implement the dormant private HTTP adapter and local loopback tests only;
      no production dispatch permit constructor or activation.
- [ ] Connect trusted in-process results to durable ACK validation/receipts/recovery,
      authenticated success ACK candidates,
      durable ACK CAS/reconciliation, and exclusive dispatch ownership, not just Git
      CAS. A remote API operation and Git persistence are not one transaction.
- [ ] Implement hosted CI serialization and remote lease/CAS push; local CAS is not
      remote-ref proof, remote synchronization, or authorization to deploy.
- [ ] Package a pinned Linux artifact and validate its runtime later before deployment.
      Linux is untested and explicitly deferred by the user, not a local-work blocker;
      do not repeat Docker checks or install targets now.
- [ ] Verify the account's included usage and paid-overage blocking before activation.
- [ ] Activate the private Calendar transport only after appropriate separate
      Calendar-write + OpenID OAuth and native or hosted secret storage;
      obtain explicit approval for hosted
      secrets, source-branch/workflow deployment, Calendar writes, and state-branch
      creation in real repositories. Only temporary fixture Git writes have occurred;
      no real source/state commits, pushes, secret changes, or billing changes.
- [ ] Implement the Calendar executor and hosted workflow after the above gates.
      Keep private source/ledger/output out of logs and automatic CI artifact uploads,
      including Atlassian; the user explicitly declined Atlassian uploads.

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

1. Retain live-verified [scheduled phone-to-editor delivery and repeat nonduplication](docs/google-tasks-capture.md)
   and the existing connection, queue, baseline, and receipts. Separately verify full
   pending-job dirty-buffer deferral and live computer-off catch-up. No Postgres prerequisite.
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
