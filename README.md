# Hive Mind

Hive Mind is the sync/API/mobile companion project for MindWeaver.

- Repository/folder: `hive-mind`
- CLI/binary: `hive`
- MindWeaver remains Markdown-first; Hive handles optional remote sync, device
  auth, the sync API, and the PWA companion.

## What is in this repo

```text
hive-mind/
  crates/hive          CLI entrypoint: `hive`
  crates/hive-capture  desktop-local Google Tasks inbound queue and mw delivery
  crates/hive-calendar credential-free Calendar planning and approved local ledger persistence
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
Normal-user integration, durable provider intents/results (remote APIs and Git are
not one transaction), hosted CI serialization, pinned Linux runtime, account cost
hard stop, Calendar OAuth/executor, and remote lease/CAS push remain pending. No
paid service is required by default; Linux remains explicitly deferred, with no
VM probes or target installs requested. Installed binaries and worker are untouched.

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
