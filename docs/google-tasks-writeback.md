# Google Tasks first-due writeback

## Overview

**Real local publisher implemented/reviewed with validation limits — 2026-11-04;
not ready for live activation.** The retained fake executor stays fake-only; the
separate default-enabled [local publisher](tasks-local-publisher.md) now implements
real due-only dispatch and Git-backed recovery. No live publisher/native/provider
operations ran this step; no installation/handoff export or notes changes occurred.
Prior disposable live proof and saved TasksWrite credentials persist, but do not
authorize applying to a canonical real task. The
accepted eventual user flow is:

```text
Edit the canonical Markdown task
  → commit AND PUSH to the private NOTES repository main
  → GitHub-hosted workflow reads the exact pushed commit
  → set the first due date on the SAME existing Google Task
```

Do not create a Calendar event. Calendar publication is deferred; existing
Calendar implementation and evidence remain historical work, not removed code.
The live Google Tasks inbound lane stays Google-read-only with its existing
`personal` grant, queue, baseline, worker, and receipts. A separate TasksWrite grant
was intentionally saved and a disposable task tested after explicit approvals;
no production/hosted outbound workflow is activated.

The earlier milestone provides **credential/provider-free local advice** in the
**built-debug, not installed** `./target/debug/hive tasks handoff` and `tasks due-plan`:
portable receipt-backed selected capture identity handoff and exact committed-source
first-due planning. The new built-debug `tasks auth` and `tasks observe` add a
separate local TasksWrite profile and authenticated, handoff-bound exact-task READ,
not mutation, ACK, or a hosted executor. At that earlier milestone no new consent
or live operations ran; the subsequent probe session is recorded below.
Validation was independently run during implementation, not rerun
for this documentation update.

The subsequent **local fake-only executor** now persists intent, conditionally
changes one synthetic task's first due and recovers without redispatch. It freshly
collects committed source using `hive_tasks::policy`, shared with due-plan; it never
executes plan JSON. It adds no Google mutation or ACK, production dispatch trait,
HTTP PATCH, hosted coordinator or live-write CLI. See the dedicated
[usage, fixture, state machine and guarantees/limits](tasks-first-due-fake-executor.md)
and [new validation checkpoint](release.md#tasks-first-due-fake-executor-checkpoint-2026-11-04).

**Separate opt-in probe has limited live validation (2026-11-04).**
The nondefault `tasks-conditional-probe` feature adds disposable-task inspect/run;
default builds exclude command and adapter. It is not the production writer and
does not change the fake-only executor. See the [compact runbook, exact interface,
journal and separate approvals](tasks-conditional-probe.md). After explicit login
and date-change approvals, `tasks-write-personal` saved `tasks` + `openid` and the
user-nominated disposable task accepted current-ETag Nov 5→Nov 6 PATCH (200), then
rejected stale-old-ETag PATCH back (412 valid Google JSON). Final and independent
read-only verification retained Nov 6/new ETag/non-due fields. Two PATCHes attempted,
one accepted; no replay/rollback/delete. The task remains open Nov 6. This is
single-target/session evidence, not a universal guarantee or production canonical
ACK. Readonly `personal` lists succeeded after login and after the test. Probe work
did not edit the real phone capture, canonical notes, worker, queue/baseline, source
refs or workflows; no code commit/push/install. See the
[live checkpoint and post-fix validation](release.md#live-tasks-conditional-proof-2026-11-04).

## API / Interfaces

### Portable selected identity handoff

Desktop queue mappings are inaccessible to a hosted runner. Export and review only
the selected private binding rather than syncing the SQLite queue. The portable
contract binds:

- opaque source repository ID, independent of local path/URL: 1–256 ASCII letters,
  digits, or `._:-` characters;
- account namespace, task list ID and exact provider task ID;
- stable canonical capture identity, tied to the retained delivery receipt;
- receipt provenance and digest, retained as audit evidence, not authentication;
- explicitly accepted **current** provider baseline, labeled as current acceptance,
  never invented or mislabeled as the original capture-time baseline.

The stable capture marker is an irreversible derived ID, not a way to recover the
provider task ID. Identity must come from the retained tuple and receipt. Never
title-match, fuzzy-match, or assign a replacement identity when evidence is missing.
The handoff contains private IDs/evidence, not credentials. Export requires an
absolute queue path and explicit `--accept-current-baseline`. Read-only inspection
loads only the selected capture and its verified account binding; the capture must
be delivered unchanged, with no provider-change flag/error and a valid retained
receipt. Its accepted current baseline must be open and undated.

Handoff JSON uses `schemaVersion: "1"`, `kind: "google-task-handoff"`,
`sourceRepoId`, `captureId`, `accountKey`, `listId`, `providerId`, `receipt`,
`acceptedCurrentBaseline: true`, and full `baseline` GoogleTask. Receipt evidence
contains `method` (`todos-add` or `snapshot-reconciliation`), `sha256`, `stableId`,
`sourceId`, `sourcePath`, and `sourceRevision`. The receipt digest hashes the
serialized stored receipt; it is audit provenance, not a signature or fresh GET.

### Committed source and advisory plan

The implemented provider-neutral `task_snapshot` facade shares existing committed
snapshot machinery. It takes an explicit full lowercase commit SHA and trusted absolute `mw` binary,
reads bounded committed Markdown through the snapshot parser, and does not read
live buffers or the mutable working tree. A saved dirty note is still invisible
at that SHA. A local commit alone does not prove a successful push to NOTES `main`.
Reusing snapshot machinery does not activate Calendar or adopt its event ledger.

The local planner consumes the explicit source-repository binding, selected handoff,
and operator-supplied remote observation. Outcomes are advisory:

| Outcome | Meaning |
| --- | --- |
| `set_first_due` | Eligible source due and accepted undated remote basis permit a proposed due-only change on the bound task, not dispatch. |
| `already_satisfied` | Observed due already equals the desired date; not proof Hive wrote it. |
| `blocked` | Identity, source, remote evidence, conflict, or supported-state requirements fail. |

Always `publicationReady: false`, `remoteVerified: false`, `pushVerified: false`,
`conditionalWriteSafe: false`, and `hiveWriteProven: false`.
Supplied evidence remains **operator asserted**, not a fresh authenticated GET,
proof of account access, or write authority. No flag or JSON field upgrades it.

The observation envelope is `schemaVersion: "1"`,
`kind: "google-task-observation"`, `sourceRepoId` (same opaque binding),
`accountKey` (`google-sub-v1:` plus 64 lowercase hex digits), `listId` (exact list),
and `task` (the full GoogleTask observation, not just a due field). Its account,
list and task ID must match the handoff. The observation is never fetched by this
command. Both handoff and observation inputs require absolute regular-file paths,
are bounded to 64 KiB each, and refuse a final symlink. Unknown envelope fields
refuse. Both commands emit JSON stdout; neither writes an output file.

Plan JSON uses `kind: "google-task-first-due-plan"` and
`observationTrust: "operator-asserted"`, retaining commit/parser and receipt
provenance plus exact input `handoffSha256` / `observationSha256` digests. A blocked
plan includes a `reason`; inspect `decision`, not only process exit status.
`set_first_due` includes only a proposed `patch: {"due": "…"}`; no PATCH is sent.

### Separate auth and exact-task observation

`tasks auth login/status/logout` selects `AuthProfile::TasksWrite`, requesting exactly
`https://www.googleapis.com/auth/tasks` plus `openid`. Native local Keychain service
is `hive/google-tasks-write/v1`; the OS-user lock namespace is
`google-tasks-write-auth`. Readonly `personal` and Calendar records/defaults are
unchanged. See [auth isolation and consent coupling](google-auth.md#separate-tasks-write-authorization-local-only):
separate local records do not isolate Google-side grants; prefer a separate Desktop
client to reduce consent coupling. The provider scope is broad, not single-task or
due-only authorization. Hosted Linux storage/rotation remain future work.

`tasks observe --connection NAME --handoff /ABS/private/reviewed-handoff.json`:

1. Validates the connection, absolute regular no-follow handoff file (at most
   64 KiB), handoff schema/baseline/tuple and bounded list/task IDs **before native
   credential setup**.
2. Requires the TasksWrite store, refreshes the saved grant once and verifies fresh
   UserInfo against its retained account binding. Rotation may update the local
   credential store before UserInfo; no implicit login or fallback occurs.
3. Requires that verified identity namespace to equal the handoff `accountKey`, then
   uses the **same bearer** for one exact GET at the fixed Google Tasks endpoint,
   `lists/{listId}/tasks/{providerId}` (IDs encoded as path segments). It validates
   the returned ID and supported open-task shape, not equality to the old baseline.
4. Emits buffered private JSON stdout only. No queue, note or Git changes, task
   mutation, redirects, retries, or list fallback. There are no environment-bearer,
   provider-endpoint, list or task overrides. Resource responses cap at 64 KiB;
   encoded wrapper output must be smaller than 64 KiB.

The distinct unsigned wrapper uses `schemaVersion: "1"`,
`kind: "google-task-live-observation"`, `handoffSha256`, RFC3339 `observedAt`, exact
source/capture/account/list/provider tuple, producer/method/provenance, and nested
`observation` in the existing `google-task-observation` schema.
`remoteReadVerifiedDuringCollection: true` describes only the producing production
execution's authenticated read; copying or editing JSON does not authenticate it.
`portableEvidenceAuthenticated`, `publicationReady`, `pushVerified`,
`conditionalWriteSafe`, and `hiveWriteProven` are all false.

**Offline due-plan rejects the wrapper and added provenance/trust fields.** An
operator may explicitly extract the nested `observation` into the existing schema,
but that input remains operator asserted and the plan's `remoteVerified` remains
false. Neither file possession, digest nor timestamp establishes authenticity,
freshness at later use, dispatch rights, or proof Hive wrote a due date.

### Local publisher

Default-enabled, built-debug, not installed `tasks publish init/inspect/prepare/apply/recover`
uses a separate dedicated bare fixed-ref Git installation, not executable plan JSON
or hosted authority. Init/inspect are local with no auth/Google; prepare authenticates
an exact GET and freezes local state; explicit reviewed apply can send one due-only
conditional PATCH; recovery is source-independent and GET-only when needed. See
the [exact flags, side effects, state machine and limits](tasks-local-publisher.md).
Local main-tip checks are not GitHub pushed-main authorization. No local-pilot
operation is approved by these examples or the earlier disposable-task consent.

### Provider payload boundary

Use Google Tasks' date-only due semantics, encoded as an RFC3339 value such as
`2026-11-10T00:00:00.000Z`. This represents the literal calendar date, not an
appointment time. Do not timezone-convert it or promise notifications. Only `due`
may be patched on the exact existing task. No create, delete, reschedule of an
existing different due, date clear, title/notes update, or completion in this pilot.

## Data Flow

1. Retain the existing inbound queue mapping and canonical receipt. Explicitly
   select one capture; review/export the portable binding and accept its current
   baseline without rewriting the inbound history.
2. Edit the canonical task's literal `due:YYYY-MM-DD`. The local planner reads only
   the specified committed snapshot and checks its stable identity against the
   handoff. It produces advice without credentials, provider I/O, or writes.
   Separately approved local auth/observe can collect exact-task evidence now;
   explicit nested extraction does not upgrade the offline planner's trust.
3. **Later, after activation gates:** a successful push to private NOTES `main`
   triggers a serialized trusted workflow. Pin the immutable pushed SHA and verify
   branch/repository/freshness policy; do not substitute a later mutable checkout.
   A local plan's `pushVerified: false` is not publication evidence.
4. Authenticate the separately authorized Tasks-write principal, then GET only the
   exact bound account/list/task. Compare the fresh state with the reviewed basis;
   block conflicting due, meaningful remote changes, or unsupported state.
5. Persist a frozen durable intent before dispatch. Verify the Tasks API's actual
    conditional PATCH support/semantics and conflict behavior in the production
    adapter, retaining the narrow checks with limited live probe evidence, then send
    only `due` with the validated precondition to that same task. Do not assume Calendar
   adapter tests prove Tasks concurrency behavior.
6. Validate the response and durably acknowledge the outcome. Recover uncertain
   dispatch or acknowledgement using retained intent and exact-task observations,
   not blind repeated writes. Matching due alone establishes observed satisfaction,
   not mutation causality. Provider API and durable state are not one transaction.

## Edge Cases

- Missing/duplicated stable identity or missing receipt/binding blocks; a matching
  title or source line is not continuity proof. Never reset the queue to recover.
  The literal source marker must be a canonical indented standalone
  `<!-- mw-task:v1:ID -->` attached to its task, with the 32-lowercase-hex capture ID.
  Parser identity and literal evidence must agree; duplicate/orphan/malformed
  markers refuse. Frontmatter/fenced examples do not establish marker ownership.
- Missing, completed, invalid/duplicate-due, or otherwise unsupported source does
  not authorize remote deletion, completion, due clearing, or creation.
- A different remote due requires review, not rescheduling. Meaningful changes
  relative to the accepted baseline and unsupported remote state block. A missing,
  deleted, hidden, completed, or moved provider task is not permission to recreate.
- A repeated equal due can be `already_satisfied` but is never a fabricated Hive
  write acknowledgement. Operator-supplied observations remain unverified.
- Known provider metadata `kind`, `selfLink`, `webViewLink`, and `position` is shape-validated
  and treated as nonsemantic alongside `etag`/`updated` for baseline comparison.
  Output-only `links: []` is supported; malformed/nonempty links still refuse.
  `selfLink` is never followed or used as identity. Unknown provider fields require
  review; meaningful baseline changes, completion and unsupported task shapes block.
- Invalid observe input fails before native setup. Missing/logged-out/wrong-profile,
  locked/denied/busy credentials, refresh/UserInfo/account mismatch stop before the
  Tasks GET; wrong-ID, unsupported or oversized responses produce no observation.
  Logout clears only the local grant, retains account binding, does not revoke
  Google authorization and cannot retract an already-issued in-flight bearer.
- Timeout, cancellation, lost response or failed durable ACK after dispatch may
  mean the write succeeded. Retain uncertain evidence and reconcile; do not erase
  pending intent, infer failure, or automatically resend.
- Future own due updates may trigger the inbound lane's sticky `provider_changed`.
  Preserve its original receipt, frozen intent, and review state until explicit
  outbound ACK integration exists. Do not suppress review or recapture the task.
- Handoffs, observations and plans contain private task content/IDs/paths. Do not
  print them in CI logs or automatically upload artifacts, including to Atlassian.
  Tokens and client secrets never belong in these files or committed notes.

## Example usage

**Implemented built-debug examples, not installed commands or activation steps.**
These examples were not executed in this documentation update. Paths and IDs are
placeholders. Both commands emit private JSON stdout; reviewing/storing the selected
handoff bytes is an explicit private operator step, not a command file-output option.

```sh
./target/debug/hive tasks handoff \
  --queue /ABS/private/google-captures.sqlite \
  --capture-id SELECTED_CAPTURE_ID \
  --source-repo-id OPAQUE_REPO_ID \
  --accept-current-baseline

./target/debug/hive tasks due-plan \
  --repo /ABS/local/notes \
  --commit FULL_LOWER_SHA \
  --mw-bin /ABS/trusted/mw \
  --source-repo-id OPAQUE_REPO_ID \
  --handoff /ABS/private/reviewed-handoff.json \
  --observation /ABS/private/operator-observation.json
```

No remote GET, push verification, or provider mutation is implied by these local
commands. Source reading inherits Git 2.50+ at `/usr/bin/git` and a trusted standalone
`mw` supporting `todos snapshot --json`, isolated from original notes/config/DB.
This is input isolation, not an OS sandbox for hostile binaries.

**Activation-sensitive auth/observe examples — not instructions to run now.**
These placeholders were not executed by this docs pass; separate authorized live
login occurred in the probe session. Login opens browser consent and can persist a separate local grant;
observe refreshes credentials and contacts Google. Status/logout involve the local
secure store (possibly OS prompts), with no live Google requests; logout removes
the local grant only, not Google's grant or an in-flight bearer. All use the
**built-debug, not installed** binary:

```sh
./target/debug/hive tasks auth login --connection tasks-write-personal --client-json /ABS/private/desktop-client.json
./target/debug/hive tasks auth status --connection tasks-write-personal
./target/debug/hive tasks auth logout --connection tasks-write-personal
./target/debug/hive tasks observe --connection tasks-write-personal --handoff /ABS/private/reviewed-handoff.json
```

These are independent interface examples, not a sequence (observe requires a saved
grant and cannot follow logout without separate explicit reauthorization).

### Staged activation plan

1. **Done — local code/review milestone:** portable selected receipt-backed handoff,
   shared committed-source facade and first-due planner. See
   [supplied implementation validation](release.md#google-tasks-local-first-due-checkpoint-2026-11-03):
   that checkpoint's Tasks integration was 17 passed / 1 ignored, plus one explicitly
   run real-MW gate passed on isolated TEMP committed source. Its latest full-workspace
   attempts had unresolved Calendar HTTP-loopback failures; the earlier intermediate
   serial pass is not final full-workspace proof. No full-workspace rerun this milestone.
2. **Done — local separate auth + exact READ code/review:** TasksWrite exact scopes,
   separate Keychain/locks, fake lifecycle and loopback tests, handoff-bound
   refresh/UserInfo/same-token exact GET, and unsigned observation boundary. See the
   [current targeted checkpoint](release.md#google-tasks-auth-and-exact-read-checkpoint-2026-11-04).
   No live consent/observe or grant installation was performed at that earlier
   code-only checkpoint; subsequent authorized probe login saved the separate grant.
   Local isolation does not isolate Google grants; leave `personal` readonly and
   Calendar untouched.
   Hosted secret storage, rotation/serialization, failed-save recovery and
   reauthorization remain gates; MacKeychain is local only, not a Linux backend.
3. **Done — reviewed fake-only executor:** explicit dedicated bare local state and
   separate synthetic world; Prepared → Attempting CAS before private move-only
   dispatch; revision-conditional due-only patch, separately saved world/outcome and
   frozen observation-only recovery. Uncertain/conflict blocks; matching due has
   UNKNOWN causality. Repeated unchanged recovery adds no commit. No production
   readiness or Google conditional-write proof; process-crash-only, not hosted fencing.
4. **Done — limited live provider evidence:** the separate nondefault
   [conditional-probe experiment](tasks-conditional-probe.md) succeeded after
   explicit approvals on one already-dated disposable target. Current-E0 due-only
   D0→D1 returned 200/new E1; stale-E0 attempt back returned valid Google JSON 412;
   final and independent GETs retained D1/E1/non-due fields. Maximum two PATCHes,
   no retry/restore. The initial GET-only refusal led to the narrow `links: []`
   compatibility fix, not relaxed unknown/nonempty-field acceptance. This is
   empirical single-session evidence, not universal guarantees or production
   authority. The broad scope still needs narrow production executor policy.
5. **Implemented/reviewed — local publisher, with validation limits:** default-enabled
   [single-target publisher](tasks-local-publisher.md), separate bare Git intent/state,
   reviewed full-resource/source checks, Prepared/Attempting CAS before one due-only
   PATCH, validated response persisted before confirmation/ACK, frozen GET-only
    recovery. No live activation; the prior independent 45-test default-parallel pass
    does not establish stability or explain earlier failures/timeouts. See the
   [checkpoint](release.md#tasks-local-publisher-checkpoint-2026-11-04).
    **Done — hosted read-only foundation, not execution:** default
    [`tasks hosted check/plan`](tasks-hosted-foundation.md), immutable injected strict
    V1 TasksWrite record and alias/account pins; exact private parser-copy comparison
    before execution/source-content reads; consistent push claims and shared source
    policy before late secret lookup. Check has no HTTP/native calls; plan uses one
    refresh/fresh UserInfo/exact GET and returns only redacted flags/decision. Claims
    are spoofable, not GitHub authentication or remote-main proof. Rotation requiring
    save fails before UserInfo/token release/GET as `auth_action_required`; manual
    separately authorized provisioning is needed, with no secret-update API. No
    hosted apply/PATCH/state commit/native fallback or workflow was added.
    **Current update — remote-authority milestone (Nov 4):** the ONLY new production
    interface is read-only `tasks hosted verify-main`: same preflight then late
    `GITHUB_TOKEN` only, fixed private GitHub identity/exact-main GETs. Fresh
    production-origin flags are observations, not push authentication or dispatch;
    check/plan are unchanged claims-only interfaces. Remote expected-OLD Git push
    CAS and integrated coordinator are **entirely cfg(test)**, reviewed with no
    remaining P1/P2 in laboratory scope, not normal-compiled production execution.
    Temporary bare receive-pack/loopback tests cover creator-only fresh ACK permits,
    persisted Prepared/Attempting, evidence-before-confirmation/ACK and frozen
    no-replay recovery. See [protocol and limits](tasks-remote-coordination.md).
    **NEXT — missing production code bridges:** GitHub HTTPS Git credential delivery
    with secret-free URL/argv/config/logs isolation; reviewed inception/independent
    root pin and quiescent local-to-hosted handoff; production coordinator constructor
    and writer wiring with credential pause/recovery. No hosted writer command or
    workflow deployment exists. Source/Git/Google are non-atomic; local permits/locks
    are not fencing and reset/admin ABA is not solved. Do not use
   expiring caches/artifacts as authoritative state or sync live SQLite. Integrate
   outbound acknowledgements with inbound review explicitly, not by discarding it.
6. **Hosted gate:** serialize `main` runs, pin exact pushed SHA and trusted artifact,
   enforce freshness, bound time/retries, protect private output and secrets, and
   provide recovery/status controls. No `.github` workflow is added now.
7. **Cost/platform gate:** verify included Actions allowance and an effective
   paid-overage hard stop; alerts alone are insufficient. No paid service by
   default. Validate pinned Linux runtime before deployment; Linux work remains
   deferred now, with no installs, VM or container probes requested.
8. **Explicit activation gate:** separately approve production grants as needed, hosted
   secrets/state/workflow deployment and one selected first-due live operation,
   then verify same-task identity and recovery/nonduplication before expansion.
   Design acceptance alone is not activation approval.

## Related files

- [Inbound capture and retained receipts](google-tasks-capture.md)
- [Fake-only executor usage, fixture, state machine and limits](tasks-first-due-fake-executor.md)
- [Real local publisher: commands, side effects, review and frozen recovery](tasks-local-publisher.md)
- [Hosted read-only foundation: exact interfaces, trust limits and credential pauses](tasks-hosted-foundation.md)
- [Production current-main verifier and test-only remote coordination protocol/limits](tasks-remote-coordination.md)
- [Separate opt-in conditional-probe runbook and approvals](tasks-conditional-probe.md)
- [Readonly, local TasksWrite, and deferred Calendar authorization](google-auth.md)
- [Current architecture priority and historical Calendar work](product-architecture-roadmap.md)
- [Current checklist](../plan.md)
- [Capture job/provider types](../crates/hive-capture/src/lib.rs)
- [Built-debug CLI](../crates/hive/src/tasks.rs) and [shared first-due policy](../crates/hive-tasks/src/policy.rs)
- [Handoff-bound authenticated read and unsigned wrapper](../crates/hive/src/tasks_observe.rs)
- [Fixed-endpoint exact-task GET](../crates/hive-capture/src/google.rs)
- [Portable handoff schema and baseline validation](../crates/hive-capture/src/handoff.rs)
- [Provider-neutral committed task facade](../crates/hive-calendar/src/task_snapshot.rs)
- [Tasks integration and real-MW gate](../crates/hive/tests/tasks.rs)
- [Desktop queue mapping and deterministic capture identity](../crates/hive-capture/src/store.rs)
- [Canonical writer receipt validation](../crates/hive-capture/src/mindweaver.rs)
- [Existing committed snapshot machinery](../crates/hive-calendar/src/source.rs)
- [Current auth profiles](../crates/hive-google-auth/src/lib.rs)
- [Account-bound refresh/rotation orchestration](../crates/hive-google-auth/src/provider.rs)
