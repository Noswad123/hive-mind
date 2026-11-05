# Tasks conditional-write probe

## Overview

**Opt-in diagnostic code/review complete; limited live proof succeeded 2026-11-04,
not production activation.** `tasks-conditional-probe` is a nondefault Cargo feature exposing
`hive tasks conditional-probe inspect/run`. Default builds exclude both command
and adapter. This experiment is separate from the fake-only executor and from the
disabled production/hosted first-due workflow; it supplies neither a Google ACK
for that workflow nor production dispatch authority.

Official guidance supplies a rationale, **not empirical stale-precondition proof**:

- [Tasks performance: “Using patch in a read-modify-write cycle”](https://developers.google.com/workspace/tasks/performance)
  describes `If-Match` with the quoted current ETag and an updated tag on success;
  its general examples explicitly illustrate concepts applicable to Tasks.
- [tasks.patch](https://developers.google.com/workspace/tasks/reference/rest/v1/tasks/patch)
  documents the PATCH endpoint and `https://www.googleapis.com/auth/tasks` scope.
- [Task resource](https://developers.google.com/workspace/tasks/reference/rest/v1/tasks)
  documents `etag` and date-only `due`: a scheduling date, with no time read/write.
  Midnight UTC encoding below does not introduce appointment-time semantics.

## API / Interfaces

| Subcommand | Required flags |
| --- | --- |
| `inspect` | `--connection NAME --expected-account-key google-sub-v1:64_LOWER_HEX --list-id EXACT --task-id EXACT --journal ABS_NEW` |
| `run` | All inspect flags, plus `--expected-etag EXACT_QUOTED_E0 --expected-original-due YYYY-MM-DD --chosen-due YYYY-MM-DD --expected-baseline-sha256 64_LOWER_HEX --acknowledge-disposable` |

Every invocation needs a **new** journal; an existing one is never resumed for
mutation. Connection names are 1–64 lowercase letters/digits/`-`/`_`. Exact IDs are
nonempty, at most 256 bytes, with no surrounding whitespace, controls or `.`;
`@default` is refused. They are encoded as path segments, not resolved via a list search.
ETags must be strong quoted values (not weak, wildcard or comma-separated tags).
Dates must be exact valid `YYYY-MM-DD`; D1 must differ from original D0.

Requires an **already saved separate TasksWrite profile** (`tasks` + `openid`),
not readonly `personal` or Calendar. The probe never creates a profile, logs in,
reads a client file, discovers credentials, or falls back to an environment token.
Refresh may persist token rotation before UserInfo; native Keychain UI is possible.
Separate local records do not prove independent Google-side grants. There are no
provider-endpoint overrides, queue/handoff inputs, list fallback or automatic task
selection, creation or deletion. HTTPS endpoints are fixed; proxy, redirects and
retries are disabled. Only the literal loopback test transport is test-configurable.

The user must supply an **already dated, supported open, title-only, top-level
disposable task**, preferably outside the capture list to avoid accidental import.
The acknowledgement flag does not establish disposability. The real inbound pilot
`Verify scheduled phone capture` is explicitly **not used**. Before the live session,
no task/profile/client file had been selected; that earlier code-only scope is
preserved in the historical release checkpoint, not the current status.

### Live evidence — 2026-11-04

After explicit browser-sign-in and date-change authorizations, a separate
`tasks-write-personal` (`tasks` + `openid`) grant was saved. The user nominated
`Hive conditional write test` in `My Tasks`, due tomorrow (API date **Nov 5**).
`inspect-01` stopped after GET, without PATCH, because of output-only `links: []`.
Shared validation now accepts empty links arrays only; malformed/nonempty links
and unknown fields still refuse. The regression and updated probe fixture passed.

`inspect-02` succeeded, then `run-01` freshly authenticated: baseline GET 200,
current-ETag due-only PATCH Nov 5→Nov 6 **200**, changed GET 200/new ETag,
stale-old-ETag PATCH back to Nov 5 **412 with valid Google error JSON**, final GET
200 still Nov 6/same new ETag/non-due fields unchanged. Separate read-only
`verify-01` independently confirmed Nov 6/same new ETag. Two PATCHes attempted,
one accepted; no replay/rollback/delete. The disposable task remains open Nov 6.

Private journals remain at
`~/Library/Application Support/Hive/tasks-conditional-probe-20261104/{inspect-01,inspect-02,run-01,verify-01}.jsonl`;
do not copy contents, IDs, tags or digests into docs/logs. These unsigned local
audits are not production canonical ACKs. Readonly `personal` lists succeeded
after login and after testing. No real phone capture, canonical notes, worker,
queue/baseline, source refs or workflows were changed by the probe work. The new
profile was intentionally saved; no code commit/push/install occurred. This is
one-target/session evidence, not a universal guarantee or hosting activation.
See [post-fix validation and next gates](release.md#live-tasks-conditional-proof-2026-11-04).

## Data Flow

1. Validate arguments and exclusively create the journal; sync frozen target and
   approval binding before native setup. Refresh the saved grant and verify fresh
   UserInfo against its retained identity and the expected account namespace.
   The **same bearer** is then used for all Tasks requests.
2. Exact baseline GET must return 200, the exact ID, supported shape, strong E0,
   and an already present midnight UTC due (`Z` or `.000Z`). If an HTTP ETag is
   supplied it must equal the resource tag. Inspect returns private unsigned review
   evidence only; run also requires the approved E0, D0 and baseline digest to match.
3. Current-E0 `If-Match` PATCH sends only `{"due":"D1T00:00:00.000Z"}`. Validate
   its 200 resource, then exact GET must confirm D1 and E1 != E0, matching the PATCH
   tag. All raw fields except `due`, `etag` and validated `updated` must be unchanged.
4. Stale-E0 `If-Match` PATCH attempts D0, **actually different from current D1**.
   Require HTTP 412 **and valid Google JSON error envelope with `error.code: 412`**.
   An empty or malformed 412 body stops; it is not success evidence.
5. Final exact GET must retain D1/E1 and unchanged non-due fields. Only then return
   `matchingAcceptedAndStaleRejected`. Success leaves chosen D1 in place.

At most **two due-only PATCHes**: never a third, rollback, reset, retry, restore,
delete or create. Every stale 2xx is failure, even if state appears unchanged; if
Google ignores the stale condition, the task can return to D0. Other statuses,
timeout, malformed responses or drift cannot establish successful conditional
behavior (errors may be classified `unsupported`, `inconclusive` or `failed`).

### Approval and evidence gate

- Obtain separate explicit authorization **before inspect**: it is a live
  authenticated read/refresh, not an offline preview.
- Privately review the exact account/list/task, E0, D0, chosen D1 and full baseline
  SHA-256. Obtain separate approval **before run for both mutations**, including
  the stale attempt back to D0 and the no-restoration outcome. A changed baseline
  requires renewed review, not a bypass or replay.
- Keep private journals/reports locally; do not unnecessarily print IDs, titles or
  journal contents in chat/CI, or automatically upload them. Record sanitized
  outcome and approval separately from credentials. Missing outcome means
  **UNKNOWN**, not safe-to-replay. Stop and review manually after any failure.
- `baselineSha256` hashes canonical compact JSON of the full parsed task resource,
  retaining absent/present distinctions; it is not a raw HTTP-byte digest,
  authentication signature or authorization token. Reports carry private
  `reviewResource`, binding, original due/tag and digest; successful runs add the
  final tag, chosen date and immutable origin-derived observation flag.
- `portableEvidenceAuthenticated`, `executionCapability` and `productionReady`
  remain false; `replayAllowed` is false. A loopback result is labeled test-only,
  never Google success. Even a separately approved successful Google experiment
  gives limited evidence for that target/session, not production authority,
   universal provider guarantees or hosted readiness. The live session above
   satisfies this limited experiment, not production authority.

## Edge Cases

- Journal path must be absolute with an existing canonical parent and no final
  symlink. Creation is exclusive, mode `0600`, no-follow/close-on-exec, with a
  nonblocking invocation-long lock and explicit unlock on drop. JSONL append and
  `sync_all` bracket each OAuth refresh/UserInfo and Tasks GET/PATCH with attempted
  and outcome records; failed append/sync stops further requests. An attempted
  request without durable outcome is UNKNOWN and never replayable.
- `resultCandidate` is explicitly **not publication**: its sync can exceed the
  budget and prevent returning success. Journals are process-crash evidence only,
  not a power-loss guarantee or a resumable production ledger.
- Resource/report/record limits are 64 KiB (serialized reports have additional
  headroom); total journal limit is 1 MiB. The 120-second admission/network budget
  is **not a hard wall-clock bound on Keychain UI or filesystem sync**. Remaining
  time is checked after attempt sync, operation/outcome sync and result sync;
  Tasks requests clamp to 10 seconds and OAuth requests to 30 seconds or remaining
  budget, whichever is smaller. No request is admitted using a pre-sync sample.
- Additive `GoogleOAuth::with_request_timeout(Duration) -> Result<Self>` clones the
  fixed-endpoint transport with a reduced nonzero budget starting at clone time;
  it cannot extend an existing clone's deadline. The original transport and
  ordinary 30-second behavior are unaffected.
- Unknown/unsupported task fields, missing mandatory strings, invalid metadata,
  hidden/deleted/completed/subtask/notes/assignment shapes and changed unrelated
  fields stop. Output-only `links: []` is supported; malformed or nonempty links
  still refuse. This experiment's already-dated D0→D1 test is **not** permission
  to reschedule a production pilot task; first-due production policy stays unchanged.

## Example usage

**Placeholders only; not executed by this docs pass, not activation instructions.** Build only (not
install):

```sh
cargo build --locked --offline -p hive --features tasks-conditional-probe
```

The shared `target/debug/hive` reflects the last feature/default build. Prefer the
explicit feature-selecting `cargo run` form below when a build is needed, **only
after the relevant live-operation authorization**. Every journal path must be new;
use actual independently reviewed values, not these placeholders.

```sh
cargo run --locked --offline -p hive --features tasks-conditional-probe -- tasks conditional-probe inspect \
  --connection NAME --expected-account-key google-sub-v1:ACCOUNT_64_LOWER_HEX \
  --list-id EXACT_LIST_ID --task-id EXACT_TASK_ID \
  --journal /ABS/private/NEW-inspect.jsonl

# Separate approval for BOTH mutations required before this invocation.
cargo run --locked --offline -p hive --features tasks-conditional-probe -- tasks conditional-probe run \
  --connection NAME --expected-account-key google-sub-v1:ACCOUNT_64_LOWER_HEX \
  --list-id EXACT_LIST_ID --task-id EXACT_TASK_ID \
  --journal /ABS/private/NEW-run.jsonl \
  --expected-etag '"EXACT_E0"' --expected-original-due 2026-11-10 \
  --chosen-due 2026-11-11 --expected-baseline-sha256 APPROVED_64_LOWER_HEX \
  --acknowledge-disposable
```

## Related files

**Next gate:** the separate default-enabled [real local publisher](tasks-local-publisher.md)
is now implemented/reviewed **with validation limits**, not activated. The probe's
limited live proof remains true, not permission for a canonical task apply. No live
publisher/native/provider operations ran in that implementation step and existing
credentials persist. Next are final validation reconciliation, hosted credential
backend/routing, source-main authorization, remote Git CAS/ownership coordination,
workflow cost/Linux gates and separate local-pilot/activation approval. See the
[new checkpoint](release.md#tasks-local-publisher-checkpoint-2026-11-04).

- [Limited live proof and post-fix validation](release.md#live-tasks-conditional-proof-2026-11-04)
- [Nov 4 validation and remaining approval gates](release.md#tasks-conditional-probe-checkpoint-2026-11-04)
- [Writeback stages](google-tasks-writeback.md#staged-activation-plan), [still-fake executor](tasks-first-due-fake-executor.md), [auth isolation](google-auth.md#separate-tasks-write-authorization-local-only)
- [Feature declaration](../crates/hive/Cargo.toml), [CLI caller](../crates/hive/src/tasks.rs)
- [Probe flow](../crates/hive/src/tasks_probe.rs), [HTTP](../crates/hive/src/tasks_probe_http.rs), [journal](../crates/hive/src/tasks_probe_journal.rs)
- [Shared task validation](../crates/hive-capture/src/handoff.rs), [OAuth reduced-timeout API](../crates/hive-google-auth/src/google.rs), [refresh/identity orchestration](../crates/hive-google-auth/src/provider.rs)
- [Probe tests](../crates/hive/src/tasks_probe_tests.rs), [feature/default CLI tests](../crates/hive/tests/tasks_conditional_probe.rs)
