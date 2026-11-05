# Tasks local first-due publisher

## Overview

**2026-11-04: implemented and reviewed, with validation limits; not ready for live
activation.** `hive tasks publish` is a real, local, single-target Google Tasks
publisher, enabled in default builds (unlike the opt-in conditional probe). It is
built-debug, not installed or activated. No local publisher installation was
created, user handoff exported, or canonical note changed during this implementation;
no live init/prepare/apply/recover, native credential or provider operations ran.
The earlier separately authorized [disposable live proof](tasks-conditional-probe.md#live-evidence--2026-11-04)
and saved `tasks-write-personal` credentials remain valid historical evidence.
That approval does **not** authorize applying a due date to a canonical real task.

This is not the accepted eventual hosted push workflow. Local `main` is not proof
of a GitHub push. Manual local pilot operations need separate user approval, and
the known canonical task/date must not be automatically applied. See
[validation evidence](release.md#tasks-local-publisher-checkpoint-2026-11-04) and
[remaining activation gates](google-tasks-writeback.md#staged-activation-plan).

## API / Interfaces

| Command | Required flags | Side effects |
| --- | --- | --- |
| `tasks publish init` | `--state ABS_NEW --repo ABS_NOTES --mw-bin ABS_TRUSTED --connection NAME --handoff ABS_REVIEWED` | Local only: creates a new dedicated bare Git repository, binding, lock and initial state ref; reads source identity/parser/handoff. No auth or Google. Existing destination refuses. |
| `tasks publish inspect` | `--state ABS` | Local locked state inspection; no auth, Google or history commit. |
| `tasks publish prepare` | `--state ABS --commit FULL_LOWER_SHA` | Local preflight, live TasksWrite refresh/UserInfo/exact GET, then local frozen-intent CAS. Credential rotation may persist. No PATCH. |
| `tasks publish apply` | `--state ABS --operation-id HEX --expected-state-oid PREPARED_OID --expected-intent-sha256 REVIEWED --acknowledge-live-write` | Rechecks source/review and fresh authenticated resource; local Attempting CAS, at most one conditional due-only PATCH, evidence/confirmation/ACK commits. |
| `tasks publish recover` | `--state ABS --operation-id HEX` | Local phase recovery; only Attempting/Uncertain need auth and exact GET. Never PATCHes or reads current source/parser. |

Paths for source, state and parser must be canonical absolute paths; handoff is an
absolute regular no-follow JSON file, at most 64 KiB. Source is a complete isolated
Git worktree with its own `.git` directory and HEAD on `refs/heads/main`. Use Git
2.50+ at `/usr/bin/git` and a trusted standalone `mw` supporting
`todos snapshot --json`. Input isolation is not an OS sandbox for a hostile binary.
Commit must be the full lowercase 40- or 64-hex current local main tip. Operation
and intent hashes are 64 lowercase hex; the prepared state OID is 40 lowercase hex.

The fixed ref is `refs/heads/hive-tasks-publisher-state`, containing `state.json`.
An installation binding **outside semantic Git state** pins canonical source/state/
MW paths, parser hash, connection, installation ID and Google-versus-loopback mode.
The inception record and durable test-origin floor reject cross-opening or promoting
fake/loopback state. Portable semantic intent contains no absolute installation
paths; its digest is review evidence, not hosted dispatch authority or a signature.
No rebind, adopt, clone-and-resume, force, reset or resolve interface exists.

Reports use `kind: "google-tasks-publisher-report"`, with `stateOid`, `operationId`,
`intentSha256`, `phase` and private operations/evidence. `localOnly` is true;
`hostedReady`, `publicationReady` and `pushVerified` are false. `remoteVerified`
describes authenticated evidence in that invocation, not portable JSON authority.
`hiveWriteProven` requires Google Applied plus authenticated evidence now; offline
inspect does not assert a fresh remote check. Retained validated PATCH evidence is
reported separately from recovery attribution. Keep reports/state private; no
automatic CI artifact uploads, public logs or Atlassian uploads.

## Data Flow

1. Init binds one reviewed receipt-backed, explicitly accepted **current undated**
   handoff, not a fabricated original capture baseline. No title matching, list
   search/fallback, bearer-token environment input or provider endpoint override.
2. Prepare collects exact committed source through an exact hashed private parser
   copy and shared first-due policy. Local main and parser hashes are checked before
   and after collection and again during apply. Literal marker/due, full source
   evidence and handoff are recollected/compared; saved dirty notes are invisible.
3. Fresh TasksWrite refresh and UserInfo must match the retained account and exact
   handoff tuple. The same bearer reads only that list/task. Prepare freezes the
   full parsed raw resource SHA-256 and strong ETag, source/parser/handoff evidence,
   nonce and due-only intent. Review the **persisted** Prepared OID and intent hash.
4. Apply requires those exact reviewed values, explicit live-write acknowledgement,
   unchanged source and a fresh GET matching the frozen full-resource digest and
   ETag. Digest means compact parsed JSON, not raw HTTP bytes. Durable Prepared and
   then successful exact-old-OID Attempting CAS precede the private move-only permit.
   Only its creating invocation can consume it for one `If-Match` PATCH of `due`.
5. A validated 200 PATCH resource (new ETag, desired due, unchanged non-due fields)
   is persisted **before** confirmation GET. Applied requires that response proof,
   matching GET including ETag, and durable ACK. API mutation and Git are not atomic.

Shared policy permits only an open supported existing task's first due date, encoded
`YYYY-MM-DDT00:00:00.000Z` (date-only, not an appointment). No create/delete,
reschedule, clear, status/completion, title/notes update or nonempty links. Unknown
or unsupported fields refuse; output-only `links: []` is supported. Response and
recovery comparisons preserve all raw non-due fields except ETag and a validated
volatile `updated` timestamp.

## Edge Cases

| State / evidence | Handling |
| --- | --- |
| Prepared recovery | Durably becomes NotDispatched, without auth; a later new prepare is possible. |
| Valid Google JSON 412 | RejectedConflict, no retry; later new preparation requires fresh review. Empty/malformed 412 is not proven rejection. |
| Timeout, lost/malformed mutation response, failed confirmation/ACK | Uncertain or retained Attempting; never automatically resend. Use recovery, not apply replay. |
| Attempting/Uncertain GET shows desired due and unchanged non-due fields | RecoveredSatisfied with **UNKNOWN attribution**, even if validated PATCH facts were separately retained. Not Applied. |
| Recovery transport/non-200/invalid or untrusted observation | Remains Uncertain so later GET can establish facts. An unchanged undated baseline also stays Uncertain. |
| Valid supported recovery resource shows actual semantic conflict | ConflictNeedsReview; blocks new operations. |
| AlreadySatisfied, Applied, RecoveredSatisfied | Closes the target's first-due lane; matching due alone never fabricates Hive mutation proof. |

Repeated same-phase recovery adds no history commit. Pending/uncertain/conflict or
closed targets cannot be bypassed with a new operation. Recovery uses frozen
intent only; unavailable source/parser does not prevent it. Preserve the installation
and evidence; do not edit state, reset refs or clone it to resume.

State is bounded to 1 MiB and 128 operations. Prepared admission caps at **768 KiB**,
reserving **256 KiB** for post-write PATCH, confirmation and recovery evidence.
That reserve is not retroactively imposed on Attempting/Uncertain recovery.
Typed JSON rejects unknown fields and malformed inputs; resources cap at 64 KiB.
Sanitized bounded Git plumbing disables hooks/signing/network helpers and explicitly
requests object/ref fsync. These are **process-crash** guarantees only, not proven
host/power-loss durability or exactly-once delivery.

**Administration assumption:** exclusively and cooperatively administer this local
installation during operations. Invocation-long nonblocking locks and CAS are local,
not a distributed fence. Source and state repositories have a small cross-repository
race; source checks and dispatch are not atomic. No protection against a malicious
administrator rewriting the entire installation/history is claimed.

The 120-second admission/network budget is not a hard wall clock for native Keychain
UI or fsync. Tasks requests clamp to 10 seconds (connect timeout 10 seconds), OAuth
to 30 seconds or remaining budget. Admission is rechecked after Attempting CAS,
before PATCH; no proxy, redirects or automatic retries. No production deadlines
were relaxed for tests.

## Example usage

**Placeholders only — NEVER run these now.** These are independent interface
examples, not approval or an automatic pipeline. First obtain separate approval
for local setup/live reads and a canonical real-task apply; disposable-task consent
does not transfer. Use independently reviewed persisted Prepared values, never pipe
a freshly computed hash into an unreviewed write.

```sh
hive tasks publish init --state /ABS/publisher.git --repo /ABS/notes \
  --mw-bin /ABS/trusted/mw --connection NAME --handoff /ABS/reviewed-handoff.json
hive tasks publish inspect --state /ABS/publisher.git
hive tasks publish prepare --state /ABS/publisher.git --commit FULL_LOWER_SHA
hive tasks publish apply --state /ABS/publisher.git --operation-id OPERATION_64_LOWER_HEX \
  --expected-state-oid REVIEWED_PREPARED_OID \
  --expected-intent-sha256 REVIEWED_64_LOWER_HEX --acknowledge-live-write
hive tasks publish recover --state /ABS/publisher.git --operation-id OPERATION_64_LOWER_HEX
```

`hive` above denotes the reviewed built-debug binary, not an installed upgrade.
Init's destination must be new; subsequent examples refer to that same installation.
Inspect/recover after ambiguous apply, never automatically retry the write. No
workflow, main push, commit or install/activation is requested here.

## Related files

**Current next gate:** the separate [hosted read-only foundation](tasks-hosted-foundation.md)
is now implemented/reviewed; it does not invoke this publisher's write path or make
local state adoptable/portable. Next are trusted current remote-main verification,
shared remote expected-OID CAS/dispatch ownership and local-to-hosted handoff,
durable intent/outcome/ACK recovery with credential pauses, then pinned trusted
workflow/cost hard stop/explicit activation and later Linux validation. Manual
local pilot and canonical apply still require separate approval. No workflow or
secret provisioning was executed. See the
[current checkpoint](release.md#tasks-hosted-read-only-foundation-checkpoint-2026-11-04).

- [Publisher/state machine](../crates/hive-tasks/src/publisher.rs), [HTTP](../crates/hive-tasks/src/publisher_http.rs), [storage](../crates/hive-tasks/src/storage.rs)
- [CLI/flags](../crates/hive/src/tasks_publish.rs), [default routing](../crates/hive/src/tasks.rs)
- [Publisher tests](../crates/hive-tasks/src/publisher_tests.rs), [CLI tests](../crates/hive/tests/tasks_publish.rs)
- [Shared policy](../crates/hive-tasks/src/policy.rs), [handoff](../crates/hive-capture/src/handoff.rs), [committed snapshot](../crates/hive-calendar/src/task_snapshot.rs)
- [Auth isolation](google-auth.md#separate-tasks-write-authorization-local-only), [writeback stages](google-tasks-writeback.md#staged-activation-plan)
- [Nov 4 checkpoint and validation limits](release.md#tasks-local-publisher-checkpoint-2026-11-04)
