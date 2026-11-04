# Tasks first-due fake executor

## Overview

**Fake-only executor code/review milestone complete — 2026-11-04.** The built-debug,
not installed `./target/debug/hive tasks fake` exercises local durable intent,
one synthetic due-only mutation, and observation-only recovery. It does not contact
Google, use native auth, change capture queues or notes, or verify a push. A local
source commit need not have been pushed. This is not the hosted publisher.

## API / Interfaces

| Command | Inputs and effects |
| --- | --- |
| `fake init` | `--state-repo ABS --world ABS --fixture ABS`; explicitly creates a dedicated bare SHA-1 state repo, fixed `refs/heads/hive-tasks-fake-state`, and separate single-target synthetic world plus lock files. |
| `fake execute-first-due` | State/world plus source repo, full lowercase commit SHA, trusted absolute MW binary, opaque source-repo ID and handoff; freshly collects committed source, evaluates shared policy, persists intent before private fake dispatch. |
| `fake recover` | State/world plus retained operation ID; uses frozen intent without source repo/parser access and **never PATCHes**. |
| `fake inspect` | State repo only; reads retained operations without acquiring/creating lock files or accessing the world. Top-level phase/operation ID and world counters are null; inspect `operations`. |

Library exports are `initialize`, `inspect`, `FakeSession::open`,
`FakeSession::execute_first_due`, `FakeSession::recover`, and `SourceOptions`.
`hive_tasks::policy` now owns the shared first-due policy used by both executor and
advisory CLI planner. Policy candidates are not dispatch capabilities. No command
accepts or executes a JSON plan, live observation wrapper, bearer, endpoint or
production dispatch option.

### Fixture and state

The checked-in [world fixture](../crates/hive-tasks/tests/fixtures/world.json) is
synthetic, with no real Google values. Its strict initialization schema is
`schemaVersion: "1"`, `worldId`, `sourceRepoId`, `accountKey`, `listId`, and `task`.
The task must be supported, open and undated. The sample uses `synthetic-repo`,
`synthetic-list`, `synthetic-task`, and a zero-filled synthetic account namespace.
It is an **init input**, not a complete source/handoff fixture or the saved world.
Execution additionally needs a matching synthetic handoff with valid receipt
evidence and derived capture identity, plus committed Markdown with exactly one
matching literal stable marker and valid due. Do not substitute live capture data.

Saved world JSON contains its bound manifest, `task`, `fakeRevision` (initially 1)
and `durablePatchCount` (initially 0). State Git commits contain `state.json` with
the manifest and operations. Frozen intent retains exact source commit/blob/path/
line/literal evidence, parser digest, handoff, expected fake revision and only
`patch: {"due": "YYYY-MM-DDT00:00:00.000Z"}`. These are audit hashes, not signatures,
authentication or provider acknowledgements.

## Data Flow

1. Init refuses overwrite, existing world/lock paths and implicit adoption. State,
   world and fixture paths must be canonical absolute paths with existing parents;
   regular-file inputs refuse final symlinks. State and world manifests bind their
   exact local paths and namespace; copied/foreign state is not adopted.
2. Execute holds separate nonblocking local Unix state and world locks through
   observation, state CAS and dispatch. Competing sessions fail rather than wait.
   Explicit unlock on drop handles shared/inherited open file descriptions.
3. Fresh internal collection reads the exact committed source, not dirty notes or
   a caller-supplied snapshot. Shared policy checks handoff/target binding, unique
   stable identity, open checkbox, literal/parser agreement and one valid due date.
   Meaningful remote baseline changes, unsupported state or conflicting due refuse.
4. Persist `Prepared`, then `Attempting` using exact old-ref CAS. Only that
   invocation's successful Attempting CAS mints the private move-only dispatch token.
   Dispatch rechecks current ref/intent, fake revision, undated target and baseline;
   revision conflict yields `RejectedConflict` without mutation.
5. The sole fake mutation changes due, increments revision and durable patch count,
   and saves the world. Outcome persistence is **separate**: a crash or failed CAS
   can leave Attempting after the world changed. Recovery observes, never replays.

### State machine

| Retained state / observation | Result |
| --- | --- |
| Fresh eligible undated target | `Prepared → Attempting → Applied` (one fake patch), or `RejectedConflict` (none). |
| Desired due already present | `AlreadySatisfied`, without dispatch or patch. |
| Recover `Prepared` | `NotDispatched`; no dispatch token was issued. |
| Recover `Attempting` / `Uncertain`, accepted fields and desired due match | `RecoveredSatisfied`, attribution `UNKNOWN`; satisfaction does not prove who wrote it. |
| Recover attempted/uncertain, unchanged undated or missing target | `Uncertain`; blocks execution, never recreates. |
| Recover attempted/uncertain, conflicting due or meaningful field change | `ConflictNeedsReview`; blocks execution. |
| Recover other terminal state | No transition or new patch. |

Prepared, Attempting, Uncertain and ConflictNeedsReview block new execution. Applied,
RecoveredSatisfied and AlreadySatisfied targets cannot receive a later operation,
even with stale/new source. Repeated unchanged Uncertain recovery makes no history
commit. Legacy full histories of consecutive Uncertain entries can still transition
by deduplicating redundant phases before appending the terminal result. There is no
reset, retry-dispatch, clear-pending or resolve escape. NotDispatched and
RejectedConflict are not satisfaction records; fresh execution still revalidates
policy and operation identity, never resumes an old dispatch.

## Edge Cases

- First due only: no reschedule, clear, create, delete, status, title or notes writes.
  Matching due accepts the exact midnight UTC spelling with `.000Z` or `Z`, not
  arbitrary time/timezone equivalence.
- State/world JSON and Git I/O are bounded to 1 MiB; at most 128 operations and 128
  phases per operation are retained. Unknown state fields and invalid transitions
  refuse. Handoff CLI input remains bounded to 64 KiB.
- Git plumbing uses sanitized environment, bounded execution and exact old-ref CAS,
  with explicit loose-object/reference fsync settings; world replacement syncs file
  and parent. **Recovery guarantee is PROCESS-CRASH ONLY**, not host/power-loss
  durability, distributed fencing or exactly-once production execution. Local paths
  and locks are not a portable hosted coordinator; cooperative local administration
  is required. Partial initialization is not an automatic adoption/retry path.
- All reports say `executionMode: "fake-only"`, `provider: "durable-fake"`, with
  `googleConditionalWriteValidated`, `productionReady`, `publicationReady`,
  `pushVerified`, `remoteVerified`, and `hiveWriteProven` false. `fakeApplied: true`
  describes only Applied fake state; RecoveredSatisfied is false with `UNKNOWN`
  attribution. Fake success cannot be promoted into a Google ACK or production proof.
- No production dispatch trait, HTTP Google PATCH or live constructor exists here.
  Output/state contain private source evidence; do not log or auto-upload them.

## Example usage

**Not executed for this docs update.** Run only against disposable synthetic inputs.
The parent `/ABS/synthetic` must already exist at its canonical path; the state repo,
world and world lock must not exist. Init writes local Git objects/refs and files.
The sample fixture alone supports init/inspect; execution requires the matching
synthetic source and handoff described above. Source reading inherits Git 2.50+ at
`/usr/bin/git` and a trusted standalone `mw` supporting `todos snapshot --json`;
input isolation is not an OS sandbox for hostile binaries.

```sh
./target/debug/hive tasks fake init \
  --state-repo /ABS/synthetic/state.git \
  --world /ABS/synthetic/world.json \
  --fixture /ABS/local/hive-mind/crates/hive-tasks/tests/fixtures/world.json

./target/debug/hive tasks fake execute-first-due \
  --state-repo /ABS/synthetic/state.git --world /ABS/synthetic/world.json \
  --repo /ABS/synthetic/source --commit FULL_LOWER_SHA \
  --mw-bin /ABS/trusted/mw --source-repo-id synthetic-repo \
  --handoff /ABS/synthetic/handoff.json

./target/debug/hive tasks fake inspect --state-repo /ABS/synthetic/state.git

./target/debug/hive tasks fake recover \
  --state-repo /ABS/synthetic/state.git --world /ABS/synthetic/world.json \
  --operation-id RETAINED_OPERATION_ID
```

**Next gate:** separately authorize a disposable-resource live test of actual
Google Tasks conditional PATCH behavior, including proof that stale preconditions
are rejected rather than ignored. Neither fake revisions nor Calendar etags prove
this. Only then proceed to a scoped production adapter, durable intent/ACK
coordination, hosted secrets/state and the notes-`main` push workflow. No new
workflow or live-write CLI is delivered or activated now.

## Related files

- [Writeback design and activation gates](google-tasks-writeback.md)
- [Nov 4 fake-executor validation checkpoint](release.md#tasks-first-due-fake-executor-checkpoint-2026-11-04)
- [Executor and frozen state](../crates/hive-tasks/src/executor.rs), [shared policy](../crates/hive-tasks/src/policy.rs), [storage/locks/CAS](../crates/hive-tasks/src/storage.rs)
- [CLI](../crates/hive/src/tasks_fake.rs), [planner caller](../crates/hive/src/tasks.rs)
- [Executor tests](../crates/hive-tasks/src/tests.rs), [CLI tests](../crates/hive/tests/tasks.rs)
- [Handoff types](../crates/hive-capture/src/handoff.rs), [committed-source facade](../crates/hive-calendar/src/task_snapshot.rs)
