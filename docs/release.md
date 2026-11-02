# Hive Release and Packaging

## Current release path

The canonical binary name is `hive`. For now, releases are source-built Rust
binaries plus optional GitHub release artifacts.

Recommended local install:

```bash
make install
```

This builds `crates/hive` in release mode and installs the binary to
`~/.local/bin/hive` by default.

### Local installation checkpoint: 2026-11-01

The stable release was successfully built and installed at
`/Users/jdawson/.local/bin/hive` on 2026-10-31, then rebuilt and installed at that
same path after the 2026-11-01 process-inventory fixes. The user reconfirmed latest
foreground `auth status` and `lists --connection personal` success. Latest-build
no-UI background Keychain access is positively proven while unlocked, not for
denial/locked scenarios. Earlier manual Android capture and
duplicate-free repeat proof remain intact; an earlier read-only queue checkpoint
found one delivered receipt with one writer attempt, before the new scheduled proof below.

For foreground verification after a binary change, use that stable path, then
exercise refresh and Google access with `lists`, handling any OS prompt there.
These checks already succeeded for the current build:

```sh
/Users/jdawson/.local/bin/hive captures auth status --connection personal
/Users/jdawson/.local/bin/hive captures lists --connection personal
```

The worker is configured against the existing personal queue/list/baseline with
fixed public environment and MindWeaver cwd (interval 60 seconds, timeout 45 seconds,
limit 1); `configValid` and `queueScopeValid` are true. After clean/idle checks, the
live editor was hotloaded in place preserving `mw_ops` identity and its active
guard across four shared references, and the bridge registered. After user-authorized
orphaned ChatGPT helper cleanup, installed `editor-check` reports `ready: true`,
`noEditor: false`, `deferred: null`. No identity check was bypassed. A live unsaved
hidden read-only normal-buffer probe verified `dirty_buffers` readiness deferral,
without an on-disk file or note edits. The guard stayed unchanged, `busy: false`;
the probe was removed after changed-tick/name/visibility checks and clean readiness
restored. Full pending-job dirty-buffer delivery deferral is not live-proven.

At the earlier polling checkpoint, two controlled `run-once` cycles completed in about 0.5 seconds each: observed 1,
unchanged 1, staged/delivered 0, `authActionRequired: false`, generation 1.
Explicit background install/start then **enabled scheduled polling**:
`serviceActive: true`, `serviceState: loaded_owned`. Actual LaunchAgent startup and
next scheduled cycles completed about 60.7 seconds apart, with no pending work,
observed 1 unchanged, zero staged/delivered, and no auth action required. Native
LaunchAgent ownership/status parsing is live-verified. At that checkpoint the original
receipt remained attempt 1 with no duplicates or Inbox/dashboard changes.
Earlier 45-second unknown outcome and 12-second local status stall did not reproduce;
inherited/minimal local diagnostics succeeded in 0.054/0.028 seconds. The same
build/credentials work; the credential-operation failure is resolved operationally,
with no established root cause or speculative source fix.

New scheduled phone-to-editor-owned writer delivery and repeat nonduplication are
now live-verified (2026-11-01). The user added the undated, title-only phone task
`Verify scheduled phone capture`; an automatic scheduled cycle, not `run-once`,
completed with observed 2, staged 1, unchanged 1, delivered 1, pending 0. Its durable
receipt retains one attempt and matches the actual Inbox revision. Actual Markdown
and recursive whole-notes-root search found exactly one unchecked stable-marker
entry. The same-cycle native Neovim bridge completed with positive writer-exit
proof and zero conflicts, leaving no busy operation or dirty buffers. The next
actual scheduled cycle observed 2 unchanged, staged/delivered 0, `no_pending`, no
auth action required, with the owned service active. Both captures retain one
attempt and their receipts. Inbox (324 bytes) and dashboard (4,651 bytes)
bytes/sizes/mtimes stayed unchanged from post-delivery to follow-up; dashboard content
retained its earlier hash, but its mtime changed on first delivery only. No Google
completion, source completion, or archival was performed.

See the [background activation runbook](google-tasks-capture.md#background-activation-enabled-and-verified)
for cycle evidence and stable-path `background status`, `stop`, `start`, and
`uninstall` controls. Stop retains config/queue; uninstall removes only the owned
plist and retains all state. Polling runs in the GUI user session at a 60-second
interval, 45-second timeout, limit 1. Google holds work while the computer is off;
no server is required. Live computer-off catch-up and full pending-job dirty-buffer
delivery deferral remain unverified; read-only dirty readiness is proven. `background install`
writes the owned plist only; `background start` loads it with `RunAtLoad`.
Neither `make install` nor building the binary activates the worker. Calendar publication,
export, completion writeback, lifelog, and PWA migration remain pending.

At the inventory-fix checkpoint, 270 normal workspace tests passed
with five ignored; strict all-targets Clippy passed for background/auth/capture;
both ignored isolated real-Neovim tests passed. The narrowly scoped SDK-typed C
`sysctl` zombie verification and checked 32-bit Darwin UID normalization do not
ignore generic inventory errors or change the group-death guard; review found no
P1/P2 issues. The inherited-FD test was isolated in a single-test child to avoid
parallel fork-test leakage, retaining original assertions and production semantics.
Both real-writer integrations passed earlier, not rerun after these fixes. The
fifth ignored crash fixture is exercised by normal recovery tests. Earlier format
and Neovim background/safety checks (32/54 tests) passed on 2026-10-31. None of these
checks alone proves live scheduled operation or computer-off catch-up. Scheduled
phone-to-editor-owned writer delivery and repeat nonduplication have separate live
evidence above. No additional tests ran during activation or this live proof;
the separate Calendar implementation validation is recorded below.

Override the install prefix when needed:

```bash
make install PREFIX=/usr/local
```

### Calendar preview checkpoint: 2026-11-01

Historical source-only checkpoint, retained separately from the planner below.
The source-only preview was built at `./target/debug/hive`, not installed in the
stable release. Use the [explicit pinned-commit command](../README.md#calendar-source-preview-local-only),
with Git 2.50+ at `/usr/bin/git` and an absolute trusted standalone MW binary path.
`--commit` requires a full lowercase commit SHA, not HEAD/ref/abbreviation; resolve
HEAD explicitly first. Working-tree/untracked notes are excluded, and a local SHA
does not establish that the commit was pushed. No real-notes preview was invoked
this session; all preview proof used fixtures.

Recorded implementation validation (not rerun for this docs-only update):

- `cargo test --workspace --locked --offline -- --test-threads=1`: 303 normal tests
  passed, eight ignored. The eight include two new real-MW integrations, one new
  internal cancellation helper exercised by its parent test, and the prior five.
- Both explicitly gated installed-MW integrations passed: source fixture and CLI,
  including valid leap-day due dates, malformed inline area, and `Due :` parser
  mismatch. They used isolated fixtures, never actual notes.
- Formatting and strict all-targets Calendar Clippy with `-D warnings` passed.
- CLI all-targets Clippy passes without warnings-as-errors. Its strict gate still
  fails on pre-existing legacy sync argument-count/lifetime warnings; those were
  not changed as part of the Calendar preview.
- An earlier default-parallel workspace run hit a pre-existing 100 ms capture
  output-cap test flake (timeout instead of expected cap). Targeted rerun and serial
  workspace run passed; no unrelated code fix was made.
- Focused review closed all prior P2 findings: ready for local preview only.
  macOS is the only installed Rust target; Linux runtime remains unverified. The
  Docker daemon was unavailable and was not started.

The preview bounds committed Markdown materialization and child I/O/runtime;
exact limits are in `crates/hive-calendar/src/{source,process}.rs`. Normal
SIGINT/SIGTERM cleanup removes private child process groups and workspace;
SIGKILL/abrupt host termination cannot guarantee cleanup. Trusted Git/MW input
isolation is not an OS sandbox. Stdout contains private task titles and paths:
do not paste it into public CI logs.

At this earlier checkpoint there was no ledger-aware/provider plan, provider state, or Calendar publication;
`publicationReady` is always false, including for stable IDs. Existing installed
Hive/MW binaries and the active background worker were untouched. No auth,
credential, network, Calendar-write, Git-ref, ledger-installation, workflow-push,
deployment, or billing changes were made. Linux artifact packaging and the
[hosted publication gates](product-architecture-roadmap.md#accepted-next-slice-hosted-calendar-publication-after-push)
remain pending. This documentation update adds or runs no automated tests.

### Calendar plan checkpoint: 2026-11-02

Historical planner checkpoint, retained separately from candidate generation below.
Built-debug `./target/debug/hive calendar plan` provided credential-free,
committed-ledger-relative advice alongside the retained preview. See
[usage](../README.md#calendar-publication-plan-local-only) and the
[strict V1 ledger contract](product-architecture-roadmap.md#calendar-ledger-schema-v1).
Explicit bootstrap is observation-only; it never installs authoritative state,
assigns UUIDs, or enrolls. Existing bad/missing state fails closed. Source HTML
markers are optional for existing explicit logical-ID maps. At this checkpoint,
enrollment and reviewed re-acknowledgement commands were unimplemented; manual JSON maintenance is not the
intended long-term workflow. Whole-file edits/moves and different parser binary
builds/platforms require review/new acknowledgement under the same logical ID.

Recorded **macOS implementation validation**, supplied for this docs-only update,
not rerun by this update:

- `cargo test --workspace --locked --offline -- --test-threads=1`: 339 normal tests
  passed, nine ignored.
- Calendar crate: 62 normal tests passed, two ignored. CLI `calendar_plan`: five
  normal tests passed, one ignored.
- The new explicitly gated real-MW markerless plan integration passed with both LF
  and CRLF. Both prior real-MW source-preview gates were also rerun and passed at
  this implementation checkpoint. All used isolated fixtures, not live user notes
  or an actual publication ledger.
- Strict all-targets Calendar Clippy with `-D warnings`, formatting, build, and CLI
  help checks passed. This does not claim the legacy CLI's strict Clippy warnings
  recorded above were fixed.
- Focused review closed the CRLF P2 and bootstrap acknowledgement-validation gap;
  no local P1/P2 findings remained. Exact CRLF literal bytes round-trip unchanged,
  and bootstrap acknowledgement shapes/limits are checked before output.
- **Linux is untested, explicitly deferred by the user until later before
  deployment.** No repeated Docker checks or target installs are requested; local
  work is not blocked on Linux proof. Exact binary-digest provenance is not
  semantic cross-platform parser proof.

Output decisions are `noop`, advisory `update`, `review`, `blocked`, or `retired`,
never create/delete. `noop` means payload equality with the last provider
acknowledgement, not remote existence/currentness. Pending prepared/uncertain
operations block retry; retirement does not resurrect, and completed/undated/
invalid source is reviewed, not deleted. All plans remain `publicationReady: false`,
`remoteVerified: false`, `ledgerHeadVerified: false`, and
`requiresLedgerCompareAndSwap: true`; no executor or latest-head proof is delivered.

Existing installed Hive/MW binaries, capture worker, private queue, and auth were
untouched. No live user-note/ledger tests, credentials, refs/commits/pushes,
workflows/cloud resources, secrets, or billing changed. At this checkpoint,
enrollment/re-acknowledgement tools, state-branch CAS writes/recovery, Calendar write
auth/executor, hosted workflow, account cost-hard-stop verification, and later Linux
validation were future gates; candidate generation is delivered below.
Private content/IDs in ledger and stdout must not be logged or auto-uploaded as CI
artifacts, including Atlassian. The user explicitly declined Atlassian uploads.
This docs-only update runs no tests/builds/services/network operations; its only
command validation is a documentation diff whitespace check.

### Calendar candidate checkpoint: 2026-11-02

Historical candidate-only checkpoint, retained separately from persistence below.
**Candidate generation only DONE at this checkpoint:** built-debug `./target/debug/hive calendar enroll`
and `reack`, not installed. See [flags and behavior](../README.md#calendar-ledger-candidates-local-only)
and [candidate contract](product-architecture-roadmap.md#calendar-candidate-generation).
Both require all source/binding/ledger inputs and exact relative Markdown path /
positive one-based line selection. Enrollment assigns an OS-entropy UUIDv4 without
markers or note edits. Explicit `--initialize-ledger` proves path absence only at
the full base commit, with any existing parent components trees, not authoritative
HEAD or remote absence; normal missing state never initializes. Open valid-due
source is required. Duplicates/reservations including retired evidence refuse.
The global enroll guard requires every old basis recognized, even retired, and no
pending operations. Missing/moved/edited retired basis can block unrelated new
enrollment until a future resolution workflow; no force override or friction-free
restore is promised.

Reack explicitly selects an old path/line or advanced existing UUID, never both,
then maps the current path/line under that UUID. Entire old proof is validated
before replacement; provider ACK/lifecycle and unselected pending/tombstones are
preserved. Pending/retired targets refuse. Parser-change acceptance is explicit
selected-entry approval, not old-semantic verification. Done/genuinely undated ACKs
do not propagate completion/deletion, and invalid/duplicate/malformed due evidence
refuses even for completed source.

Recorded **macOS implementation validation**, executed in the implementation
session and not rerun for this documentation update:

- `cargo test --workspace --locked --offline -- --test-threads=1`: 363 normal tests
  passed, ten ignored. The earlier 303/339 counts above remain historical checkpoints.
- Calendar crate: 82 normal tests passed, two ignored. CLI `calendar_candidates`:
  four normal tests passed, one ignored.
- All four explicitly gated real-MW integrations ran and passed: CLI candidate,
  plan, and preview, plus source-reader fixture. The new candidate gate covers
  markerless enrollment, move reack retaining UUID/provider ACK, done ACK, and
  refusal of invalid due evidence on completed source.
- New nonignored CLI coverage proves forged old acknowledgements cannot be washed
  out by reack and source rollback refuses with empty stdout.
- Strict all-targets Calendar Clippy, formatting, and build passed. Focused review
  found no P1/P2 issues for local candidate generation. This does not claim the
  historical legacy CLI strict-Clippy warnings were fixed.
- Fixtures used temporary repositories only; fixture commits were confined to those
  repositories, never actual notes or publication state. No Linux runtime/testing,
  VM, target installation, or repeated Docker probes were performed, by request.

Output is a fully buffered serialized `calendar-ledger-candidate` with operation,
source/parser provenance, logical ID, exact base commit/path/blob-or-absence
condition, and whole validated V1 candidate ledger. Publication/remote/head verified
flags are false and CAS is required. Generation persists nothing; enrollment IDs
can differ on rerun until accepted. Never publish from unpersisted candidates. At
this checkpoint, next local work was the reviewed approval/apply writer, proposal
validation, authoritative state-branch
HEAD **and** path/blob-or-absence CAS, and recovery. Persist the accepted ID before
provider dispatch; an exact base blob is not HEAD proof. Enrollment persistence and
normal-user UI were future work, not manual JSON maintenance. Local persistence is
delivered in the next checkpoint; normal-user UI remains pending.

At this checkpoint no actual Git commits were approved. Calendar OAuth write authorization,
publisher, hosted workflow/deployment, and verified billing hard-stop activation
remain pending. Linux runtime follow-up is still required before hosted deployment,
not a blocker for local development. No installed Hive/MW binary swap, capture
worker/private queue/auth change, actual ledger/source/state/provider write,
workflow/deployment/billing/secret change occurred. Private candidate/ledger output
must not be logged or auto-uploaded; Atlassian uploads were explicitly declined.
This docs-only update runs no tests/builds/services/provider or real-notes commands;
only Git diff inspection and whitespace checks are performed.

### Calendar local persistence checkpoint: 2026-11-02

Historical persistence checkpoint, retained separately from offline operations below.
**Local persistence code/test milestone DONE; integration activation not done.**
Built-debug, not installed, `calendar verify-candidate` performs read-only approval
replay and deterministic blob/tree/commit OID computation with no object/ref writes.
`calendar apply` explicitly writes local Git objects and performs one atomic
old-OID CAS of `refs/heads/hive-calendar-state`, never a push or Calendar call.
See [review-first command examples](../README.md#calendar-local-persistence) and
[the full contract](product-architecture-roadmap.md#calendar-local-approval-and-persistence).
The earlier 303/339/363 workspace counts remain historical, not superseded claims
about those earlier runs.

Both commands require the same explicit source commit/repo/MW and independent
bindings, absolute dedicated bare ledger repository, relative JSON ledger path,
fixed state ref, absolute regular JSON candidate, and independently approved
64-lowercase-hex SHA-256 of its exact bytes. Initial ref creation alone requires
`--create-ref`; a selected reack's old-parser migration requires independently
renewed `--accept-parser-change`. Capture is once, bounded to 8 MiB, with
`O_NOFOLLOW`; unknown/duplicate/trailing JSON refuses. This is exact-byte approval,
not a signature or provider proof. Before mutation, original ledger/history is
validated and enroll/reack is replayed for whole-candidate equality; a digest is
not arbitrary replacement authority. Approved enrollment UUIDs are never
reallocated; persisted ledger JSON is canonical and compact.

V1 accepts only a dedicated bare files-backend state repository with no linked
worktrees, refusing a normal notes worktree, foreign existing tips, symbolic/dangling
refs, and object-store indirection. Updates require owned strict tool-shaped
ref/path/binding/blob receipts (not cryptographic signer proof), whole-commit base
HEAD equality, and matching old blob. Init requires ref absence via zero-OID CAS;
candidate path absence at the seed alone is neither branch nor remote absence
proof. The resulting sole-parent commit inherits the **whole seed tree, possibly
notes**, not an orphan branch. No adopt/reset/force/push is supported. Raw-tree
plumbing preserves unrelated modes/OIDs without checkout/index/config edits,
hooks/signing/maintenance. Fixed identity and UTC parent committer time + 1 second,
not approval time, make verify/apply expected commit IDs identical.

**Operator MUST exclusively administer the state repository during an operation.**
Preflight is not atomic protection against concurrent worktree/config administration;
CAS coordinates ledger writers, not admins. The focused review's readiness is for
local persistence under this bare/exclusive-administration assumption, not general
deployment.

Recorded **macOS implementation validation after the root-cancellation fix**,
executed in the implementation session and not rerun for this docs-only update:

- `cargo test --workspace --locked --offline -- --test-threads=1`: **389 normal tests
  passed, 12 ignored**.
- Calendar crate: **105 normal passed, 3 ignored**. CLI `calendar_apply`: **3 normal
  passed, 1 ignored**.
- New explicitly gated real-MW apply fixture passed: initialization candidate,
  read-only verify, local apply, exact retry, source move/reack, verify/update, and
  stale-retry refusal. All four earlier real-MW gates were rerun and passed, for
  **five gated fixture passes** total.
- Root phase-boundary SIGINT and SIGTERM coverage passed, including helper-return
  gaps; pre-CAS cancellation after actual object writes stops ref advancement. An
  actual successful CAS followed by injected cancellation reports unknown, then
  exact retry returns `already_applied` with no writes.
- Strict all-targets Calendar Clippy, formatting, and build passed. Focused review
  closed the P2 lost-cancellation finding. The main CLI's four original legacy
  warnings are unchanged; this does not claim a new strict main-CLI Clippy pass.
- Tests used temporary repositories only, with actual state-writer Git writes
  confined to temporary bare state repositories. No actual user-note/source/ref
  changes or new real state repository occurred. No Linux runtime test, VM probe,
  target install, or repeated Docker check was performed.

Verify returns `verified`/`already_applied`; apply returns `applied`/`already_applied`.
`localRefUpdated` is true only for an actual successful CAS. Publication, remote,
and remote-ref verification flags remain false. An exact retry at the expected
tip writes nothing; later advancement refuses, never rewinds or accepts ledger
content equality alone. Recovery requires the same approved candidate, destination,
bindings, source commit, pinned parser artifact, and permission flags.

The root cancellation guard covers capture, replay, writer, and CAS. Cancellation
observed before CAS stops ref advancement; failure after object writes can leave
unreachable objects with no automatic GC. After CAS dispatch, cancellation/lost
response or stdout failure may mean success: report unknown, retry exactly, never
roll back. This is local ref recovery, not a provider transaction protocol.

Installed Hive/MW binaries and worker/private queue are untouched. No actual
notes/source/state activation, hosted workflow, Calendar/grant/credentials/Keychain,
provider, billing, or remote changes occurred. No remote sync or Calendar writes.
Normal-user UI/integration activation, durable provider intents/results (remote
APIs and Git are not one transaction), hosted CI serialization and remote lease/CAS
push, pinned Linux runtime, account paid-overage hard stop, and Calendar OAuth
grant/executor remain pending. No paid service by default. **Linux is explicitly
deferred until later before deployment, not a local-work blocker.**

Candidate/ledger/output privacy remains unchanged: do not log or auto-upload private
content/IDs as CI artifacts; Atlassian uploads were explicitly declined. This
docs-only update performs no tests/builds, services, network, provider, or real-notes
commands, and no Git writes; only read-only source/diff inspection and documentation
whitespace checks.

### Calendar offline operation checkpoint: 2026-11-02

Historical offline checkpoint, retained separately from the private adapter below.
**Offline stage DONE, not an actual publisher.** Built-debug, not installed,
`calendar prepare` generates a persistable frozen create/update intent candidate;
`calendar mark-uncertain` generates a persistable monotonic phase-only recovery
candidate; `calendar simulate-ack` generates only an operator-asserted,
nonpersistable hypothetical result. See
[commands and exact flags](../README.md#calendar-offline-operations) and
[the schema/encoding contract](product-architecture-roadmap.md#calendar-offline-operation-contract).
Older **389/363/339/303** results above remain historical checkpoints, not counts
for this stage. This evidence was **executed by the main implementation session**,
not attributed to the user and not rerun during this docs-only pass.

Actual macOS implementation validation:

- Serial `cargo test --workspace --locked --offline -- --test-threads=1`:
  **415 normal passed, 13 ignored**.
- Calendar crate: **130 normal passed, 3 ignored**. CLI `calendar_apply`:
  **4 normal passed, 2 ignored**.
- **All six explicit real-MW gates ran and passed**: two apply fixtures (including
  the new offline operation flow), CLI candidate, plan, preview, and source-reader.
  Ignored-by-default does not mean those six gates were unexecuted.
- Strict all-targets Calendar Clippy, formatting, and build passed. Review found
  no remaining P1/P2 for the local offline scope; CLI end-to-end follow-ups are now
  done. This does not claim historical legacy main-CLI warnings were fixed or a
  new strict main-CLI Clippy pass.
- Wrong observation namespace/binding, logical/operation/managed IDs, intent digest,
  event, etag, payload, and forged provenance are rejected in unit coverage.

The new main CLI fixture actually exercised the following sequence, rather than
only constructing fake envelopes:

1. Enroll/apply V1, then prepare/verify/apply V2, preserving source ACK/watermark/
   lifecycle/provider ACK while persisting selected Pending Prepared.
2. Confirm global pending blocks another prepare; remove the current notes
   repository and parser artifact. Mark-uncertain generation, verification, and
   apply still work from the frozen ledger. Exact apply retry is `already_applied`.
3. Restore preparation inputs before testing the stale prepare against the advanced
   state ref. This is a **true stale-ref CAS rejection**, not an unavailable-source
   failure; objects remain unchanged. Remove source/parser again for recovery.
4. Confirm simulate-ack requires `--assert-operator-observation`. Generate actual
   correct simulation output from frozen pending state, hash its exact bytes, and
   pass that correct approved SHA to ordinary **verify and apply**. Both reject,
   with unchanged objects/refs, retained pending, and provider ACK still None.

Prepare requires independent binding, source/MW, committed ledger, and either
logical ID or old acknowledged path/line; no retired target, any pending, stale
source/parser basis, closed/undated/invalid source, or payload-equal active no-op
is accepted. Creates reserve deterministic namespace/UUID `h1` plus SHA-256 hex;
updates retain old event/base etag with changed managed payload. A bounded fresh
UUIDv4 identifies each operation; only selected pending is added, without source
comments or ACK/lifecycle/watermark changes. Reservation is not provider absence,
ownership, API idempotence, or dispatch authorization.

Mark-uncertain generation has only binding/ledger/selector/operation-ID inputs,
no source/MW arguments. It requires the persisted exact Prepared ID, changes only
phase, retains frozen sourceCommit/parser metadata and phase-independent binary
intent digest, and refuses new generation from Uncertain. Exact apply retry is
idempotent; no cancel/clear/reset/reprepare/delete/retire layer exists. For generic
verify/apply CLI compatibility, SourceArgs is still required: `--commit` must match
frozen metadata; repo/MW paths are not read on phase-only recovery. No current
source eligibility is claimed. Both phases block new prepare.

Simulation's once-captured regular no-follow absolute observation is at most 1 MiB,
with exact approved 64-lowercase-hex SHA and strict duplicate/unknown/trailing JSON
rejection. V1 observation provenance is only `operator_asserted`, with frozen
binding/IDs/target/intent and exact managed payload, recomputed hash, and valid
asserted result etag. Fake `verified_http` or an input `remoteVerified` refuses;
there is no trust override. V2 `calendar-provider-simulation` embeds typed
observation/hash and hypothetical ledger, `simulationOnly: true`,
`persistenceAllowed: false`, and false remote/publication/head flags. Its
hypothetical Active/provider ACK/pending None result preserves source ACK/watermark
even after current notes disappear, but **cannot be persisted through ordinary
verify/apply**. At this offline checkpoint no authenticated-success constructor or
transport existed; the private adapter below does not make simulation persistable.

Strict V1 ledger and enroll/reack candidate formats are unchanged. New strict V2
`calendar-operation-candidate` prepare/mark_uncertain uses full replay and operation
proof. V2 ownership records bind operation ID/digest/committed pending phase/event/
payload; exact V1 marker/encoding remains unchanged. Current writer reads both;
old state writers refuse V2-owned tips, without claiming all read-only ledger
readers refuse the unchanged V1 ledger. New-operation local apply receipts are V2
with operation/intent fields; V1 receipt fields remain unchanged. Same dedicated
bare fixed-ref CAS, exact retry, exclusive administration, and cancellation guard.
Persist prepared intent before future dispatch; Prepared is not safe-to-retry.

All tests used temporary fixtures only, with actual state-writer Git writes confined
to temporary repositories. No Google API, Keychain, live user-state, hook, CI, or
account-quota calls; no actual user-repository Git commit/ref/push, secret, billing,
or provider changes. Installed Hive/MW binaries and active worker were unchanged.
No Linux runtime checks, probes, VMs, or target installs: Linux is explicitly
user-deferred until later before deployment, not a local-work blocker.

At this checkpoint future gates included real Calendar transport with write OAuth/native or
hosted secret storage, authenticated bound event observations and success ACK
candidates, durable ACK CAS/reconciliation before provider ACK/pending changes,
exclusive dispatch ownership **beyond ledger Git CAS**, remote CAS push/hosted
workflow, account billing hard stop, and later Linux validation. Remote APIs and
Git are not one transaction; provider-result persistence is not complete. No paid
database is needed for the current protocol. Normal-user UI/policy, automatic
enrollment, and actual integration activation remain pending; conservative
source-basis restrictions are unchanged.

Private output remains no-log/no-auto-upload. This docs-only pass reads source and
diffs and checks documentation whitespace only; no tests/builds/services/network,
provider/credentials/state operations, Git writes, or installed-binary changes.

### Calendar private HTTP adapter checkpoint: 2026-11-02

**Dormant private HTTP adapter code + loopback-test milestone DONE, not an actual
publisher, production authentication proof, or activation.** See
[adapter contract and source links](product-architecture-roadmap.md#calendar-private-http-adapter)
and [current boundary](../README.md#calendar-private-http-adapter-dormant).
Compiled real request builders remain private and unwired: no new public export,
CLI command/config/env credential lookup or production consumer. Private move-only
intent/origin-bound POST/PATCH permits have no production constructor. Neither
Prepared nor local Git CAS grants dispatch authority. The module's narrow
`allow(dead_code)` documents intentional nonactivation, not blanket warning relief.

Actual **macOS validation executed by the main implementation session**, supplied
here and not rerun by this documentation update:

- Serial `cargo test --workspace --locked --offline -- --test-threads=1`:
  **430 normal passed, 13 ignored**. Calendar: **145 normal passed, 3 ignored**.
- All **15 actual reqwest loopback tests** passed in the latest default-parallel
  and serial runs, plus the full suite. Tests exercise actual HTTP over literal
  `127.0.0.1` only; no live Google authentication/provider proof is claimed.
- Coverage includes Userinfo first and principal mismatch with zero Calendar I/O;
  exact encoded URLs/auth/body/If-Match and unrelated private-property preservation;
  wrong IDs/hash/owner, unsupported classes and etags; status mappings, collision,
  conflict and sent-write 500/502/503/504 uncertainty; redirects with no secondary
  contact; chunked/oversized/truncated/trickled response bounds; redacted token/error
  privacy; production HTTPS refusal before loopback contact; bad permit/origin zero I/O.
- Total-budget coverage is phase-aware: a recorded mutation must yield Uncertain
  on timeout; budget exhausted during preflight is Unavailable with GET-only
  evidence. There is no automatic retry or implication that Prepared is safe to retry.
- The earlier P2 sent-5xx mapping defect is closed. A broken builder-test delimiter
  was corrected and timing tests stabilized. Final review found **no P1/P2 for the
  dormant adapter scope**. Strict Calendar all-targets Clippy with `-D warnings`,
  formatting and build passed. The four legacy main-CLI warnings remain unchanged.
- No new gated real-MW fixtures were added. **The six real-MW gate passes above
  belong to the prior 415 checkpoint and were not rerun for this adapter.** Older
  415/389/363/339/303 counts and gate evidence remain separate historical checkpoints.
- Cargo.lock adds declared dependency references only (`hive-core`, `reqwest`,
  `zeroize` for Calendar), with no version upgrades/downgrades.

The adapter uses fixed production HTTPS Userinfo/Calendar endpoints with HTTPS-only,
no proxy/redirect/retry, shared 30-second operation deadline, 10-second connect
timeout, 64 KiB Userinfo and 1 MiB event caps. Same-bearer Userinfo subject namespace
must match the bound account before Calendar I/O; email/token claims/public token
metadata are not authority. Principal binding does not prove scopes/calendar
ownership/dispatch rights. The opaque zeroizing/redacted token has no verified
claims; credential provider/Keychain/refresh/grants are unchanged. Future activation
needs separate Calendar-write + OpenID authorization, not the Tasks-readonly grant.

Bound-event GET validates observed desired state, not dispatch causality. Reserved-ID
POST and prior-ownership/hash/strong-etag GET then managed-only If-Match PATCH are
the only mutations; unrelated private properties are preserved. No unmanaged-event,
DELETE, timed-event or recurrence support. 409 means collision for later explicit
reconcile; 412 means conflict, without retry. Sent-write 5xx/I/O/timeout or malformed/
mismatched 2xx is Uncertain; Userinfo/GET 5xx is Unavailable without a write. Static
429/other mappings do not authorize retry; pending Prepared still requires reconcile.

Sealed validator-only success has private fields/no serde/public constructor and
binds principal/binding, IDs/intent/event/etag/hash, mutation-vs-observation cause,
and immutable origin. Test-only HTTP origin markers cannot become production
authentication proof: loopback origin eligibility is false, with no DurableAuthClaim.
No authenticated ACK consumer/reducer/persistence, V3 receipt, exclusive worker or
remote Git coordination was added. Operator simulation stays nonpersistable;
Ledger V1, Candidate V1/V2 and local receipts are unchanged. Future trusted results
cannot be supplied by JSON or a `remoteVerified` boolean.

All actual HTTP test calls were loopback-only. No live Google, Keychain, credential
state, actual notes/source/ref/state-repository activation, workflow, billing,
secret, installation or capture-worker changes occurred. No Linux probes/VMs/targets:
Linux is explicitly deferred until later before deployment. This docs-only pass
performs source/diff reads and Git diff whitespace checks only, no tests/builds,
network/services, provider/credentials/state operations or Git writes.

Next work remains **pending**: trusted in-process result -> durable ACK validation,
receipts/recovery; exclusive authoritative dispatch ownership; credential/grant
wiring; hosted CI/remote CAS; cost cap; Linux later before deployment and explicit
activation approvals. Remote API and Git persistence are not one transaction. No
public native-publish command or activation is delivered. Private content remains
no-log/no-auto-upload; the user's no-Atlassian-upload boundary is unchanged.

## Build targets

```bash
make validate
make build
make pwa-build
```

`make docker-build` builds the local `hive-sync-api` image when a Docker-compatible
runtime is available:

```bash
make docker-build IMAGE=hive-sync-api:local
```

## First packaged release shape

Use GitHub Releases as the first distribution channel:

1. Run `make validate`.
2. Run `make build`.
3. Package `target/release/hive` with README/license/checksum.
4. Attach platform-specific archives to a GitHub release.

Suggested archive names:

- `hive-<version>-aarch64-apple-darwin.tar.gz`
- `hive-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `hive-<version>-aarch64-unknown-linux-gnu.tar.gz`

## Deferred package-manager paths

- Homebrew tap for macOS once CLI surface stabilizes.
- Arch/CachyOS PKGBUILD once release artifacts are stable.
- Container image publishing once Docker/Cloud Build verification is available.

Do not publish `hive` under the MindWeaver `mw` binary name; Hive remains the
sync/API/PWA companion boundary.
