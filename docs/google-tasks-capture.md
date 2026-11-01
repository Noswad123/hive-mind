# Google Tasks inbound capture

## Overview

**Manual capture live-verified; scheduled background polling enabled and verified.**
`hive-capture`, `hive-background`, and `hive captures` provide an inbound-only path
from Google Tasks to the canonical MindWeaver Markdown Inbox. Google retains phone
captures while the computer is off. On manual invocation or an explicitly activated
worker cycle, Hive reads a complete paginated list, stages it in its own private
SQLite queue, invokes the existing `mw todos add` engine, and retains a durable
canonical receipt.

This is the first personal vertical slice, not a prerequisite Postgres rewrite.
There is no always-on Hive service or Hive/server acknowledgement while the
desktop is off, hosted transport, automatic background installation, PWA migration,
Calendar adapter, export, or completion writeback in this lane. Legacy LWW sync is
unchanged and unused here. The `personal` connection and dedicated Google Tasks
list are connected, with one live Android-to-Markdown capture verified. Hive
was built in release mode and installed at `/Users/jdawson/.local/bin/hive` on
2026-10-31, then rebuilt and installed at the same stable path after the inventory
fixes on 2026-11-01. New phone-task-to-background-source-write delivery and live
computer-off catch-up remain unverified; completed polling is not new-write proof.

Desktop OAuth login, refresh, verified account binding, and native macOS Keychain
storage are now implemented. Start with [Google sign-in setup](google-auth.md);
the client, browser consent, Keychain persistence, and refresh have passed live
activation. On 2026-11-01 the user reconfirmed latest-build foreground `auth status`
and `lists --connection personal` success. Latest-build **no-UI background Keychain
access is positively proven while unlocked**, not for denial/locked scenarios.

The worker is configured against the existing `personal` queue, baseline, and list,
with fixed public environment, MindWeaver working directory, 60-second interval,
45-second timeout, and delivery limit 1. Checks returned `configValid: true` and
`queueScopeValid: true`. After a clean/idle check, live Neovim's `mw_ops` was
hotloaded in place, preserving module identity and the active upvalue guard across
four shared references; bridge setup registered the editor. This was a controlled
live update, not a recommendation to replace a running module or discard its guard.

**Enabled:** two controlled `run-once` cycles completed in about 0.5 seconds each,
followed by explicit background install/start. The owned LaunchAgent reports
`serviceActive: true`, `serviceState: loaded_owned`; actual startup and next
scheduled cycles completed about 60.7 seconds apart. All four cycles observed one
unchanged task and staged/delivered zero, with no auth action required (controlled
cycles: generation 1). Native LaunchAgent ownership/status parsing is live-verified.
Inbox/dashboard hashes, bytes, and mtimes remain unchanged; the receipt is still
attempt 1 with no duplicates. Editor readiness is `ready: true`, `noEditor: false`;
live dirty-buffer readiness deferral and restored clean readiness are also verified.
See the activation evidence below for the exact boundary and controls.

## API / Interfaces

Successful command output is JSON with `schemaVersion: "1"`; operational errors
also use a nonzero exit status and stderr. Read JSON reports as well as exit status.

| Command | Arguments / effect |
| --- | --- |
| `hive captures auth login/status/logout` | Explicit saved-connection management; see [auth interfaces](google-auth.md#api--interfaces). |
| `hive captures lists` | Exactly one `--connection NAME` or `--access-token-env HIVE_GOOGLE_ACCESS_TOKEN`; list Google task lists, without creating a queue. |
| `hive captures poll` | Required `--db-path PATH --list-id ID --bootstrap baseline\|import-open` plus exactly one credential mode. Token-env mode also requires `--account-key NAMESPACE`; saved mode forbids it. Fetch and stage only; no Markdown writes. |
| `hive captures deliver` | Required `--db-path PATH --mw-bin ABSOLUTE_PATH`; optional `--limit 100` (default 100, range 1–1000). Local delivery only; no Google token needed. |
| `hive captures run` | Poll arguments plus `--mw-bin ABSOLUTE_PATH` and optional `--limit`; one poll followed by bounded delivery. |
| `hive captures status` | Required `--db-path PATH`; read-only records, no file/directory creation or state transition. A missing DB is an error. |
| `hive captures recover-store` | Required `--db-path PATH`; explicitly recover an identified queue after an interrupted SQLite transaction, without invoking Google or mw. Never creates a missing queue. |
| `hive captures reconcile` | Required `--db-path PATH --mw-bin ABSOLUTE_PATH --capture-id SAVED_ID`; snapshot-only reconciliation, never calls add. |
| `hive captures background configure` | Bind one existing queue and saved verified connection; freeze public routing, executables, destination, and working directory. See the activation sequence below. |
| `hive captures background check` | Read-only local configuration, queue scope/counts, and registration count; no credential access or editor probe. |
| `hive captures background editor-check` | Probe editor readiness without delivery or source writes; no credentials. |
| `hive captures background run-once` | One supervised poll/delivery cycle; can contact Google and write Markdown. Defaults: 45-second cycle budget, delivery limit 1. |
| `hive captures background status` | Read sanitized last-cycle state and launchd ownership/loading status; inspect JSON, not just exit status. |
| `hive captures background install/start/stop/uninstall` | Explicit owned LaunchAgent lifecycle; install does not start, start loads with `RunAtLoad`. Stop/uninstall preserve queue, credentials, and worker data. |

Credential choice is mandatory: the prototype's implicit token-env default has
been removed. Explicit token-env mode accepts only valid `HIVE_GOOGLE_`-prefixed
variable names; there is no raw-token argument or refresh in that mode. Its stable
operator account key must not start with reserved `google-sub-v1:`. Saved mode
derives identity from verified Google issuer/subject, not the alias or client ID,
and never falls back to an ambient token if credentials are absent/locked/denied.
Tokens are not stored in the queue, passed to `mw`, or logged. See the
[auth guide](google-auth.md) for local-only status, refresh rotation, and logout
semantics. `deliver`/`reconcile`/`status`/`recover-store` remain credential-free.

Queue records expose the external account/list/task tuple, stable `captureId`,
state, attempts, frozen destination binding, receipt, last error, provider-change
flag, and raw observation. States include `baseline`, `pending`, `delivering`,
`delivered`, `needs_review`, `calendar_pending`, `ignored`, and `unsupported`.
Delivery reports explicitly count blocked and deferred items; `run`/`deliver`
exit nonzero for blocked/unsupported or Calendar-deferred work. A valid `status`
read still prints these records without treating their state as a command failure.

## Data Flow

1. Resolve the explicit credential mode. Saved connections refresh once at command
   start and verify the same subject via UserInfo before exposing an access token;
   no access cache or implicit browser. Then read Google Tasks using GET only,
   including completed/deleted/hidden records.
   All pages must succeed before queue ingestion: pagination failure stages
   nothing, and absence from a snapshot never implies deletion. The reader bounds
   each page to 2 MiB and each snapshot to 100 pages / 10,000 items; no automatic
   HTTP retry, mid-pagination refresh, or redirect following.
2. Persist observations and the first bootstrap policy per account namespace/list.
   `baseline` permanently excludes eligible tasks seen on the first scan;
   `import-open` explicitly admits eligible existing open tasks. Changing the flag
   later does not rebaseline or import previously excluded records.
3. Derive a 32-lowercase-hex ID from the first 128 bits of SHA-256 over the
   versioned, length-delimited full `(account namespace, list ID, Google task ID)`
   tuple. Retain that tuple and mapping; title, device, and timestamps are not
   identity inputs. Same-title tasks with different IDs stay separate. Changing
   namespaces or moving between lists can capture again: no cross-list deduplication.
4. Discover the trusted absolute `mw` executable's destination through
   `mw config json`, verify capture capabilities, and durably claim a pending job
   as `delivering` before launching the writer. Transactional local claims prevent
   concurrent delivery of the same pending job. Pin the notes root, Inbox, and
   dashboard paths; pass the literal title through argv, never a shell:
   `mw todos add --capture-id ID --json -- TITLE`.
5. Validate the canonical result and persist its receipt independently in Hive.
   Queue acceptance is not proof of a Markdown write. A retained Hive receipt
    prevents later recreation even after the local task/history is removed.

### Live capture proof — 2026-10-31

After baselining the dedicated `MindWeaver Inbox` list empty, one new unchecked,
undated, title-only task was collected and delivered into `introspection/inbox.md`
under Todo / Inbox. The queue records one writer attempt and a canonical receipt.
A second full `run` observed one unchanged provider task, staged zero, and delivered
zero. The canonical stable ID resolves to exactly one Markdown task; Inbox and
dashboard bytes, sizes, and mtimes were unchanged by that repeat. Neovim had no dirty
notes or reconciliation conflicts, and its temporary operation guard was released.

The source now has 12 tasks (7 Do Now, 5 Inbox), including the one new open capture.
No Google task was changed, completed, or recreated. These are manual live checks,
not proof of unattended polling or a live computer-off catch-up; automated crash/
offline queue behavior remains covered separately by isolated tests. Private source
IDs and the activation checkpoint are retained under the app's local state directory.
Read-only confirmation on 2026-11-01 still found one delivered receipt with one
writer attempt; no new live writer proof was performed.

The queue is a dedicated versioned SQLite database, **never the MindWeaver notes
database**. Its parent must already exist. New Unix files use mode `0600`; existing
files must be owner-private and owned by the current user. Symlinks (including
parents), hard links, unrelated databases, and unknown versions are refused.
Queue observations include private task content: protect the queue and status
output accordingly; retain mappings/receipts rather than deleting the queue to retry.

Writable opens identify an existing queue through its private file metadata and
SQLite header before permitting journal recovery, then validate its recovered
schema. `status` remains read-only and can refuse a hot journal; use the explicit
`recover-store` operation rather than deleting that journal. Delivery/reconciliation
also use noncreating, recovery-capable opens. Recovery preserves committed claims
and receipts; it does not retry an interrupted capture. Unknown/truncated headers
or an interrupted initial database creation require operator review, not repair
by guessing a schema.

Queue schema **V2** adds `identity_bindings`, committed transactionally with verified
snapshot ingestion and connection/bootstrap creation. Read-only inspection supports
V1 without changing files. A mutable open recovers an identified journal first,
then migrates V1 by adding the binding table and version; it does not rewrite
existing IDs, receipts, or jobs. Older V1 binaries refuse V2 queues.

Migration does **not** automatically adopt a legacy operator namespace/account
binding. Within one queue, verified and unverified connections cannot share the
same list ID in either direction, including an empty legacy baseline connection.
Distinct list IDs can coexist; multiple verified subjects may use the same list ID.
A refusal requires a future explicit legacy-binding migration. **Do not create a
fresh database or namespace to bypass this guard**: changing capture scope can
duplicate previously captured items. Retain the old queue and receipts for review.

## Edge Cases

- Initially completed, deleted, or hidden tasks are ignored. Notes, subtasks,
  assignments, unsafe titles, and inline `area:` titles are not flattened into
  plain captures. Full raw observations, including notes, are preserved.
- Dated tasks stay `calendar_pending`, preserving the date; they are never silently
  imported as undated tasks. Unsupported/deferred decisions are sticky, not
  automatically approved if a later observation looks simpler.
- Provider changes after accepted capture intent trigger sticky `provider_changed`
  review. Original intent and any receipt remain retained; edits do not become
  LWW updates. A provider change during delivery remains review work even if a
  valid source receipt arrives.
- A failed/partial writer, 30-second timeout, combined output over 1 MiB, invalid
  receipt, or failed receipt commit has an unknown outcome. Hive does not
  automatically retry it. Inspect `status`, then explicitly reconcile the saved ID.
- Reconciliation accepts only an exact stable ID, frozen Inbox destination, and
  literal display title from saved-Markdown snapshot evidence. Missing, moved,
  edited, archived, or ambiguous tasks require review; reconciliation never revives
  them. A missing ID is not permission to retry: historical evidence may be gone.
- MindWeaver's same-ID creation effect is idempotent while the task or managed
  archive evidence is retained. Hive's receipt survives independently after
  acknowledgement. These are durable delivery attempts with an idempotent creation
  effect under those retention conditions; ambiguous cases pause for reconciliation.
  This is **not cross-database exactly-once atomicity** or a guarantee that every
  capture can be automatically recovered without retained evidence.

## Example usage

### Activation gates and current proof

1. Follow [Google sign-in setup](google-auth.md#example-usage): create/select your
   own Cloud project, enable Tasks API, configure consent/test-user policy, download
   a Desktop OAuth client JSON, and explicitly log in. Share only its path, not
   contents/tokens. No automatic Google Cloud actions are performed.
2. Use `lists --connection personal` and choose the actual task list. Saved mode
   verifies the Google subject and derives its namespace; do not supply an account
   key. Legacy token-env mode still needs an operator-maintained stable binding;
   a default `gcloud` cloud-platform token is not evidence of Tasks authorization.
3. Verify the installed stable Hive CLI and trusted absolute `mw` path, review the
   resolved Inbox destination, and choose a dedicated queue path with an existing
   real parent directory. Latest-build foreground status/list access, configuration,
   registration, editor readiness, and unlocked no-UI background Keychain access
   are verified for the current setup.
4. Choose `baseline` versus `import-open` deliberately before the first poll. Then
   retain the existing Android capture, desktop staging, canonical receipt, and
   manual repeat-run nonduplication proof. Those checks are complete. Actual
   computer-off capture/catch-up and a new phone-task-to-background-source-write
   delivery remain unverified. Scheduled polling is enabled and verified;
   isolated tests do not activate an account or phone.

For a new setup's first live proof, use an initially empty personal list, baseline
it, then create one undated top-level Android task without notes or subtasks. A later `run`
should capture it once. First-scan baseline intentionally excludes tasks already
present, so creating the test task before baselining would not prove delivery.

Illustrative manual commands only, after those gates. These use the installed
stable binary. Replace the queue path and selected list ID after
explicit saved-connection login. The current dedicated list already has a baseline
and delivery receipt; do not reset its queue or choose a new namespace to repeat it.

```sh
/Users/jdawson/.local/bin/hive captures lists --connection personal
/Users/jdawson/.local/bin/hive captures poll \
  --connection personal \
  --db-path /absolute/existing-private-directory/google-captures.sqlite \
  --list-id SELECTED_LIST_ID --bootstrap baseline
/Users/jdawson/.local/bin/hive captures status \
  --db-path /absolute/existing-private-directory/google-captures.sqlite
/Users/jdawson/.local/bin/hive captures deliver \
  --db-path /absolute/existing-private-directory/google-captures.sqlite \
  --mw-bin /Users/jdawson/.local/bin/mw --limit 100
/Users/jdawson/.local/bin/hive captures run \
  --connection personal \
  --db-path /absolute/existing-private-directory/google-captures.sqlite \
  --list-id SELECTED_LIST_ID --bootstrap baseline \
  --mw-bin /Users/jdawson/.local/bin/mw --limit 100
```

Subsequent `run` uses the same poll arguments plus `--mw-bin` (and optional limit).
For an unknown outcome, inspect first, then use `reconcile` with the exact saved
`captureId`, same queue, and trusted `--mw-bin`; do not invent a new ID or reset the
queue. There is no automatic background installation. Later export/completion
requires separately authorized write access; Calendar requires its own scopes and
a date/time policy.

### Background activation (enabled and verified)

Foreground verification is a prerequisite for setup or a changed binary, handling
any macOS Keychain approval there. The user reconfirmed both commands succeeded
with the latest build on 2026-11-01. Bounded local status diagnostics with inherited
and minimal environments both succeeded in 0.054 and 0.028 seconds. The earlier
45-second `unknown_outcome` cycle and 12-second local status stall did not reproduce.
The credential-operation failure is resolved operationally, not root-caused; the
same build/credentials now work and no speculative source fix was made. Do not
request a fresh Google grant unless explicitly needed.
`auth status` is local-only; `lists` exercises refresh, UserInfo verification, and
Google Tasks access:

```sh
/Users/jdawson/.local/bin/hive captures auth status --connection personal
/Users/jdawson/.local/bin/hive captures lists --connection personal
```

Do not proceed if foreground verification hangs or fails. Review the public
source selectors explicitly: `MW_CONFIG`, `NOTES_DIR`, `INBOX_PATH`, `MW_INBOX_PATH`,
`DASHBOARD_PATH`, `XDG_CONFIG_HOME`, and `XDG_DATA_HOME`. Only deliberately selected
absolute paths should be present when configuring; never rely on an unreviewed
shell environment. Configure captures these values, not tokens or loader hooks.
Use the **existing physical queue path** and selected list, not a fresh queue or
namespace. Resolve symlinks to their physical spelling and review ownership first.
The following configure/check sequence is already complete for the current setup;
these remain operator examples, not instructions to replace the existing config.

```sh
/Users/jdawson/.local/bin/hive captures background configure \
  --connection personal \
  --queue-path /absolute/existing-private-directory/google-captures.sqlite \
  --list-id SELECTED_LIST_ID \
  --mw-bin /Users/jdawson/.local/bin/mw \
  --nvim-bin /absolute/physical/path/to/nvim \
  --hive-bin /Users/jdawson/.local/bin/hive \
  --working-directory /Users/jdawson/Projects/mind-weaver \
  --interval-seconds 60 --cycle-timeout-seconds 45
/Users/jdawson/.local/bin/hive captures background check
```

Configuration freezes the verified account/list/queue, trusted absolute programs,
working directory, resolved notes/Inbox/dashboard destination, and allowlisted
public selectors. It requires an existing identified queue with matching scope.
Hive/mw executables and their ancestors must be root/current-user owned and not
group/world writable; Neovim is an identity locator, with group-writable ancestors
allowed but not a group/world-writable executable or world-writable ancestry.
State is private and user-owned; symlink/hard-link and ownership checks fail closed.
Replacement requires explicit `--replace` and a stopped coordinator. Uninstall the
owned agent before changing its configuration; do not hand-edit around the checks.

Worker state is fixed under the effective OS user's home at
`Library/Application Support/Hive/background` (`config.json`, `status.json`,
`editors`, `leases`, and persistent locks). `background paths` reports those paths
without creating them. Neither `HOME` nor XDG redirects this state or the auth lock
domain. Worker children use that OS-user home and a cleared, allowlisted environment.

After configuring, arrange cooperative Neovim registration **at startup, before
opening note buffers**, via `require('util.hive_background').startup()` and the
trusted stable Hive binary (`vim.g.hive_bin` if needed). This is a separate editor
setup action, not something `configure` or `install` does. Startup remains the
normal path. The controlled in-place hotload recorded above preserved shared module
and guard identity; do not substitute a fresh module or clear an active guard.
The current editor is registered, but registration alone is not readiness proof.
Inspect readiness separately before any write-capable cycle:

```sh
/Users/jdawson/.local/bin/hive captures background editor-check
```

Current installed `editor-check` reports `ready: true`, `noEditor: false`, and
`deferred: null`. Earlier narrow zombie/Darwin signed-UID inventory fixes and
user-authorized orphan-helper cleanup cleared process-identity readiness without
bypassing identity checks or changing the group-death guard.

A live dirty-buffer readiness probe verified `deferred: dirty_buffers` using its
own **unsaved hidden read-only normal buffer**, with no on-disk file or actual note
edits. The existing guard remained unchanged with `busy: false`. After changed-tick,
name, and visibility checks, the probe was removed and clean readiness restored.
This is live readiness-deferral proof, **not** a live full pending-job dirty-buffer
delivery test.

Two controlled `run-once` cycles completed in about 0.5 seconds each. Each observed
1, unchanged 1, staged 0, delivered 0, with `authActionRequired: false` and
generation 1. The receipt remains attempt 1; no duplicates or Inbox/dashboard
hash, byte, or mtime changes occurred. This proves completed no-UI polling while
unlocked, not a new source write or Keychain denial/locked behavior.

Only after readiness succeeds, and with explicit authorization for a live cycle:

```sh
/Users/jdawson/.local/bin/hive captures background run-once
/Users/jdawson/.local/bin/hive captures background status
```

`run-once` is a live write-capable activation step, not a dry run. Inspect its
sanitized outcome/counts and canonical receipts before authorizing unattended use.
The worker polls read-only, then delivers at most one pending item per cycle;
defaults are a 60-second interval and 45-second cycle budget. Google retains work
while the computer sleeps or is off; this is not a hosted always-on service.

Dirty scoped buffers, busy operations, non-normal/active UI modes,
unregistered/multiple/uncertain editors, source locks, or destination mismatch defer
delivery rather than force a write. With a ready registered editor, its existing
MindWeaver operation guard owns delivery and reconciliation; only definite absence
of an editor permits the supervised direct path. Background auth forbids Keychain
UI and never launches consent. Action-required auth pauses refresh for the same
credential generation; see [foreground recovery](google-auth.md#troubleshooting).

After an explicitly successful live cycle and separate authorization:

```sh
/Users/jdawson/.local/bin/hive captures background install
/Users/jdawson/.local/bin/hive captures background start
/Users/jdawson/.local/bin/hive captures background status
```

`install` writes only the owned `com.hive-mind.google-captures.plist` in the OS-user
`Library/LaunchAgents`; it does **not** load/start it. `start` bootstraps it into the
user GUI domain, with `RunAtLoad` allowing an immediate cycle and `StartInterval`
scheduling later cycles. Byte-exact generated plist content, file ownership, and
loaded origin/program/argv/working-directory identity are checked before control;
a matching label or marker alone is not ownership. Unknown/colliding jobs are refused.

Install/start have now been executed: `serviceActive: true`,
`serviceState: loaded_owned`. Native LaunchAgent ownership/status parsing and
actual scheduled polling are verified by these two completed launchd cycles:

| Cycle | ID | Started (epoch ms) | Finished (epoch ms) |
| --- | --- | --- | --- |
| Startup | `f0db755b2def92cc0652270584dbd2b6` | `1793571599017` | `1793571599542` |
| Next scheduled | `0ff4aaabfd218db77044f769d80e691a` | `1793571659700` | `1793571660255` |

Starts were about 60.7 seconds apart. Both had no pending work, observed 1 unchanged,
staged/delivered 0, and no auth action required. Polling is running in the GUI user
session at a 60-second interval, 45-second timeout, limit 1. Google holds work while
the computer is off; there is no server. A new phone task reaching Markdown through
background source-write delivery and live computer-off catch-up are **not yet
live-verified**. Source-write/guarded delivery has isolated integration and earlier
manual capture proof only.

Use the stable binary for routine inspection and lifecycle control. `start` resumes
a stopped owned agent; `uninstall` is optional removal, not needed just to pause:

```sh
/Users/jdawson/.local/bin/hive captures background status
/Users/jdawson/.local/bin/hive captures background stop
/Users/jdawson/.local/bin/hive captures background start
/Users/jdawson/.local/bin/hive captures background uninstall
```

Stop unloads the owned job and retains config/queue; uninstall also removes only
its owned plist and retains all state, including receipts, queue, credentials, and
worker configuration. Shutdown/timeouts supervise owned
process groups and retain uncertain claims for review, not automatic replay. An
RPC disconnect, elapsed deadline, or missing coordinator PID alone cannot release
the editor write barrier: it needs positive writer-exit/cooperative delivery-lock
proof. Safety assumes trusted binaries, participating editor guards/locks, and
normal process-group behavior; this is not filesystem-wide exclusion against
arbitrary same-user writers or descendants that deliberately escape supervision.

### Recorded implementation validation

Latest validation on 2026-11-01 after the inventory fixes: **270 normal workspace
tests passed, five ignored by default**. Strict all-targets Clippy passed for
background, auth, and capture; both explicitly ignored isolated real-Neovim tests
passed after the fixes. The inherited-FD test runs in an isolated single-test child
to avoid parallel fork-test descriptor leakage; its original assertions and
production semantics are unchanged.

The real-MindWeaver CLI integration and fake-Google/real-Hive/MindWeaver integration
passed earlier, **not rerun after these fixes**. The fifth ignored entry is the
crash fixture already exercised by normal recovery tests. Earlier 2026-10-31 checks
also passed format validation and Neovim's **32 background** and **54 safety** tests;
those are historical results, not newly rerun suites. Rust check commands (the
real-writer commands below describe the earlier integration checks):

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
MW_TEST_BIN=/Users/jdawson/.local/bin/mw cargo test -p hive --test captures --locked -- --include-ignored
HIVE_TEST_BIN=/Users/jdawson/Projects/hive-mind/target/debug/hive MW_TEST_BIN=/Users/jdawson/.local/bin/mw cargo test -p hive-capture --lib google::tests::fake_google_to_real_hive_and_mw_durable_capture --locked -- --ignored --exact
cargo clippy -p hive-background -p hive-google-auth -p hive-capture --all-targets --locked -- -D warnings
```

Integration evidence uses fake GET-only Google HTTP responses/test tokens,
library ingestion into the queue, temporary notes, the actual Hive CLI delivery
path, and actual installed `mw`. Those isolated tests do not by themselves establish
live account/device behavior. Separately, the authorized manual activation above
has now verified OAuth/Keychain/refresh, actual `poll`/`run`, and real-note delivery
for one Android capture. Latest-build foreground status/lists, two controlled
background cycles, and actual LaunchAgent startup/next scheduled cycles now verify
enabled polling and no-UI Keychain access while unlocked. Live dirty-buffer
readiness deferral is proven, not full pending-job dirty-buffer delivery deferral.
New phone-task-to-background-source-write delivery and live computer-off catch-up
remain unverified; isolated integrations and earlier manual capture are the only
source-write/guarded-delivery proof. No additional tests ran during activation.
This docs-only update adds or reruns no tests and changes no source code; the
270-test record is unchanged. Calendar/export/completion writeback, lifelog,
and PWA migration remain pending. Background, auth, and capture pass strict Clippy;
the workspace-wide warnings-as-errors gate still encounters existing warnings in
the legacy server/desktop crates.

## Related files

- [Google OAuth setup and security contract](google-auth.md)
- [CLI arguments and dispatch](../crates/hive/src/main.rs)
- [Background CLI and supervised cycles](../crates/hive/src/captures_background.rs)
- [Background config, state, editor coordination, and launchd](../crates/hive-background/src/lib.rs)
- [Capture types and reports](../crates/hive-capture/src/lib.rs)
- [Read-only Google client and end-to-end fake-provider test](../crates/hive-capture/src/google.rs)
- [Private queue, identity, bootstrap, and claim handling](../crates/hive-capture/src/store.rs)
- [MindWeaver argv adapter, receipts, and reconciliation](../crates/hive-capture/src/mindweaver.rs)
- [CLI integration tests](../crates/hive/tests/captures.rs)
- [Product architecture roadmap](product-architecture-roadmap.md)
- [MindWeaver workflow roadmap](../../mind-weaver/docs/todo-workflow-roadmap.md) (sibling checkout)
