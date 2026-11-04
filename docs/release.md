# Hive Release and Packaging

## Tasks first-due fake-executor checkpoint: 2026-11-04

**New fake-only executor milestone complete after review; not production or
activation.** Built-debug, not installed `./target/debug/hive tasks fake
init/execute-first-due/recover/inspect` adds dedicated bare local state at fixed
`refs/heads/hive-tasks-fake-state` plus a separate single-target synthetic world.
The checked-in fixture has no real Google values. Shared policy moved from the CLI
to `hive_tasks::policy`; dispatch freshly collects exact committed source, never
executes JSON plans, and does not require that source to be pushed.

Prepared → Attempting is durable exact-ref CAS under separate local locks before
private move-only dispatch. Revision-conditional fake PATCH changes due only;
world mutation and outcome persistence are separate. Frozen recovery has no
source/parser dependency and never PATCHes: Prepared becomes NotDispatched;
attempted/uncertain matching desired state becomes RecoveredSatisfied with UNKNOWN
causality; unchanged/missing stays Uncertain; conflict becomes blocking
ConflictNeedsReview. Repeated unchanged recovery adds no history commit, including
at the history bound; legacy consecutive Uncertain histories can still terminate.
No reset/retry-dispatch/resolve escape or later operation after satisfaction.

All reports retain `executionMode: "fake-only"`, `provider: "durable-fake"`, and
false `googleConditionalWriteValidated`, `productionReady`, `publicationReady`,
`pushVerified`, `remoteVerified`, `hiveWriteProven`. Fake Applied is not a Google
ACK and cannot be promoted into production evidence. Sanitized bounded Git plumbing
uses old-ref CAS and explicit object/ref fsync; guarantees remain **process-crash
recovery only**, not host/power loss or distributed fencing. Path-bound local state
is not portable hosted coordination. Separate world locking explicitly unlocks
shared/inherited descriptors.

### Supplied final targeted validation

Actually run during implementation/review, **not run by this docs update**:

- Coordinator independently ran final default-parallel hive-tasks: **21 passed**;
  reviewer independently confirmed **21 passed**. Implementer also ran three
  successive default-parallel passes and one serial pass, each **21 passed**.
  These repeat the same suite, not additional distinct tests.
- Final CLI Tasks integration: **22 passed / 1 ignored**; Tasks unit: **5 passed**.
  The real-MW integration remained ignored for this milestone, not rerun.
- Earlier implementation regression checks: capture **68 passed / 2 ignored**,
  auth **68 passed**, capture CLI **7 passed / 1 ignored**, auth CLI **6 passed**.
  The coordinator did not rerun these for the final checkpoint.
- Strict new-crate Clippy passed; CLI Clippy passed with existing
  `too_many_arguments` / `needless_lifetimes` allowances. Formatting and diff checks
  passed. No fully strict workspace Clippy claim.
- Initial parallel new-crate failures were reproduced and fixed; lock coverage now
  includes explicit release with shared/inherited descriptors. Final recovery
  coverage also verifies idempotent bounded Uncertain history. The repeated
  parallel passes above are the post-fix evidence,
  not a claim of final flakiness or that initial failures never happened.
- **Full workspace NOT rerun.** Prior unresolved Calendar loopback evidence and the
  Nov 3 intermediate serial pass remain historical below, not latest green proof.

No live auth/observe/Google requests, native credential operations, installed
binaries, workflows, capture queue, notes, real user refs/state, commit/push or
live activation were touched. Tests created actual Git refs/commits **only in
temporary synthetic repositories**; this is not a claim of no Git writes anywhere.
This documentation pass runs no tests, builds, shell commands or live operations.

**NEXT gate:** separately authorize a disposable-resource live Google Tasks test
proving stale conditional PATCH preconditions are rejected, not ignored. Fake
revisions and Calendar etags do not prove Tasks behavior. Then deliver a scoped
production adapter, durable intent/ACK coordination, hosted secrets/state and the
notes-`main` push workflow, with existing cost/Linux/activation gates. No new
workflow or live-write CLI now. Auth docs and prior Nov 3/Nov 4 checkpoints are
unchanged. See [usage, fixture, state machine and limits](tasks-first-due-fake-executor.md)
and [staged writeback gates](google-tasks-writeback.md#staged-activation-plan).

## Google Tasks auth and exact-read checkpoint: 2026-11-04

**Separate local TasksWrite auth + handoff-bound exact Google Task READ complete;
NOT task mutation or activation.** Built-debug, not installed,
`./target/debug/hive tasks auth login/status/logout` selects exactly
`https://www.googleapis.com/auth/tasks` plus `openid`, with native local Keychain
service `hive/google-tasks-write/v1` and `google-tasks-write-auth` lock namespace.
Readonly `personal` and Calendar profiles remain untouched. Local storage/lock
isolation is not Google-side grant isolation; the broad scope is not single-task
restricted. Hosted Linux secure storage/rotation remain future work.

`tasks observe --connection tasks-write-personal --handoff /ABS/private/reviewed-handoff.json`
preflights input before native setup, refreshes one saved grant with fresh UserInfo,
matches the handoff account namespace and uses the same bearer for exact GET. No
environment bearer/provider/list/task overrides, redirects/retries/list fallback,
queue/note/Git changes or task mutations. Resource/output limits are 64 KiB (encoded
output strictly smaller). Its unsigned `google-task-live-observation` carries
handoff digest, time, exact tuple and nested observation. Collection provenance
`remoteReadVerifiedDuringCollection: true` is not portable authentication;
`portableEvidenceAuthenticated`, `publicationReady`, `pushVerified`,
`conditionalWriteSafe` and `hiveWriteProven` stay false. Offline due-plan rejects
the wrapper/provenance fields; explicit nested extraction stays operator asserted,
`remoteVerified: false`. Files/digests/time grant neither authenticity nor dispatch.

### Independently run implementation validation

Supplied results, actually executed independently during implementation; **not
rerun by this docs-only update**:

```text
cargo test --locked --offline -p hive-google-auth -p hive-capture -p hive -- --test-threads=1
```

- Completed with all invoked suites passing: CLI unit **58 passed**; Tasks
  integration **20 passed / 1 ignored**; capture library **68 passed / 2 ignored**;
  auth library **68 passed**. Existing CLI Calendar/capture integration suites also
  passed. Total across main harnesses: **242 passed / 9 ignored**; subprocess probes
  are extra and are not double-counted. This is not a full-workspace run or a rerun
  of ignored real-MW gates.
- Coverage includes exact profile namespaces/scopes, fake auth lifecycle and
  loopback tests, plus **8 new exact-GET tests**. Reviewer found no P1/P2 after
  fixing Tokio dispatch to `spawn_blocking`: blocking clients are constructed,
  used and dropped off runtime threads. The shared async regression uses real
  blocking HTTP client constructors but fake credential/provider dependencies,
  with **zero network requests** in that regression.
- New tests use no live Keychain, browser, Google provider or user state. HTTP
  transport testing is loopback-only, not live Google behavior or consent proof.
- Formatting and diff checks passed. Focused strict auth/capture Clippy passed.
  Hive CLI strict checking still requires the existing unrelated
  `too_many_arguments` / `needless_lifetimes` allowances; no fully strict workspace
  Clippy claim.
- **Full workspace was NOT rerun this milestone.** Prior Calendar HTTP-loopback
  failures and their unconfirmed cause remain unresolved evidence at the checkpoint
  below; the earlier intermediate serial pass is not latest full-workspace proof.

Login and observe examples are activation-sensitive, **not executed or instructions
to run now**. Status/logout access local secure storage but do not call Google;
logout retains account binding, does not revoke the Google grant and cannot retract
an in-flight bearer. No new consent/Google operations, PATCH/ACK/hosted workflows,
commit/push/install or real-state changes occurred. The eventual source-push-to-private-
NOTES-`main` target is unchanged; Calendar remains deferred. Prior checkpoints below
are preserved as historical records, not rewritten to this milestone's status.

See [contracts, CLI examples and remaining gates](google-tasks-writeback.md),
[auth profiles](google-auth.md#separate-tasks-write-authorization-local-only),
[CLI/dispatch](../crates/hive/src/tasks.rs),
[observation collector](../crates/hive/src/tasks_observe.rs),
[exact GET](../crates/hive-capture/src/google.rs), and
[auth profile definitions](../crates/hive-google-auth/src/lib.rs).

## Google Tasks local first-due checkpoint: 2026-11-03

**Local code/review milestone done, not outbound activation.** Built-debug, not
installed, `./target/debug/hive tasks handoff` exports one selected delivered
unchanged capture with verified queue binding, receipt provenance/digest and
explicitly accepted current undated baseline. `tasks due-plan` reads a full pinned
commit through the shared committed-task facade plus explicit handoff and
operator-asserted observation. Both emit JSON stdout, not output files. Advice is
`set_first_due`, `already_satisfied`, or `blocked`; publication/remote/push
verification, conditional-write safety and Hive-write proof always remain false.

The accepted eventual target is a push to private NOTES `main` followed by hosted
execution against that exact pushed commit, updating the SAME Google Task's first
due only—not creating a Calendar event. Calendar work/evidence below is historical
and deferred, not removed. See [interfaces and remaining gates](google-tasks-writeback.md).

**Validation and remaining test issue:**

- Latest Tasks integration: **17 passed, 1 ignored**; the ignored real-MW gate was
  explicitly run separately and **1 passed**, using sibling `target/release/mw`
  with an isolated TEMP committed repository. Both checks were independently
  rerun after the final fixes. The real-MW gate produced due-only
  `2026-11-10T00:00:00.000Z` advice with source/index unchanged, including coexistence
  with unrelated inactive/archive history and rejection of duplicate identity.
- An earlier intermediate full **serial** workspace run passed **486 / 14 ignored**.
  **Latest full-workspace validation is not green:** later serial attempts each
  reached three Calendar HTTP-loopback failures before Cargo stopped, and a
  transport-only rerun also failed. Failures included request-count mismatches and
  `Unavailable` versus `Uncertain`/`Invalid` outcomes. Calendar transport, process,
  and original source-reader files are unchanged; root cause remains unconfirmed,
  with no pristine-baseline comparison. Do not treat the earlier pass as final
  full-workspace proof. Other crates were checked separately by the implementer.
- Strict capture/Calendar library Clippy passed. CLI Clippy passed only with
  allowances for pre-existing `too_many_arguments` / `needless_lifetimes`; no
  globally strict clean claim. Formatting and diff checks passed.

No installed binaries, user notes/queues, credentials/providers, `.github`
workflows, real user refs/commits/pushes or activation changed. The inbound worker
and readonly grant remain live and untouched. Remaining gates include separate
Tasks-write auth and hosted secret/rotation design, exact-task authenticated GET
and conditional PATCH verification, durable intent/uncertain-outcome ACK/recovery,
serialized immutable-SHA/freshness workflow, included-allowance hard stop, and Linux
validation before deployment (deferred now). This documentation pass runs no tests.

Implementation: [CLI/planner](../crates/hive/src/tasks.rs),
[handoff](../crates/hive-capture/src/handoff.rs),
[shared task facade](../crates/hive-calendar/src/task_snapshot.rs), and
[integration tests](../crates/hive/tests/tasks.rs).

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

Historical adapter-only checkpoint, retained separately from private ACK below.
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

At this historical checkpoint next work was trusted in-process result -> durable ACK validation,
receipts/recovery; exclusive authoritative dispatch ownership; credential/grant
wiring; hosted CI/remote CAS; cost cap; Linux later before deployment and explicit
activation approvals. Remote API and Git persistence are not one transaction. No
public native-publish command or activation is delivered. Private content remains
no-log/no-auto-upload; the user's no-Atlassian-upload boundary is unchanged.

### Calendar private ACK checkpoint: 2026-11-02

Historical ACK checkpoint, retained separately from the coordinator below.
**PRIVATE in-process ACK -> local CAS code/tests DONE, NOT activation or production
approval.** See the [source-linked ACK contract](product-architecture-roadmap.md#calendar-private-ack-persistence)
and [brief current boundary](../README.md#calendar-private-ack-persistence-dormant).
Older **430/415/389/363/339/303** checkpoints above remain historical.

Actual macOS validation executed by **this main implementation session**, not the
user and not rerun for this documentation pass:

- ACK-checkpoint **FINAL GREEN**: `cargo test --workspace --locked --offline`, DEFAULT
  parallel, **444 normal PASS / 13 ignored**. This is NOT a serial result.
- Calendar: **159 normal passed / 3 ignored**, default parallel. Strict Calendar
  all-targets Clippy with `-D warnings`, formatting and build passed. No claim that
  the four historical main-CLI warnings were fixed.
- All **six explicit real-MW compatibility gates RERAN and passed earlier in this
  session**: CLI apply x2, candidate, plan, preview and source-reader. This updates
  the older adapter checkpoint's not-rerun boundary; it does not rewrite its history.
- New ACK writer tests use **SYNTHETIC trusted handoffs and TEMP repositories only**,
  NOT the real Google authentication pipeline. Actual HTTP loopback admission
  rejection is covered, without relabeling loopback evidence as Google.
- Pure ACK create/update in both Prepared/Uncertain phases, unavailable frozen
  notes/parser recovery, identity/base/phase/source/mapping/evidence tamper, and
  preservation of current old source ACK rather than intendedSource all passed.
- SHA-1/SHA-256 V3 state CAS, exact retry after injected post-CAS unknown outcome,
  refusal after later advancement, V3 Source/Intent continuation floors and reader
  compatibility passed. V1/V2 goldens remain unchanged.
- Focused review found **no P1/P2 for private local ACK under the dedicated bare,
  exclusive-administration assumption**, not production approval.

#### Test stability and failed runs

The initial full serial run failed on the pre-existing capture 100 ms output-cap
race: timeout won instead of the cap. The **test-only output-cap case** now uses
2 seconds; the separate 100 ms timed-out case remains. Production capture Runner
did not change. Do not present that initial serial run as green.

Later full default AND serial runs hit loopback `TransientUnavailable` before
complete Userinfo, with request counts zero. Fake HTTP server handling was corrected
to record only complete headers/body and not consume a reply on an empty request.
Fixtures use 10-second stream timeouts and **one mutex per HTTP test**, not nested
per-server locks. A `cfg(test)` shared `OnceLock` blocking client and pre-listener
warmup remove per-test runtime churn. Production HTTP configuration is unchanged
by this test pool. Error classes, request-count assertions and retries were NOT
relaxed to get a pass.

**Root cause is NOT established.** Diagnostics reproduced accepted=1/complete=0;
the connection ended during cleanup, not server lifetime expiry. The slow-client-
build theory did not reproduce. These observations are not a proven causal fix.
After pooling, the bounded-response test passed **10 consecutive times**; all
**16 HTTP tests including ACK passed twice serially**, plus a parallel module pass;
then the full Calendar crate passed 159 normal in parallel and the final MAIN
workspace run passed 444 normal in default parallel. **No full serial 444 pass is
claimed: earlier full serial runs FAILED.**

#### Delivered Boundary

Pre-HTTP `PinnedContext` retains exact committed base/path/blob, old ledger and full
pending including phase; it attaches `contextDigest` to intent and HTTP-sealed
result. Admission matches that basis, namespace/IDs/intent/event/hash/strong ETag
and immutable Google origin. Actual loopback origin is always rejected for durable
ACK. `LedgerChangeProof::Source | Intent | Acknowledgement(SealedAckProof)` separates
authority, with a private ACK constructor. Audits/JSON cannot become authority.
Ordinary public apply still accepts only V1 source/V2 intent candidates: no new CLI
ACK/publish command, raw authenticated JSON or public API.

Frozen create/update ACK supports both phases without source/parser/current-task
eligibility. Only selected Active/provider ACK/result/pending None changes;
CURRENT old source ACK, watermark/order/tombstones/unselected evidence stay intact.
GET cause `ReconciledObservation` is not worker-dispatch proof. Borrowed
`PinnedAckTransaction` retains original base/result through errors, reconstructing
the same commit from OLD base even if latest pending cleared. Exact retry succeeds;
later advancement refuses, never rebases. Fixed-ref bare/no-worktree/exclusive-admin,
CAS and cancellation policies remain unchanged.

V3 terminal ownership/apply receipts validate OLD parent ledger/full pending phase,
result and exact new reduction. Following Source/Intent writes remain V3, preserving
the minimum-reader floor. Audit/provenance floors distinguish GoogleAuthenticated
from TEST-ONLY SyntheticFixture. Test factories force Synthetic and recompute the
digest; all synthetic continuations retain that floor, even after pending clears.
Production excludes the variant and rejects fixture branches. Audit digests and
Git receipts are NOT cryptographic signatures. Generic publication/remote/ref
verification stays false: observation cause/provenance is not current remote sync.

Opaque child `RemoteDispatchAuthority` + `FreshAuthoritativeView` have no production
constructors/Serde/Clone; mint consumes matching head/path/blob/context/owner nonce/
intent/origin/Prepared evidence. Google permits require authority basis; test
factories are loopback-only. This is NOT an actual remote lease/fencing protocol:
local CAS/nonces/freshness/single-use values do not stop paused old cross-host
workers, and no expiry takeover exists. Safe remote ownership is UNIMPLEMENTED;
production minting remains unavailable.

Remaining gates: remote Git authoritative ownership/fencing, real provider grants/
credential wiring, private coordinator/CLI integration, hosted push trigger plus
remote CAS, cost hard stop, normal-user UI/activation approval and Linux later before
deployment. No automatic Calendar writes now. No new API credentials, private queue,
worker/binary/settings changes. No actual notes/source/refs, Google/Keychain,
services/deployment, Git commit/push, secret/billing or Atlassian changes occurred;
implementation writes were TEMP-only. Linux is explicitly deferred: no probes,
VMs or targets. This docs pass only inspects source/diffs and checks whitespace,
without tests/builds/network/services or state operations.

### Calendar private coordinator checkpoint: 2026-11-02

**PRIVATE CREATOR-ONLY COORDINATOR SEAM code/tests DONE, not hosted execution,
production dispatch or a production-ready publisher.** See the
[source-linked protocol and integration gap](product-architecture-roadmap.md#calendar-private-creator-only-coordinator)
and [brief boundary](../README.md#calendar-private-coordinator-dormant).
The sealed AuthoritativeState backend has no production implementation/constructor;
the receipt-consuming dispatch bridge cannot supply production authority. No new
public export/CLI, credential provider/grant, workflow or full executor is wired.

Latest **macOS validation executed in the implementation session**, not rerun
in this docs-only pass:

- `cargo test --workspace --locked --offline -- --test-threads=4`:
  **464 normal PASS / 13 ignored**. This is a four-thread run, not default or serial.
- Calendar: **179 normal PASS / 3 ignored**. New coordinator: **20 tests passed
  serially**, including the final post-mint root cancellation/expiry hardening test.
- Earlier transport-target subset: **43 tests passed serially with 19 coordinator
  tests**. All current 44 transport tests were included in the full workspace run;
  no separate 44-test serial target run was performed. Serial coordinator coverage
  is 20.
- Strict Calendar all-targets Clippy with `-D warnings`, formatting and build passed.
  Main CLI `clippy --no-deps` passed with four existing legacy warnings. No strict
  main-CLI warning-free result is claimed.
- The **six real-MW gate passes are historical 444-checkpoint evidence**, NOT rerun
  for this coordinator slice. No new MW gate was added. Historical 444 default-
  parallel, 430/415/389 serial results and their failed-run/stability limits above
  remain intact; none is reattributed to this slice or the user.
- Focused review readiness is for the **private protocol**, not a publisher.
  Tests cover root cancel AND expired Ready discard after CPU mint, receipt
  consumption and next-invocation recovery-only/no new publication.

Delivered protocol proof uses only `cfg(test)` in-memory and owned TEMP bare
authoritative adapters plus a separate worker-local bare repository. Bounded local
Git packs transfer objects; actual atomic `git update-ref NEW EXPECTED_OLD` performs
the authoritative compare, not an ordinary push. The fixture checks actual H1
commit/blob and sole parent H0, plus exact single prepare with no old pending,
valid frozen sourceAck/payload and unchanged source/provider ACKs, watermark,
tombstones, order and unrelated evidence. Source eligibility remains delegated to
the preparer, not fresh-note validation by the coordinator.

Only fresh successful exact H0 -> H1 CAS in the SAME invocation mints an opaque
move-only FreshCas receipt. Consume it once with invocation nonce and exact latest
H1 head/path/blob/binding/authorityId/origin to yield one intent-bound permit. Any
existing Prepared OR Uncertain pending anywhere in the binding returns RecoverOnly
before staging/publication. Own old head, same-job rerun, copied nonce, local
ApplyReceipt, `already_applied`, logs and matching H1 readback cannot acquire.
Two creators at H0 have at most one CAS winner/permit. Lost CAS reply, even with
own-H1 readback, is recovery-only. Missing/stale/advanced/non-ABA rollback heads
fail without rewind; basis/nonce/origin mismatches refuse. A paused creator may
make its late first attempt, but another invocation never writes. GET 404/410
recovery records two GETs (Userinfo/event), zero mutations, and no new acquisition.
No expiry/steal/reset/reprepare-pending/blind-retry route exists.

Root checks after CAS/latest view AND after CPU authority mint discard Ready if
cancelled/expired as PublicationUnknown; the consumed receipt cannot retry, and
the next invocation sees pending RecoverOnly. Caller MUST check root before HTTP;
this is not atomic revocation. `InvocationOutcome::Ready` returns an **H1 policy
pin, not H0 and not a claimed local repository** (`PathBuf::default()`). The future
worker MUST materialize/verify H1 and load actual local PinnedContext with a real
repository BEFORE HTTP/durable ACK. This slice is NOT the full HTTP + local ACK +
remote publication executor; no production-ready integration claim is made.

ACK publication is a separate policy stage with NO production AckPublication
builder. Completed requires confirmed exact authoritative ACK CAS, not local ACK
objects, unknown or conflict. Root cancellation after publication is unknown.
Synthetic V3 ACK local evidence stays Synthetic, never Google; actual Loopback
results still fail private ACK admission. Lost ACK reply may mean H2 committed and
pending cleared: unknown does NOT guarantee unchanged pending. No repeated provider
write is authorized; actual remote ACK readback/recovery is not implemented.

Local CAS/nonce/latest view is NOT remote lease/fencing/exactly-once. Git ABA/admin
rewind is out of scope, requiring cooperative writers/exclusive admin and later
remote controls. A separately reviewed actual fixed-identity remote CAS backend
must prove exact expected OLD in a server-atomic transaction; unconditional REST
ref update, ordinary FF push or readback cannot substitute. Remaining gates also
include full worker/materialized pin/credential grant, authoritative ACK recovery,
queue/dispatch policy operations, source-policy UI, hosted workflow, pinned Linux
later, account included usage/paid-overage hard stop and explicit activation approval.

All implementation HTTP was loopback-only; all coordinator Git writes were TEMP
bare fixtures. No actual GitHub/Google/credentials/user state, notes/source/refs,
installation, capture worker, services, CI/workflow, pushes, secrets or billing
changed; no paid service provisioned. Linux is explicitly **DEFERRED**, with no
probes, VMs or target installs. This docs pass performs only source/diff inspection
and `git diff --check`, no tests/builds/network/services or state operations.
Private output remains no-log/no-upload: no Atlassian uploads or upload offers.

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
