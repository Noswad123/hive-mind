# Hive Mind

Hive Mind is the sync/API/mobile companion project for MindWeaver.

- Repository/folder: `hive-mind`
- CLI/binary: `hive`
- MindWeaver remains Markdown-first; Hive handles optional remote sync, device
  auth, the sync API, and the PWA companion.

## Current priority — Google Tasks due writeback (2026-11-04)

**Accepted target: edit canonical Markdown → commit AND PUSH to private NOTES
`main` → GitHub-hosted workflow reads the exact pushed commit → update the SAME
existing Google Task, due only. Do not create a Calendar event.** Calendar is now
deferred; the Calendar sections below preserve historical implementation/evidence,
not the current delivery target, and do not imply code was removed.

The credential/provider-free local milestone is implemented in the **built-debug,
not installed** `./target/debug/hive tasks handoff` and `tasks due-plan`: a portable,
receipt-backed selected capture identity handoff and exact committed-source
first-due planner. Desktop queue mappings are unavailable on hosted runners; export
only the selected binding, never sync SQLite or infer identity from a title or
reverse the stable marker. Explicitly accepted current baseline evidence must not
be called the original capture baseline. Local plans remain advisory, with
`publicationReady`, `remoteVerified`, `pushVerified`, `conditionalWriteSafe`, and
`hiveWriteProven` false. Both commands emit JSON stdout and write no output files.

**New local milestone complete: separate TasksWrite auth and handoff-bound exact
Google Task READ**, in built-debug, not installed, `tasks auth login/status/logout`
and `tasks observe`. Observe refreshes one saved grant, verifies fresh UserInfo and
the handoff account, then uses the same bearer for the exact task GET. Its unsigned
`google-task-live-observation` wrapper records collection provenance, not portable
authentication or dispatch authority. Offline due-plan rejects the wrapper;
explicit extraction of its nested observation remains operator asserted with
`remoteVerified: false`. No task mutation, durable ACK, or hosted executor is added.

**Fake-only executor milestone complete after review (2026-11-04):** built-debug,
not installed, `tasks fake init/execute-first-due/recover/inspect` now exercise
durable local intent, exact committed-source shared policy, conditional synthetic
due-only mutation and frozen observation-only recovery. No JSON plan is executed.
See the [dedicated usage/state-machine contract](docs/tasks-first-due-fake-executor.md).
Fake success is not a Google ACK; all production/publication/push/remote/Hive-write
proof flags remain false. A separate **nondefault `tasks-conditional-probe` feature**
now provides reviewed disposable-task `inspect/run` diagnostics, not a production
writer. Default builds exclude its adapter and command. See the
[probe runbook and separate read/two-mutation approval gate](docs/tasks-conditional-probe.md).
**Limited live conditional proof succeeded Nov 4 after explicit authorization:**
a separate `tasks-write-personal` grant was saved; the user-nominated disposable
task accepted a current-ETag due-only change from Nov 5 to Nov 6, rejected the stale
old-ETag attempt back with valid Google JSON HTTP 412, and retained Nov 6/new ETag
on final and independent read-only verification. Two PATCHes attempted, one accepted;
no replay/rollback/delete. This one-target/session evidence is not a production ACK
or universal provider guarantee. The opt-in feature is not installed.

**New local publisher: implemented and reviewed, with validation limits (Nov 4).**
Default-enabled, built-debug, not installed `tasks publish init/inspect/prepare/apply/recover`
now provides a real single-target first-due adapter, separate bare Git state,
reviewed Prepared/Attempting CAS, one strong-If-Match due-only PATCH and frozen
GET-only recovery. This is not hosted execution or live activation. No local
publisher installation/handoff export, canonical-note edit or live publisher/native/
provider operation occurred this step. Prior saved TasksWrite credentials persist;
disposable-probe approval does not authorize a canonical real-task apply. See the
[local runbook and side effects](docs/tasks-local-publisher.md) and
[validation checkpoint](docs/release.md#tasks-local-publisher-checkpoint-2026-11-04).
The latest independent default-parallel task-crate run passed all 45 tests;
earlier failures/timeouts remain unexplained, so stability is not established.

**Hosted read-only foundation now implemented/reviewed:** default `tasks hosted
check/plan` validates pinned binding, consistent push claims, exact committed source
and private parser copy, then an immutable injected TasksWrite record. Check has no
HTTP/native calls; plan refreshes, verifies fresh UserInfo and performs only an exact
task GET. Claims are spoofable, not GitHub authentication/current remote-main proof.
Rotation requiring persistence pauses before UserInfo/GET; no secret-update API or
hosted apply exists. See the [foundation contract](docs/tasks-hosted-foundation.md)
and [current checkpoint](docs/release.md#tasks-hosted-read-only-foundation-checkpoint-2026-11-04).
**Current remote-authority update (Nov 4):** reviewed with no remaining P1/P2 in
scope. The ONLY new production interface is read-only `tasks hosted verify-main`:
same binding/source/private parser preflight before late `GITHUB_TOKEN` lookup,
then fixed GitHub private identity and exact current-main GETs. It never reads the
Google record. Current-main flags describe fresh production-origin collection,
not push authentication, persisted authority or dispatch. Check/plan remain
claims-only. The remote Git expected-OLD CAS and integrated coordinator are
**entirely `cfg(test)` laboratories**, with real receive-pack in temporary local
bare repositories and synthetic HTTP, not a normal-compiled hosted writer.
See the [protocol, limits and remaining code gates](docs/tasks-remote-coordination.md)
and [new validation checkpoint](docs/release.md#tasks-remote-authority-checkpoint-2026-11-04).
Next: production GitHub HTTPS write-credential delivery; reviewed inception/root pin
and quiescent local-to-hosted handoff; production coordinator/writer wiring with
auth pause/recovery; then pinned workflow/secrets, cost hard stop, Linux validation
and explicit activation. No live GitHub token read/call, hosted writer command or
workflow deployment this step. No commands in these runbooks are to be run now.

**Inbound capture remains live and Google-read-only; production/hosted writeback
is not activated.** Readonly `personal` list access succeeded after the separate
write-profile login and again after testing. Calendar remains unactivated. The
probe did not edit the real phone capture or canonical notes, worker, queue,
baseline, source refs or workflows; credentials intentionally gained the new profile.
No code commit/push/install was performed. Login/observe examples are
activation-sensitive, not instructions to run now; local status/logout still access
the secure store. A saved dirty note
is not visible at a pinned commit; even a local commit does not prove a push.
The pilot excludes create/delete, reschedule, due clearing, and completion.
See [accepted design and staged activation gates](docs/google-tasks-writeback.md)
for implemented CLI examples, separate local Tasks-write auth, and remaining
hosted execution/recovery/cost/Linux gates. See the
[live evidence and post-fix targeted validation](docs/release.md#live-tasks-conditional-proof-2026-11-04);
no new tests ran for this documentation update.

## What is in this repo

```text
hive-mind/
  crates/hive          CLI entrypoint: `hive`
  crates/hive-capture  desktop-local Google Tasks inbound queue and mw delivery
  crates/hive-tasks    shared first-due policy, fake executor and local real publisher
  crates/hive-calendar local Calendar planning/persistence and a dormant private HTTP adapter
  crates/hive-background bounded worker, editor coordination, and owned macOS LaunchAgent
  crates/hive-google-auth desktop Google OAuth, refresh, and native macOS Keychain storage
  crates/hive-core     shared protocol/client/diagnostic types
  crates/hive-desktop  desktop SQLite bridge for MindWeaver sync tables
  crates/hive-server   Rust sync API server used by `hive serve`
  hive-pwa/            Svelte/Vite browser/mobile companion
  scripts/cloud/       Cloud Run build/deploy/monitoring/backup helpers
  docs/                architecture, protocol, runbooks, deployment, release docs
  legacy/go-reference  copied Go implementation used as a behavior oracle
```

## Build and install

For the CachyOS development continuation, start with the
[Linux setup and credential-free checks](docs/development-linux.md), not installation.
The [development handoff](docs/development-handoff.md) records current evidence,
remaining production code gates, and the boundary between retiring the Mac checkout
and preserving its existing capture worker. Linux runtime validation is still pending.

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

## Google Tasks inbound capture (first personal slice)

The desktop-local `hive captures` lane and bounded background worker are implemented
and validated: on manual invocation or an explicitly activated worker cycle, read a
complete Google Tasks list → private dedicated SQLite queue → existing `mw todos add`
with stable capture ID → durable canonical receipt. Google retains phone captures
while the desktop is off; Hive does not acknowledge them until it runs.

See [Google Tasks capture](docs/google-tasks-capture.md) for commands, bootstrap
choices, unknown-outcome recovery, validation evidence, and activation gates.
Desktop OAuth login, account-bound refresh, and native macOS Keychain storage are
implemented; see [Google sign-in setup](docs/google-auth.md). Manual live capture
from the dedicated Android Google Tasks list into Markdown is verified, including
a repeat run with no duplicate or source rewrite. The stable release was built and
installed at `/Users/jdawson/.local/bin/hive` on 2026-10-31 and rebuilt/installed
there after the 2026-11-01 inventory fixes. On 2026-11-01 the user reconfirmed
latest-binary foreground `auth status` and `lists --connection personal` success.
The existing personal queue/list/baseline is configured with fixed public routing,
MindWeaver cwd, interval 60 seconds, timeout 45 seconds, and limit 1; configuration
and queue scope checks passed. The clean/idle live editor was hotloaded in place,
preserving `mw_ops` module/active-guard identity, and the bridge registered.
**Scheduled phone-to-editor-owned writer delivery and repeat nonduplication are
live-verified (2026-11-01).** At the earlier polling checkpoint, two controlled `run-once` cycles
completed in about 0.5 seconds each, followed by explicit background install/start.
The owned LaunchAgent reports `serviceActive: true`, `serviceState: loaded_owned`;
its startup and next scheduled cycle completed about 60.7 seconds apart. Each of
these four cycles observed one unchanged task, staged/delivered zero, and required
no auth action (controlled cycles: generation 1). Latest-build no-UI Keychain access
is positively proven while unlocked, not for denial/locked scenarios. Earlier
45-second unknown outcome and 12-second local status stall did not reproduce;
their root cause is not established and no speculative source fix was made.
Editor readiness is `ready: true`, `noEditor: false`; a live unsaved hidden-buffer
probe verified `dirty_buffers` readiness deferral, then clean readiness was restored
without note edits or guard changes. Those earlier polling checks left the original
receipt at attempt 1 without Inbox/dashboard changes.
The user then added the undated, title-only phone task `Verify scheduled phone capture`.
An automatic scheduled cycle (not `run-once`) observed 2, staged 1, unchanged 1,
delivered 1, pending 0, and completed through the native Neovim bridge with positive
writer-exit proof and zero conflicts. The durable receipt records one attempt;
actual Markdown and a whole-notes-root search confirm exactly one unchecked source
entry with its stable marker. The next actual scheduled cycle observed 2 unchanged,
staged/delivered 0, and retained that receipt at attempt 1 without Inbox/dashboard
byte, size, or mtime changes. The original capture also retains one attempt.
Full pending-job dirty-buffer delivery deferral and live computer-off catch-up
remain unverified. Polling runs in the GUI user session; Google holds work while the
computer is off, with no server required. See the runbook for stable-path
`background status`, `stop`, `start`, and `uninstall` controls; stop retains config
and queue, and uninstall removes only the owned plist while retaining all state.
At the inbound inventory-fix checkpoint, 270 normal workspace tests passed (five ignored), strict
background/auth/capture Clippy passed, and both ignored real-Neovim tests passed
after the fixes. Real-writer integrations passed earlier, not rerun after the
fixes; earlier format and Neovim background/safety checks (32/54 tests) also passed.
No additional tests ran during activation; these isolated checks are separate from
the live scheduled-polling proof.
`lists`/`poll`/`run` now require exactly one explicit `--connection NAME` or
`--access-token-env HIVE_GOOGLE_ACCESS_TOKEN`; saved connections forbid `--account-key`.
Calendar publication, export, completion writeback, lifelog, and PWA migration remain pending.
This lane does not use or change legacy LWW sync/PWA transport and
does not require a hosted server or Postgres before first personal use.

## Calendar source preview (local only)

> **Superseded target / deferred as of 2026-11-03.** All Calendar sections through
> the private coordinator below retain historical capabilities and evidence only.
> Their remaining gates are not the current next slice; use the
> [Google Tasks writeback plan](docs/google-tasks-writeback.md) instead. Calendar
> publication and Calendar authorization remain unactivated.

`hive-calendar` and the built-debug CLI implement a credential-free, source-only
preview. The separate ledger-relative planner below is also implemented; neither
command is a hosted publisher. From this repository:

```sh
# Resolve HEAD explicitly, then pass the returned full lowercase SHA as --commit.
/usr/bin/git -C /ABS/local/notes rev-parse --verify 'HEAD^{commit}'
./target/debug/hive calendar preview \
  --repo /ABS/local/notes \
  --commit FULL_LOWER_SHA \
  --mw-bin /ABS/trusted/mw
```

Requires Git 2.50+ at `/usr/bin/git` and a trusted standalone `mw` supporting
`todos snapshot --json`. The exact commit excludes uncommitted/untracked notes;
`HEAD`, refs, and abbreviated SHAs are not accepted by `--commit`. A local commit
is not proof it was pushed. Bounded regular Markdown blobs are materialized in a
private workspace; an exact hashed private copy of `mw` reads them with cleared
environment and private home/config, without a database or original-note mutations.
This is input isolation, not an OS sandbox for hostile executables.

Open tasks with valid literal due metadata preview all-day Available/transparent
reminders, ending on the exclusive next calendar date. Completed/undated tasks are
skipped; invalid/duplicate due values produce diagnostics and no event. No start
date or estimate supplies scheduling. `publicationReady` is always false, including
for stable IDs; unmarked identities stay visibly unresolved, without new HTML
comments. There are no Google event IDs, create/update/noop decisions, or provider
state claims. **Stdout contains private titles and paths; do not paste it into
public CI logs.**

Local macOS fixture validation passed; Linux code exists but runtime validation is
pending. No real-notes preview was invoked in this session. The installed release
and active background worker are untouched. See [release evidence and limits](docs/release.md#calendar-preview-checkpoint-2026-11-01)
and the [remaining publication gates](docs/product-architecture-roadmap.md#accepted-next-slice-hosted-calendar-publication-after-push).

## Calendar publication plan (local only)

The **built-debug** `./target/debug/hive calendar plan` compares committed source
with explicit committed publication evidence, without credentials, network, or
writes. For observation-only bootstrap, from this repository:

```sh
./target/debug/hive calendar plan \
  --repo /ABS/local/notes \
  --commit FULL_LOWER_SHA \
  --mw-bin /ABS/trusted/mw \
  --source-repo-id OPAQUE_REPO_ID \
  --account-id google-sub-v1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --calendar-id CONCRETE_CALENDAR_ID \
  --bootstrap-preview
```

Supply your independent bindings: the account namespace requires 64 lowercase hex
digits after `google-sub-v1:`; the calendar ID must be concrete, never `primary`
or `default`. The opaque source repository ID is portable, not a path or URL.
These are namespace claims, not credential or account-ownership proof, and are
never inferred from the ledger. The account value above is illustrative only.

To consume an existing ledger, replace `--bootstrap-preview` with **all three**:

```sh
  --ledger-repo /ABS/local/publication-state \
  --ledger-commit FULL_LOWER_LEDGER_SHA \
  --ledger-path calendar/ledger.json
```

Both commits must be full lowercase object IDs; the ledger path is an exact
repository-relative JSON blob path, not a working-tree file. Source isolation and
Git/MW requirements are the same as preview. Ledger planning requires complete,
non-shallow source ancestry and never fetches missing history. Missing or bad
existing state is an error, never a fallback to bootstrap. Bootstrap only exposes
observations: it does not create an authoritative ledger, assign UUIDs, or enroll.
The future CI workflow must supply that history (for example, checkout with
`fetch-depth: 0`) and all referenced blobs before invoking the planner; its default
shallow checkout is insufficient.

Output is `kind: "calendar-publication-plan"`, mode `ledger` or `bootstrap-preview`.
Decisions are `noop`, advisory `update`, `review`, `blocked`, or `retired`, never
create/delete. A `noop` only means the desired payload matches the last provider
acknowledgement, not that an event exists or is current remotely. Always:
`publicationReady: false`, `remoteVerified: false`, `ledgerHeadVerified: false`,
`requiresLedgerCompareAndSwap: true`. Offline planning cannot know the latest
ledger head; a future executor must enforce compare-and-swap and remote recovery.

Explicit logical UUID mappings support markerless tasks; source HTML markers are
optional. Candidate generation and reviewed local persistence are implemented below;
normal-user UI and integration activation remain future work. Do not adopt hand-maintained
JSON as the long-term workflow: any edit anywhere in an
acknowledged file, even an unrelated paragraph, or a move requires review and a
new explicit source acknowledgement under the same logical ID. A different MW
build/platform also requires parser-provenance review/re-acknowledgement; its exact
binary digest is not semantic cross-platform proof. No title/fuzzy/comment-only
auto-matching provides continuity.

See the [ledger schema contract](docs/product-architecture-roadmap.md#calendar-ledger-schema-v1)
and [historical plan checkpoint](docs/release.md#calendar-plan-checkpoint-2026-11-02).
**Output and ledger contain private task content, paths, and IDs, not credentials.
Do not log or auto-upload them as CI artifacts, including to Atlassian. The user
explicitly declined Atlassian uploads.** Linux validation is deferred by user request
until later before deployment; it does not block local work. The installed release,
capture worker, private queue, and auth setup are unchanged.

## Calendar ledger candidates (local only)

The **built-debug, not installed** `./target/debug/hive calendar enroll` and
`calendar reack` generate a whole validated V1 ledger candidate on stdout. They
write no notes, ledger, Git refs, credentials, or provider state. These are command
examples with placeholder bindings, not instructions to activate publication or
maintain JSON manually. Source isolation, Git/MW, full lowercase commit IDs, and
non-shallow source-history requirements above still apply.

Both commands require **all** shared source, binding, ledger, and current-selection
flags shown here, including the ledger base when proposing initialization:

```sh
./target/debug/hive calendar enroll \
  --repo /ABS/local/notes \
  --commit FULL_LOWER_SOURCE_SHA \
  --mw-bin /ABS/trusted/mw \
  --source-repo-id OPAQUE_REPO_ID \
  --account-id google-sub-v1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --calendar-id CONCRETE_CALENDAR_ID \
  --ledger-repo /ABS/local/publication-state \
  --ledger-commit FULL_LOWER_LEDGER_BASE_SHA \
  --ledger-path calendar/ledger.json \
  --source-path projects/example.md \
  --source-line 12 \
  --initialize-ledger
```

`--source-path` selects an exact repository-relative Markdown path;
`--source-line` is its positive one-based checkbox line at the source commit.
`--initialize-ledger` is explicit enroll-only initialization: the ledger path must
be absent **at the supplied base commit**, with any existing parent components
trees. It proves neither authoritative-HEAD absence nor absence of remote events.
For an existing committed ledger, omit `--initialize-ledger`; a missing ledger
then errors and never initializes implicitly.

Enrollment requires an open task with valid due evidence. Hive assigns a new
OS-entropy UUIDv4, not a user-minted ID or HTML marker, and edits no source.
Duplicates and source reservations, including pending and retired evidence, refuse.
The conservative global guard also requires **every old source basis, including
retired entries, to remain recognized**, and no pending operations anywhere. A
missing, moved, or edited retired basis can therefore block an unrelated new
enrollment until a future resolution workflow exists. There is no force override
or promised friction-free restore.

For `reack`, use the same shared flags, replace `enroll` with `reack`, omit
`--initialize-ledger`, and explicitly map the old acknowledgement to the current
`--source-path` / `--source-line` using either:

```sh
  --old-source-path projects/old-example.md --old-source-line 12
```

or advanced `--logical-id EXISTING_LEDGER_UUID`, never both selectors. No fuzzy
identity matching occurs. Reack preserves the same UUID across file moves/due
changes, provider acknowledgement, lifecycle, and unselected pending/tombstone
evidence. The entire old ledger proof is validated **before** replacement; forged
old acknowledgements and source rollback cannot be washed out. A pending or retired
target refuses. `--accept-parser-change` explicitly approves only the selected
entry's parser-digest change; it does not verify old parser semantics. Done or
genuinely undated source can be acknowledged, not propagated as completion/deletion.
Shared due validation still refuses malformed/duplicate due evidence even when done.

JSON uses `kind: "calendar-ledger-candidate"`, `operation: "enroll"` or `"reack"`,
`sourceCommit`, `parserSha256`, `logicalId`, and the whole V1 `candidateLedger`.
`base` contains `commit`, `path`, `gitBlobID` (value or null), and `condition`
(`existing_blob_at_commit` or `path_absent_at_commit`). The CLI buffers the complete
serialized candidate before stdout. Always `publicationReady: false`,
`remoteVerified: false`, `ledgerHeadVerified: false`, and
`requiresLedgerCompareAndSwap: true`.

**Candidate generation itself persists nothing; enrollment IDs can differ on rerun.
Never publish from an unpersisted candidate.** Reviewed local persistence is now
implemented below and retains the exact approved UUID, not a newly generated one.
Normal-user UI and real-repository activation remain pending. Calendar write OAuth,
publisher, hosted workflow, and billing-hard-stop activation remain gated. Linux
runtime validation remains deferred until before hosted deployment, not a blocker
for local development. See the historical
[candidate validation checkpoint](docs/release.md#calendar-candidate-checkpoint-2026-11-02).
Candidate output has the same private-content/no-logging/no-Atlassian-upload boundary
as plans and ledgers above.

## Calendar local persistence

**Local persistence code/test milestone DONE; integration activation not done.**
Built-debug, not installed, `calendar verify-candidate` replays an approved candidate
and computes the expected ledger blob, tree, and commit OIDs without writing Git
objects or refs. `calendar apply` repeats those checks, then explicitly writes local
Git objects and performs one atomic old-OID compare-and-swap (CAS) of
`refs/heads/hive-calendar-state`. Neither command pushes, syncs remotely, calls
Calendar, or changes source notes, grants, or credentials.

V1 requires an absolute **dedicated bare files-backend state repository with no
registered linked worktrees**. A normal notes worktree is refused. The operator
**must exclusively administer the state repository throughout the operation**:
preflight cannot atomically prevent concurrent worktree/config administration.
CAS coordinates ledger writers, not repository administrators. No real state
repository has been created or activated; all write proof used temporary fixtures.

These are placeholders for a separately approved operation, **not authorization to
apply to a user repository**. First review the machine-generated candidate, its
source/parser provenance, bindings, destination, and initialization intent. Record
the SHA-256 of those exact reviewed bytes independently. Do not pipe a freshly
calculated hash straight into an automatic write or hand-maintain ledger IDs/JSON.

```sh
# Inspection only: compare this digest with the independently approved value.
shasum -a 256 /ABS/reviewed/candidate.json

# Read-only verification; no Git object/ref writes.
./target/debug/hive calendar verify-candidate \
  --repo /ABS/local/notes \
  --commit FULL_LOWER_SOURCE_SHA \
  --mw-bin /ABS/trusted/PINNED_mw \
  --source-repo-id OPAQUE_REPO_ID \
  --account-id google-sub-v1:ACCOUNT_64_LOWER_HEX \
  --calendar-id CONCRETE_CALENDAR_ID \
  --ledger-repo /ABS/dedicated-bare-state.git \
  --ledger-path calendar/ledger.json \
  --state-ref refs/heads/hive-calendar-state \
  --candidate /ABS/reviewed/candidate.json \
  --candidate-sha256 APPROVED_64_LOWER_HEX
```

For initialization only, add `--create-ref` to verification and apply; otherwise
omit it. A selected reack with an old-parser digest change independently requires
renewed `--accept-parser-change` on both commands, even if accepted during candidate
generation. Both commands require the same explicit source and independent binding
flags above. There is no `--ledger-commit`: the approved candidate carries its base.

After reviewing verification output and obtaining separate local-write approval,
the explicit apply form is the following. **WARNING: this writes LOCAL Git objects
and may advance the fixed local state ref. These examples were not executed.**

```sh
./target/debug/hive calendar apply \
  --repo /ABS/local/notes \
  --commit FULL_LOWER_SOURCE_SHA \
  --mw-bin /ABS/trusted/PINNED_mw \
  --source-repo-id OPAQUE_REPO_ID \
  --account-id google-sub-v1:ACCOUNT_64_LOWER_HEX \
  --calendar-id CONCRETE_CALENDAR_ID \
  --ledger-repo /ABS/dedicated-bare-state.git \
  --ledger-path calendar/ledger.json \
  --state-ref refs/heads/hive-calendar-state \
  --candidate /ABS/reviewed/candidate.json \
  --candidate-sha256 APPROVED_64_LOWER_HEX
```

The absolute regular candidate file is captured once, bounded to 8 MiB, with
`O_NOFOLLOW`; unknown/duplicate JSON fields and trailing JSON are rejected. The
64-lowercase-hex SHA-256 approves exact bytes, not a cryptographic signature or
provider proof. Original ledger/history validation and semantic transition replay
must reproduce the **whole candidate**, not merely its digest. Persistence uses
canonical compact ledger JSON and never reallocates the approved enrollment UUID.

Updates require an owned strict tool-shaped receipt bound to ref/path/binding/blob
(not cryptographic signer proof), the fixed ref's whole-commit HEAD equal to the
candidate base, and the matching old blob. Foreign tips, symbolic/dangling refs,
and object-store indirection refuse. Initialization requires absent-ref CAS against
zero; path absence at the selected base alone proves neither branch nor remote
absence. Its commit has that base as sole parent and inherits the **whole seed
tree, possibly including notes**; it is not an orphan state branch. No adoption,
reset, force, or push is supported.

Raw-tree Git plumbing preserves unrelated entries' modes/OIDs without checkout,
index, config edits, hooks, signing, or maintenance. The fixed identity and UTC
timestamp (parent committer time plus one second, not approval wall-clock time)
make verify/apply commit IDs identical for the same inputs. Verify returns
`verified` or `already_applied`; apply returns `applied` or `already_applied`.
`localRefUpdated` is true only for an actual successful CAS;
`publicationReady`, `remoteVerified`, and `remoteRefVerified` are always false.

An exact retry at the expected tip returns `already_applied` without writes. A
later advanced tip refuses, never rewinds or accepts content equality alone. Keep
the **same approved candidate, destination, bindings, source commit, parser artifact,
and permission flags** for recovery; pinning the parser matters. The root
cancellation guard spans capture, replay, object writing, and CAS. Cancellation
observed before CAS stops ref advancement, but failed/cancelled object writes may
leave unreachable objects, with no automatic GC. Once CAS is dispatched, a lost
response/cancellation can mean success: report unknown and retry exactly, never
roll back. Output failure after CAS has the same recovery boundary.

See [macOS persistence evidence](docs/release.md#calendar-local-persistence-checkpoint-2026-11-02)
and the [architecture contract](docs/product-architecture-roadmap.md#calendar-local-approval-and-persistence).
Normal-user/private coordinator integration, real provider authentication wiring
and remote recovery (APIs and Git are not one transaction), hosted CI serialization,
pinned Linux runtime, account cost
hard stop, Calendar OAuth/executor, and remote lease/CAS push remain pending. No
paid service is required by default; Linux remains explicitly deferred, with no
VM probes or target installs requested. Installed binaries and worker are untouched.

## Calendar offline operations

**Offline stage DONE, not an actual publisher.** Built-debug, not installed,
`calendar prepare` proposes a persistable frozen intent; `calendar mark-uncertain`
proposes monotonic recovery evidence; `calendar simulate-ack` produces only an
operator-asserted, **nonpersistable hypothetical result**. None calls Google,
reads credentials, adds source comments, or authorizes provider dispatch.

The following are placeholder generation examples, not activation instructions:

```sh
./target/debug/hive calendar prepare \
  --repo /ABS/local/notes --commit FULL_LOWER_SOURCE_SHA \
  --mw-bin /ABS/trusted/PINNED_mw \
  --source-repo-id OPAQUE_REPO_ID \
  --account-id google-sub-v1:ACCOUNT_64_LOWER_HEX \
  --calendar-id CONCRETE_CALENDAR_ID \
  --ledger-repo /ABS/dedicated-bare-state.git \
  --ledger-commit FULL_LOWER_LEDGER_SHA --ledger-path calendar/ledger.json \
  --logical-id EXISTING_LEDGER_UUID

./target/debug/hive calendar mark-uncertain \
  --source-repo-id OPAQUE_REPO_ID \
  --account-id google-sub-v1:ACCOUNT_64_LOWER_HEX \
  --calendar-id CONCRETE_CALENDAR_ID \
  --ledger-repo /ABS/dedicated-bare-state.git \
  --ledger-commit FULL_LOWER_PREPARED_LEDGER_SHA --ledger-path calendar/ledger.json \
  --logical-id EXISTING_LEDGER_UUID --operation-id PERSISTED_OPERATION_UUID

./target/debug/hive calendar simulate-ack \
  --source-repo-id OPAQUE_REPO_ID \
  --account-id google-sub-v1:ACCOUNT_64_LOWER_HEX \
  --calendar-id CONCRETE_CALENDAR_ID \
  --ledger-repo /ABS/dedicated-bare-state.git \
  --ledger-commit FULL_LOWER_PENDING_LEDGER_SHA --ledger-path calendar/ledger.json \
  --logical-id EXISTING_LEDGER_UUID --operation-id PERSISTED_OPERATION_UUID \
  --observation /ABS/reviewed/operator-observation.json \
  --observation-sha256 APPROVED_64_LOWER_HEX \
  --assert-operator-observation
```

All three select **either** `--logical-id` **or** `--old-source-path` plus positive
one-based `--old-source-line`, never both. The latter selects the ledger's old
acknowledged location, not a new current source location. Prepare alone requires
the source repo/commit/MW inputs. Generation of mark-uncertain and simulation has
no `--repo`, `--commit`, or `--mw-bin` inputs and never reads today's notes/parser.

Prepare requires a selected nonretired entry, no pending operation **anywhere**,
exact current source acknowledgement/parser basis, and open valid-due source.
Enrolled creates reserve `h1` plus a namespace/logical-UUID SHA-256 hex digest;
active updates retain the acknowledged event ID/base etag and require a changed
managed payload hash. Payload equality refuses as a no-op. A fresh bounded
OS-entropy UUIDv4 identifies the operation. Only the selected `pending: prepared`
is added; source ACK, watermark, lifecycle, provider ACK, and other entries stay
unchanged. The deterministic event reservation proves neither provider absence,
ownership, nor an idempotent API. No delete/retire operation is added.

Prepare and mark-uncertain emit strict `schemaVersion: "2"`,
`kind: "calendar-operation-candidate"`, operation `prepare` / `mark_uncertain`,
`operationId`, `intentDigest`, source/parser metadata, exact base, and whole V1
`candidateLedger`. The digest binds the immutable intent and namespace, not phase.
Reviewed ordinary verify/apply supports these candidates with full semantic replay,
the same dedicated bare fixed-ref CAS, cancellation guard, and exact-retry policy
described above. Neither `--create-ref` nor `--accept-parser-change` is allowed.
**Persist the approved prepared intent before any future dispatch. Prepared is
not safe-to-retry or dispatch authorization; both phases block a new prepare.**

Mark-uncertain requires the exact persisted Prepared operation ID and changes only
its phase to Uncertain. Frozen sourceCommit/parser metadata is retained; unrelated
current edits, deleted notes, or unavailable MW do not block recovery. Generating
a new candidate from already-Uncertain state refuses, but exact apply retry is
idempotent. No cancel, clear, reset, or reprepare escape exists.

**Verify/apply CLI compatibility exception:** generic `SourceArgs` still requires
`--repo`, `--commit`, and `--mw-bin` for V2 mark-uncertain candidates. `--commit`
must equal the candidate's frozen source metadata; the repo/MW paths are not read
on this phase-only route. This validates frozen recovery, not current eligibility.
Prepare replay still requires the actual pinned source and parser.

Simulation captures one absolute regular no-follow observation file, at most 1 MiB,
checking the independently approved exact-byte lowercase SHA-256 and rejecting
duplicate/unknown fields or trailing JSON. Its strict observation uses string
`schemaVersion: "1"`, `provenance: "operator_asserted"`, `binding`, `logicalId`,
`operationId`, `intentDigest`, `eventId`, `etag`, `managedLogicalId`, and
`payload: {summary, startDate, endDate, transparency}`. Namespace, IDs, target,
intent and recomputed managed payload hash must match the frozen pending intent;
the asserted result etag must be nonempty, bounded, and control-free. This is not
authentication or an etag-based remote concurrency check. Fake `verified_http`
provenance or an input `remoteVerified` field refuses; there is no trust override.

Output is `schemaVersion: "2"`, `kind: "calendar-provider-simulation"`, with typed
embedded `observation`, `observationSha256`, and `hypotheticalLedger`;
`simulationOnly: true`, `persistenceAllowed: false`, and publication/remote/head
verification false. The hypothetical create/update success sets Active/provider ACK
and clears pending, retaining source ACK/watermark even after current notes vanish.
**Ordinary verify AND apply reject this output even with its correct approved SHA.**
The persisted pending and provider ACK remain unchanged. At the offline checkpoint
there was no authenticated-success constructor or transport. The private adapter
below now exists, and the private in-process ACK-to-local-CAS stage is delivered
below; neither is activated or makes simulation persistable.

Ledger V1 and enroll/reack Candidate V1 wire formats remain unchanged. New operations
use V2 ownership records and local apply receipts with operation/intent fields.
The current writer reads both versions; old state writers refuse V2-owned tips,
which does not imply all read-only V1 ledger readers refuse the unchanged ledger.
Git CAS coordinates ledger writes, not exclusive API dispatch ownership.

See the [offline operation contract](docs/product-architecture-roadmap.md#calendar-offline-operation-contract)
and [macOS evidence](docs/release.md#calendar-offline-operation-checkpoint-2026-11-02).
Production transport activation/write OAuth/native or hosted secret storage,
private coordinator/CLI integration, remote-authoritative dispatch ownership, hosted
workflow/remote CAS push, and billing hard stop remain gates. No paid DB is needed
for this protocol. Normal-user UI/policy and automatic enrollment remain pending;
conservative source-basis restrictions are unchanged. Linux is user-deferred with
no probes, VMs, or target installs. Installed binaries and worker remain unchanged.
All private-output/no-upload boundaries above apply.

## Calendar private HTTP adapter (dormant)

**Private adapter code and loopback-test milestone DONE, not an actual publisher
or activation.** The compiled `calendar_transport` module has no new public export,
CLI command, configuration, environment credential lookup, or production consumer.
POST/PATCH requires a private move-only intent/origin-bound `DispatchPermit` with
no production constructor; Prepared and local Git CAS are not dispatch authority.
The opaque zeroizing, redacted token makes no verified identity/scope claim. No
credential provider, Keychain, refresh, or grant behavior changed.

Fixed production HTTPS Userinfo and Calendar endpoints use no proxy, redirects,
or retries, a shared 30-second operation deadline, 10-second connect timeout, and
64 KiB Userinfo / 1 MiB event limits. Before any Calendar request, same-bearer
Userinfo `sub` must match the binding's existing Google identity namespace. This
does not prove scopes, calendar ownership, or dispatch authority; future activation
needs separate Calendar-write + OpenID authorization, not the Tasks-readonly grant.
Bound-event GET reconciliation observes desired state, not dispatch causality.
Create uses the reserved ID; update validates prior ownership/hash and exact strong
quoted etag before managed-fields-only If-Match PATCH, preserving unrelated private
properties. No unmanaged-event writes, DELETE, timed events, or recurrence support.

Sealed validator results distinguish mutation responses from reconciled observations
and carry immutable endpoint origin. Test-only literal `127.0.0.1` HTTP results
are ineligible for production authentication evidence or durable ACK authority.
At the adapter-only checkpoint no ACK consumer or V3 receipt existed; the private
ACK stage below now adds those local capabilities, not an exclusive worker or remote
Git coordination. JSON or a `remoteVerified` boolean cannot supply trusted results.

Historical adapter-only macOS results: **430 normal workspace passed / 13 ignored**
(serial locked/offline suite), Calendar **145 normal / 3 ignored**; all **15 actual
reqwest loopback tests** passed in latest default-parallel and serial runs and the
full suite. Strict Calendar all-targets Clippy, fmt/build passed. The six real-MW
gates belong to the earlier **415** checkpoint and were **not rerun** for this
adapter. Only loopback HTTP was tested, not live Google authentication/provider
behavior. No tests or network operations ran for this documentation update.

See the [adapter contract](docs/product-architecture-roadmap.md#calendar-private-http-adapter)
and [validation evidence](docs/release.md#calendar-private-http-adapter-checkpoint-2026-11-02).
The new local ACK milestone and remaining activation gates follow. Linux remains
explicitly deferred with no probes, VMs, or target installs.

## Calendar private ACK persistence (dormant)

**PRIVATE in-process ACK -> local CAS code/tests DONE, not activation.** A pre-HTTP
pinned committed ledger context binds the exact base/path/blob and full pending
phase to the intent and sealed HTTP result. Google-only admission rejects actual
loopback origins; operator JSON never becomes authority. Frozen create/update ACKs
from Prepared or Uncertain set only selected Active/provider ACK/pending None,
preserving the old ledger's current source ACK, watermark and all unrelated evidence.
Recovery borrows the retained original transaction and rebuilds the same commit
from the old base on every retry; a later advanced tip refuses, never rebases.

V3 local ownership/apply receipts validate the parent-ledger transition and retain
a V3 minimum-reader and provenance floor on subsequent source/intent writes.
Synthetic test branches remain synthetic and are rejected by production readers,
even after pending clears. Digests/receipts are audit metadata, not signatures;
generic publication/remote/ref verification remains false. No new ACK/publish CLI
or authenticated-JSON path exists: ordinary apply still accepts V1 source and V2
intent candidates only. Production dispatch authority minting remains unavailable;
single-use permits, local CAS and nonces are not distributed fencing.

Historical ACK main implementation-session macOS result: default-parallel
`cargo test --workspace --locked --offline`, **444 normal passed / 13 ignored**;
Calendar **159 normal / 3 ignored**. Strict Calendar all-targets Clippy, fmt/build
passed; six explicit real-MW compatibility gates were rerun and passed earlier in
that session. ACK writer proof is **synthetic handoff + temporary repositories**,
not a real Google authentication pipeline. Earlier full serial runs failed; no
444-test serial pass is claimed. See the [ACK contract](docs/product-architecture-roadmap.md#calendar-private-ack-persistence)
and [evidence and test-stability limits](docs/release.md#calendar-private-ack-checkpoint-2026-11-02).

Remote Git authoritative ownership/fencing, real provider grants/credential wiring,
full worker integration, hosted push trigger/remote CAS, cost hard stop,
and Linux validation later before deployment remain gates. No automatic Calendar
writes run. No actual notes/refs/provider/credentials/private queue/services,
installed binary/settings, or worker changed. This docs pass runs no tests/builds
or network operations; fixture writes in the implementation were temporary only.

## Calendar private coordinator (dormant)

**PRIVATE creator-only coordinator seam code/tests DONE, not hosted execution or
production readiness.** Only a fresh, confirmed exact H0 -> H1 authoritative CAS
in the same invocation can yield one move-only intent-bound permit. Any existing
Prepared/Uncertain pending entry means recovery only, never reacquisition, even
after a lost CAS reply or matching H1 readback. The sealed backend has no production
implementation or constructor; the dispatch bridge cannot be used in production.
No new CLI/export, credentials, workflow, or automatic Calendar writes are wired.

`Ready` carries an **H1 policy pin, not a materialized local repository**. A future
worker must materialize/verify H1 and load the actual local pinned context before
HTTP or durable ACK, and check the root cancellation/deadline immediately before
HTTP. Local ACK is not completion: only confirmed exact authoritative ACK CAS is;
unknown ACK publication may already have committed. Remote ACK recovery is not
implemented. This is not a remote lease, fencing, or exactly-once guarantee.

Latest supplied main-session macOS evidence: **464 normal workspace passes / 13
ignored**, `cargo test --workspace --locked --offline -- --test-threads=4`;
Calendar **179 normal / 3 ignored**, coordinator **20 tests passed serially**.
Strict Calendar all-targets Clippy, fmt/build passed. Six real-MW gates remain
historical 444-checkpoint evidence, not rerun for this slice. See the
[coordinator contract](docs/product-architecture-roadmap.md#calendar-private-creator-only-coordinator)
and [exact validation limits](docs/release.md#calendar-private-coordinator-checkpoint-2026-11-02).
Actual fixed-identity remote CAS, full worker/materialized-pin/grant integration,
authoritative ACK recovery, queue/dispatch operations, source-policy UI, hosted
workflow, account paid-overage hard stop, and later pinned Linux remain gates.
Linux is explicitly DEFERRED; no probes, VMs, or target installs. No paid service
was provisioned. This docs-only pass changes no live state and runs no tests,
builds, or network operations; no uploads or upload offers.

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

- `docs/google-tasks-capture.md` — implemented local inbound lane and live activation gates
- `docs/google-tasks-writeback.md` — accepted same-task first-due design and staged activation gates
- `docs/tasks-first-due-fake-executor.md` — synthetic fixture, built-debug usage, recovery and guarantees/limits
- `docs/tasks-local-publisher.md` — default-enabled real local first-due publisher; review, side effects and recovery (not activated)
- `docs/tasks-hosted-foundation.md` — default read-only check/plan/verify-main, pinned source and immutable Google credentials
- `docs/tasks-remote-coordination.md` — production GET verifier versus test-only remote CAS/coordinator; remaining code and activation gates
- `docs/tasks-conditional-probe.md` — opt-in disposable-task experiment, private journal and approval gates; not production
- `docs/google-auth.md` — Desktop OAuth setup, saved connections, refresh, and troubleshooting
- `docs/architecture.md` — architecture and component boundaries
- `docs/product-architecture-roadmap.md` — product architecture needed for real
  multi-device note/todo sync and future MindWeaver integration
- `docs/protocol.md` — sync API contract
- `docs/hive-sync-runbook.md` — operational examples
- `docs/hive-sync-mobile-onboarding.md` — PWA/mobile setup and smoke tests
- `docs/hive-sync-cloud-deploy.md` — Cloud Run deployment notes
- `docs/hive-sync-monitoring.md` — monitoring setup
- `docs/hive-sync-backup-recovery.md` — backup caveats and future durable-store notes
- `docs/hive-sync-token-rotation.md` — device bearer-token rotation procedure
- `docs/release.md` — build/install/release packaging path
- `legacy/go-reference/README.md` — Go oracle/reference notes
