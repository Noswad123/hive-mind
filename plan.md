# Hive Mind Status and Roadmap

Last updated: 2026-11-04

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

## Current priority — same-task Google Tasks first-due writeback

**Accepted behavior:** edit canonical Markdown → commit AND PUSH to private NOTES
repository `main` → GitHub-hosted workflow reads the exact pushed commit → update
the SAME existing Google Task (pilot due only). Not a Calendar event. Calendar is
deferred; its historical milestones and checklist below are retained, not removed
code or current activation instructions. Inbound capture stays live and
Google-read-only. Production/hosted writeback remains disabled; separately
authorized disposable-task live conditional proof succeeded Nov 4 (not activation).

Current checklist (local code/review milestone done; outbound activation not done):

- [x] Record same-task due-only target and defer Calendar.
- [x] Implement credential/provider-free portable receipt-backed selected capture
      handoff; review repo/account/list/provider-task/stable-capture binding and
      receipt provenance/digest. Accept the current baseline explicitly, never
      mislabel it original. Desktop SQLite is not synced to hosted runners.
- [x] Implement source-reader facade sharing committed snapshot machinery and
      first-due planner over full SHA plus explicit handoff/operator observation.
      Saved dirty notes are invisible; local commit alone is not push proof.
      Advisory outcomes: `set_first_due` / `already_satisfied` / `blocked`;
      `publicationReady`, `remoteVerified`, `pushVerified`, `conditionalWriteSafe`,
      and `hiveWriteProven` remain false.
- [x] Reconcile CLI/docs with reviewed implementation: **built-debug, not installed**
      `./target/debug/hive tasks handoff` / `tasks due-plan`, JSON stdout only.
      Record supplied implementation validation, not tests rerun for docs.
- [x] Implement/review separate local TasksWrite (`https://www.googleapis.com/auth/tasks`
      + `openid`) auth login/status/logout, isolated Keychain service/locks, without
      changing `personal` readonly or Calendar. Built-debug, not installed; explicit
      browser approval subsequently saved `tasks-write-personal`. Local isolation
      is not Google grant isolation; readonly lists succeeded after login/test.
- [x] Implement handoff-bound exact authenticated READ: input preflight before native
      setup, one refresh/fresh UserInfo, handoff namespace match, same-token exact GET.
      Unsigned live wrapper is collection provenance only; offline due-plan rejects
      wrapper/trust fields and explicit nested extraction stays operator asserted.
      No mutation, queue/note/Git changes, ACK, or activation.
- [x] Complete reviewed fake-only durable executor (`tasks fake`), sharing
      `hive_tasks::policy` with due-plan; fresh committed source, Prepared/Attempting
      CAS before private dispatch, revision-conditional due-only fake patch and
      frozen recovery that never PATCHes. Local process-crash proof only, not Google
      ACK, hosted coordination or production readiness. See
      [usage, fixture and state machine](docs/tasks-first-due-fake-executor.md).
- [x] Implement/review nondefault `tasks-conditional-probe` inspect/run and bounded
      private journal; default builds exclude adapter/command. Separate experiment,
      not a promotion of fake-only code or production workflow. See the
      [runbook and approval contract](docs/tasks-conditional-probe.md).
- [x] Obtain explicit live sign-in/read and date-change authorizations; test only
      user-nominated `Hive conditional write test` in `My Tasks`. API baseline Nov 5;
      current-ETag due-only PATCH Nov 6 accepted (200), stale-old-ETag PATCH back
      rejected (412 valid Google JSON); final and independent read-only verification
      retained Nov 6/new ETag/non-due fields. Two PATCHes, one accepted, no replay/
      rollback/delete. Real `Verify scheduled phone capture` and notes not edited.
- [x] Fix normal output-only `links: []` compatibility after GET-only inspect refusal;
      accept empty arrays only, retain malformed/nonempty/unknown-field refusal.
- [x] Implement/review immutable injected TasksWrite backend: existing strict native
      V1 codec, alias/account pins, process-instance lock only; every save fails with
      CredentialPersistenceRequired. Omitted/identical refresh token proceeds;
      rotation pauses before UserInfo/token release/GET. No secret-update API.
- [ ] Approve trusted hosted provisioning and manual credential pause/recovery;
      the old secret may be spent on rotation. No credentials in state Git.
- [x] Record limited single-target/session live exact-task conditional PATCH evidence,
      not a universal provider guarantee or production canonical ACK.
- [x] Implement/review real default-enabled local single-target `tasks publish`:
      separate bare fixed-ref Git state, reviewed intent/source/resource checks,
      Prepared/Attempting CAS, one conditional due-only PATCH, persisted response
      proof before confirmation and durable ACK, source-independent GET-only recovery.
      **Implemented/reviewed with validation limits, not ready for live activation**;
      no installation or live publisher operation this step. See the
      [runbook](docs/tasks-local-publisher.md) and
      [validation checkpoint](docs/release.md#tasks-local-publisher-checkpoint-2026-11-04).
- [x] Reconcile independent final validation: default-parallel task-crate **45 passed**,
      default CLI **25 passed / 1 ignored**, feature Tasks unit **41 passed** and
      publisher/probe CLI **4 passed**. Full workspace not rerun.
- [ ] Diagnose earlier intermittent failures/timeouts; one passing default-parallel
      rerun does not establish stability or explain their cause.
- [x] Implement/review default `tasks hosted check/plan`: consistent push-claim
      admission (not GitHub authentication), full committed-source policy and exact
      private parser-copy pin before execution/content reads, late secret lookup,
      structure-only offline check or fresh refresh/UserInfo/exact GET planning.
      Redacted summary only; no apply, state commit, adoption or native fallback.
      See [foundation contract](docs/tasks-hosted-foundation.md).
- [x] Record the foundation's final targeted validation in its
      [historical checkpoint](docs/release.md#tasks-hosted-read-only-foundation-checkpoint-2026-11-04);
      not a full-workspace, stability or Linux pass.
- [x] Implement/review read-only `tasks hosted verify-main`: existing claims/source/
      private-copy-pin preflight before late `GITHUB_TOKEN` only; fixed private
      GitHub identity/exact-main GETs. Collection observation, not push authentication
      or dispatch authority. No live token read/GitHub calls this step.
- [x] Review remote expected-OLD Git push CAS and integrated intent/Attempting/
      evidence/ACK/recovery protocol **entirely under cfg(test)**. No remaining
      P1/P2 in reviewed laboratory scope; temporary local bare receive-pack and
      synthetic HTTP only. See [protocol/limits](docs/tasks-remote-coordination.md).
- [ ] Reconcile final remote full task-crate/probe/feature CLI/Clippy runs: PENDING
      coordinator final report; [completed targeted evidence](docs/release.md#tasks-remote-authority-checkpoint-2026-11-04)
      is not final full-suite/workspace or stability proof.
- [ ] **Next:** production GitHub HTTPS credential bridge with secret-free URL/argv/
      config/logs and process isolation; reviewed inception/independent root pin and
      quiescent local-to-hosted handoff; production coordinator construction and
      hosted writer wiring with credential pause/recovery. These code bridges are
      absent, not merely deployment tasks. Local permits/locks are not fencing.
- [ ] Complete hosted production coordination and inbound ACK integration;
      retain inbound receipts/review, including future own-write `provider_changed`,
      until explicit outbound ACK integration exists.
- [ ] Deliver serialized trusted-main workflow with immutable pushed SHA/freshness,
      durable state, included-allowance paid-overage hard stop, and Linux validation
      before deployment. Linux validation remains deferred now.
- [ ] Obtain separate production activation approvals for hosted grants/secrets/state,
      workflow deployment and selected first-due writes; disposable-probe approvals
      do not authorize them. No workflow is added in this slice.

See [implemented local interfaces and activation boundaries](docs/google-tasks-writeback.md),
the [new local publisher validation limits](docs/release.md#tasks-local-publisher-checkpoint-2026-11-04),
and the [live proof/post-fix checkpoint](docs/release.md#live-tasks-conditional-proof-2026-11-04).
The earlier [fake-executor checkpoint](docs/release.md#tasks-first-due-fake-executor-checkpoint-2026-11-04) records:
final hive-tasks **21 passed**, independently confirmed; CLI Tasks **22 passed / 1
ignored**, Tasks unit **5 passed**. Repeated runs are not additional distinct tests.
The earlier auth/read **242 passed / 9 ignored** remains its own historical checkpoint. No latest
full-workspace green claim: it was not rerun this milestone. The earlier intermediate
486/14 serial pass and later unresolved/unconfirmed-cause Calendar loopback failures
remain [historical evidence](docs/release.md#google-tasks-local-first-due-checkpoint-2026-11-03).
Post-fix independent validation: capture **69 passed / 2 ignored**, probe unit
**36 passed**, Tasks integration **22 passed / 1 ignored**, probe CLI **2 passed**;
strict capture/feature CLI Clippy (existing CLI allowances), fmt/diff checks passed.
Prior auth **71 passed** was not rerun in the live session. The new credential
profile was intentionally saved and the disposable task left open Nov 6; worker,
queue/baseline, canonical notes, source refs and workflows were unchanged by the
probe work. No code commit/push/install; this docs pass runs no tests or shell/native calls.

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

### Historical accepted slice: hosted Calendar publication (deferred)

> **Superseded target as of 2026-11-03.** The Calendar milestones, supplied test
> evidence, and delivery gates below are historical/deferred, not current priority.
> The accepted next slice is same-task Google Tasks first-due writeback above.
> Existing Calendar code is retained; no Calendar activation is authorized.

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
adapter and private local ACK stage below now exist, without production integration
or activation; simulation remains nonpersistable.

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
proof. At that adapter-only checkpoint no ACK consumer/reducer/persistence/V3 receipt
existed; the next stage below adds local durability, not an exclusive worker or
remote Git coordination. Operator simulation and V1/V2 candidate formats remain unchanged.
See [contract](docs/product-architecture-roadmap.md#calendar-private-http-adapter).

Historical adapter-only main-session macOS validation: **430 normal workspace passed,
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

**PRIVATE in-process ACK -> local CAS code/tests DONE, NOT activation.**
`calendar_transport/ack.rs` pins exact committed base/path/blob, old ledger and
full pending (including phase) before HTTP, attaching `contextDigest` to intent
and sealed result. Google-only admission matches all basis/namespace/IDs/intent/
event/hash/strong-ETag evidence; actual loopback origin is always refused. No JSON
or trust flag creates ACK authority. Explicit `LedgerChangeProof` separates Source,
Intent and Acknowledgement with private-constructor `SealedAckProof`. Ordinary public
apply remains V1 source/V2 intent only: no new ACK/publish CLI or authenticated JSON.

Prepared/Uncertain create/update ACKs need no current source/parser/task eligibility.
Only selected Active/provider ACK/result/pending None change; CURRENT old source ACK
(not intendedSource), watermark/order/tombstones/unselected evidence stay intact.
GET `ReconciledObservation` is not worker-dispatch proof. Borrowed
`PinnedAckTransaction` retains original base/result through errors and rebuilds
the same commit from OLD base each retry, even after latest pending clears. Exact
retry succeeds without writes; later advancement refuses, never rebases. Bare,
fixed-ref, no-worktree, exclusive-admin, CAS and cancellation policies are unchanged.

V3 owned/apply receipts validate old parent ledger/full pending phase/result and
exact new reduction. V1/V2 goldens stay unchanged; all following Source/Intent writes
stay V3, without minimum-reader downgrade. Audit/provenance floors distinguish
GoogleAuthenticated and TEST-ONLY SyntheticFixture. Test factories force Synthetic
and recompute digests, never Google from loopback; all synthetic continuations
retain that floor. Production excludes the variant and rejects fixture branches,
including after pending clears. Digests/receipts are not signatures. Generic
publication/remote/ref verification stays false, not a current-remote-sync claim.

At the ACK checkpoint, dispatch child opaque `RemoteDispatchAuthority` + `FreshAuthoritativeView` were
consumed against head/path/blob/context/owner nonce/intent/origin/Prepared matching,
without production constructors, Serde or Clone. Google permits require authority
basis; test factories are loopback-only. This is NOT remote leasing/fencing: local
CAS/nonces/freshness and single-use values cannot stop a paused old cross-host
worker; no expiry takeover exists. Production minting/safe remote ownership remains
UNIMPLEMENTED. The creator-only coordinator seam below adds fixture-backed acquisition,
not production authority. See [ACK contract](docs/product-architecture-roadmap.md#calendar-private-ack-persistence).

Historical ACK FINAL GREEN in the main implementation session: default-parallel
`cargo test --workspace --locked --offline`, **444 normal PASS / 13 ignored**;
Calendar **159 normal / 3 ignored**. Strict Calendar all-targets Clippy `-D warnings`,
fmt/build passed. All six real-MW compatibility gates reran earlier this session
and passed (CLI apply x2, candidate, plan, preview, source). New ACK writer tests
are synthetic trusted handoff/TEMP repos, NOT a real Google authentication pipeline.
Actual loopback admission refusal, frozen create/update/both phases, tamper,
SHA-1/SHA-256 V3 CAS/exact retry after injected post-CAS unknown, later-advance refusal,
continuation floors and reader compatibility passed. Focused review: no P1/P2 for
private local ACK under bare/exclusive administration, not production approval.

Earlier full serial/default runs failed. Test-only capture output-cap timeout became
2 seconds (separate 100 ms timeout case retained); production capture runner did not
change. HTTP fixtures now record complete headers/body without consuming replies on
empty requests, use 10-second stream timeouts and one mutex per HTTP test, plus a
cfg(test) shared OnceLock blocking client/pre-listener warmup. Production HTTP config,
error classes, request-count assertions and retries were not relaxed. Root cause is
NOT established: accepted=1/complete=0 reproduced and ended on cleanup, not lifetime
expiry; slow-client-build theory did not reproduce. After pooling, bounded-response
passed ten consecutive times, 16 HTTP tests including ACK twice serial plus parallel
module passed, then Calendar 159 parallel and final workspace 444 parallel passed.
**No full serial 444 pass is claimed.** See [full stability evidence](docs/release.md#calendar-private-ack-checkpoint-2026-11-02).
Older 430/415/389 checkpoints remain historical. Tests were run by the implementation
session, not the user or this docs pass. No actual notes/source/refs/Google/Keychain,
private queue/credentials/services/binary/settings/worker, deployment/secrets/billing
or Git commit/push changed; implementation writes were TEMP-only. Linux remains
explicitly deferred without probes/VMs/targets; no tests/builds/network ran for docs.

**PRIVATE CREATOR-ONLY COORDINATOR SEAM code/tests DONE, not hosted execution.**
Private `calendar_transport/coordinator.rs` seals `AuthoritativeState`; no production
backend implementation/constructor, credential wiring, CLI/export or workflow exists.
The dispatch code is a receipt-consuming bridge, not a usable production factory.
Only fresh successful exact H0 -> H1 CAS in the SAME invocation produces opaque
`FreshCasReceipt`, consumed once with its invocation nonce and exact latest
head/path/blob/binding/authority ID/origin. Any Prepared/Uncertain pending anywhere
in the binding returns `RecoverOnly` before staging/publication. Own old head,
same-job rerun, copied nonce, local ApplyReceipt/`already_applied`, logs or matching
H1 readback cannot acquire. No expiry/steal/reset/reprepare or blind retry exists.

Staging checks actual sole-parent H1 objects in the fixture backend and exact
single-prepare ledger semantics, retaining source/provider ACKs, watermark, order,
tombstones and unrelated entries. Source eligibility is delegated to the preparer,
not freshly validated against notes by the coordinator. Root checks after CAS/latest
view and after CPU mint discard Ready on cancellation/expiry as PublicationUnknown;
the consumed receipt cannot retry, and the next invocation sees recovery-only pending.
Caller MUST still check the root before HTTP: this is not atomic revocation.

Ready's context is an **H1 policy pin (not H0), with default/empty repository path**.
Full worker integration must materialize/verify H1 and load actual local PinnedContext
with a real repository before HTTP/durable ACK. The slice is NOT a full HTTP + local
ACK + authoritative publication executor. Separate ACK policy has no production
AckPublication builder; Completed requires confirmed exact authoritative ACK CAS,
not local ACK objects, unknown or conflict. Post-publication root cancellation is
unknown. Lost ACK replies may mean committed H2/pending cleared, not always unchanged
pending; no repeated provider write and no implemented remote ACK readback recovery.

Latest macOS implementation-session evidence:
`cargo test --workspace --locked --offline -- --test-threads=4`, **464 normal PASS /
13 ignored**; Calendar **179 normal / 3 ignored**; new coordinator **20 tests passed
serially**, including post-mint cancellation/expiry discard and next-invocation
no-publication. Strict Calendar all-targets Clippy `-D warnings`, fmt/build passed.
The prior filtered transport subset passed 43 tests with 19 coordinator tests;
all current 44 transport tests were included in the full workspace run, but no
separate 44-test serial target run was performed. Six real-MW gates
remain historical 444-checkpoint passes, NOT rerun; no new MW gate. Main CLI final
`clippy --no-deps` passed with four existing legacy warnings, not claimed fixed.
Focused review readiness is private protocol only,
not publisher readiness. See [contract](docs/product-architecture-roadmap.md#calendar-private-creator-only-coordinator)
and [release evidence](docs/release.md#calendar-private-coordinator-checkpoint-2026-11-02).

Test-only in-memory and owned TEMP bare authority adapters use a separate local
worker bare repo, bounded local Git packs and exact atomic `git update-ref NEW OLD`,
not ordinary push. Only loopback HTTP; Synthetic V3 ACK stays local fixture evidence,
never Google, and actual loopback results still fail the private ACK gate. Local
CAS/nonce/latest view is NOT remote lease/fencing/exactly-once: a paused creator may
make its late first attempt, but a second invocation never writes. Non-ABA stale/
advanced/rollback/missing heads fail without rewind; Git ABA/admin rewind is out of
scope and needs cooperative writers/exclusive admin/remote controls later. Actual
remote CAS must prove exact expected OLD in a server-atomic transaction; unconditional
REST ref update, ordinary FF push or readback is insufficient. No actual user refs,
notes, provider, credentials, services, installs, worker, CI, secrets or billing
changed, and no paid service was provisioned. Linux remains explicitly DEFERRED,
with no probes/VMs/targets. No tests/builds/network or uploads/offers in this docs pass.

Historical Calendar delivery gates (deferred, not the current checklist):

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
- [x] Implement private pinned in-process ACK validation/reduction, V3 receipts and
      local CAS/exact recovery, synthetic test floors and gated dispatch seam only.
      Code/tests DONE, not activation or real Google authentication-pipeline proof.
- [x] Deliver the private creator-only coordinator protocol seam and TEMP backend
      tests, not a production backend, publisher or full worker integration.
- [ ] Deliver full worker integration, materialized/verified H1 local pin and real
      provider grants/credentials; implement authoritative ACK readback/recovery.
- [ ] Deliver queue/dispatch policy operations and source-policy/approval UI.
- [ ] Implement separately reviewed fixed-identity remote exact-OLD CAS backend;
      unconditional REST ref updates, ordinary FF pushes and readback do not suffice.
- [ ] Implement remote Git authoritative ownership/fencing beyond single-use values
      and local CAS; a paused cross-host worker is not fenced by local freshness.
      A remote API operation and Git persistence are not one transaction.
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
