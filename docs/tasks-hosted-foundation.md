# Tasks hosted read-only foundation

## Overview

**2026-11-04 current update: implemented and reviewed; read-only, not
automatic notes-main publishing.** Default builds expose `hive tasks hosted check`,
`plan` and now `verify-main`. None activates a workflow, changes a task, commits state, or acquires
dispatch authority. This read-only lane does not invoke the local publisher's live
write path; it reuses a GET-only facade over its fixed transport/resource validation.
Prior local publisher, disposable-probe and live-proof evidence remains separate.

The binding and executable must come from independently trusted provisioning.
Changing a configuration file in a notes working tree on push does not establish
that trust. Runner environment/event data are **spoofable claims**, not GitHub
authentication, proof of a push, or verification of current remote `main`.

## API / Interfaces

All three commands require exactly `--binding ABS --repo ABS --mw-bin ABS --commit FULL_SHA`.
The commit is a full lowercase, nonzero 40- or 64-hex supported Git object ID.

| Interface | Behavior |
| --- | --- |
| `tasks hosted check` | Admit consistent claims, validate committed-source policy and exact private parser pin, then inspect credential structure. No HTTP client, refresh, UserInfo, or native storage. Success does not prove grant validity. |
| `tasks hosted plan` | Same preflight, then one refresh, fresh UserInfo and same-bearer exact bound task GET; emit only a redacted decision summary. Provider read-only, but refresh can spend the old refresh token. |
| `tasks hosted verify-main` | Same claims/source/private-copy-pin preflight, then late `GITHUB_TOKEN` lookup and fixed GitHub private-repository identity/current-main GETs only. No Google credential read or native auth. |
| `hosted::{check, plan}(Inputs, PushClaims, secret_callback)` | Library inputs are binding/repo/parser paths and commit; the one-shot callback returns `Zeroizing<String>` only after local preflight. No endpoint/write callback is accepted. |
| `hosted::verify_main(Inputs, PushClaims, token_callback)` | One-shot zeroizing GitHub token callback after preflight; redacted current-main observation, never dispatch authority. |
| `ImmutableTasksWriteStore::from_record_bytes(connection, account_namespace, bytes)` | Decode the existing strict native V1 credential codec (64 KiB), require a live TasksWrite grant and independently pinned alias/account. No file, environment or native lookup inside this backend. |

Check/plan read only the fixed Google credential variable `HIVE_HOSTED_TASKS_WRITE_RECORD`;
verify-main reads only `GITHUB_TOKEN`, never that Google record. No generic bearer
fallback exists. No hosted apply/PATCH, state commit, adoption, native fallback,
portable observation input, dynamic endpoint, or credential-export API exists.

### Binding V1

An absolute regular no-follow file, bounded to **128 KiB**, rejects unknown fields,
recursive duplicate JSON keys and trailing JSON. Exact camelCase fields:

- `schemaVersion`: string `"1"`; `kind`: `"google-tasks-hosted-binding"`.
- `expectedRepositoryId`: canonical positive numeric **string** fitting `u64`, no
  leading zero; `expectedRepository`: `owner/name`.
- `sourceRepoId`: opaque source namespace matching the handoff.
- `connection`: validated credential alias.
- `expectedAccountNamespace`: `google-sub-v1:` plus 64 lowercase hex, matching handoff
  `accountKey` and the credential identity.
- `trustedParserSha256`: 64 lowercase hex for the trusted executable artifact.
- `reviewedTaskHandoff`: full validated [TaskHandoff](../crates/hive-capture/src/handoff.rs),
  including receipt evidence and explicitly accepted current **undated** baseline.
- `handoffSha256`: lowercase SHA-256 of the **full decoded TaskHandoff**, converted
  to recursively sorted `serde_json::Value` and compact JSON. Defaults matter:

  ```rust
  format!("{:x}", Sha256::digest(serde_json::to_vec(&serde_json::to_value(h)?)?))
  ```

This is neither a hash of original handoff-file bytes nor the local publisher's
struct-serialization digest. It is review metadata, not authentication. Concrete
list/task IDs must pass exact-ID validation; `@default` and IDs containing `.` refuse.
Use the schema/fixture cross-references rather than inventing IDs or secret files.

### Job claims

Required values are `GITHUB_ACTIONS=true`, `GITHUB_EVENT_NAME=push`,
`GITHUB_REF=refs/heads/main`, `GITHUB_SHA` exactly equal to `--commit`, and
`GITHUB_REPOSITORY`/`GITHUB_REPOSITORY_ID` exactly equal to the binding pins.
`GITHUB_EVENT_PATH` is captured once as an absolute no-follow regular file, bounded
to **1 MiB**; FIFO/nonregular inputs refuse without blocking. Recursive duplicate
keys refuse even in unknown fields; normal unknown event fields are allowed.
Payload `ref`, `deleted: false`, `after`, numeric `repository.id`, and
`repository.full_name` must agree with those claims and pins. The private
`AdmittedPush` is check-only, never exported or execution authority.

## Data Flow

1. Capture/validate binding and push claims. Require a complete non-shallow checkout
   with its own `.git` directory and no object alternates. Exact detached HEAD is
   accepted when it equals the selected SHA; no local branch-name proof is required.
2. Read committed Markdown, not dirty notes, using Git 2.50+ and a trusted standalone
   `mw` supporting `todos snapshot --json`. `read_committed_tasks_pinned` passes the
   expected digest into the shared source reader: it compares the **exact private
   executable copy before source-content reads or execution**, not just a pathname
   hash checked after execution. Full shared source policy validates handoff,
   unambiguous stable marker and literal valid due date against the baseline before
   invoking the secret callback.
3. Decode the injected record late. Check stops after structure and unchanged-source
   checks; its success decision is `set_first_due`, not a remote observation.
4. Plan rechecks HEAD/parser around authentication; changed source before GET stops
   collection. Refresh verifies fresh UserInfo against the retained identity and
   account pin, then reads only the bound list/task. No list search or fallback.
5. Validate the exact resource and evaluate the shared first-due policy. Summary
   decision is `set_first_due`, `already_satisfied` or `blocked`; recheck source
   before returning. Invalid transport/resource/auth inputs fail rather than
    fabricate an observation. A 120-second admission/network budget controls requests,
    not a hard wall-clock bound on filesystem work.

Private materialization, cleared child environment and private home/config provide
**input isolation, not an OS sandbox**. Never run untrusted code with secrets. Late
application lookup does not mean the runner lacked the secret earlier: its original
environment already contains it. Only the newly read local environment copy is
`Zeroizing`; the runner's original environment remains.

Verify-main follows the same steps 1–2, then checks source after token lookup and
after fixed GitHub identity/ref GETs. See [production verifier and test-only remote
protocol](tasks-remote-coordination.md) for exact endpoint, limits, origin flags and
remaining code bridges. The remote CAS/coordinator modules are entirely `cfg(test)`,
not a dormant normal-compiled production writer.

## Edge Cases

- The immutable store lock coordinates **one process-local store instance only**,
  not separate instances, processes or workflows. Every `save`, regardless of
  arguments, returns `AuthError::CredentialPersistenceRequired`.
- An omitted or identical refresh token needs no save and can proceed. A new,
  different rotated token attempts save and fails **before UserInfo, token release
  or task GET**; hosted reports `auth_action_required`. The old injected secret may
  already be spent. Manual, separately authorized reauthorization/provisioning is
  required; there is no automatic repair or rotation upload path. Google and secret
  storage are not a transaction.
- This backend does not make GitHub secrets mutable at runtime, use a secrets API,
  or assume `GITHUB_TOKEN` can write secrets. No credentials belong in state Git,
  handoffs, source notes, logs or artifacts. No provisioning instructions are
  implied by these interfaces.
- Matching due is satisfaction, not proof Hive wrote it. Conflicting due or changed
  supported baseline fields block; no reschedule, clear, completion, create/delete
  or Calendar event is supported. Unsupported resource shapes refuse.
- Check/plan output is only `google-tasks-hosted-summary`, redacted flags and decision: no task
  titles, account/list/task IDs, source paths, due payload or full plan. Successful
  summaries have `redacted`, `secretPresent`, `structureValid`, `pushClaimsAdmitted`
  true. `remoteReadVerifiedDuringCollection` is true only after a fresh collected
  read (false for check); `googleOrigin` distinguishes the production origin.
  `currentMainVerified`, `pushVerified`, `githubVerified`,
  `portableEvidenceAuthenticated`, `publicationReady`, `conditionalWriteSafe`,
  `hiveWriteProven` and `hostedExecutionEnabled` are always false. An unsigned summary
  cannot be replayed as authority; loopback-origin tests are not Google auth proof.

## Example usage

**Interface placeholders only, not commands to run now.** These assume independently
trusted bindings/artifacts and an already authorized job context/credential supply;
no workflow or secret provisioning was executed. `hive` means the reviewed debug
build, not an installed upgrade. Plan contacts OAuth/UserInfo/Tasks and needs separate
live-read authorization; check has no HTTP/native calls but does read secret structure.

```text
hive tasks hosted check --binding /ABS/trusted/binding.json --repo /ABS/notes --mw-bin /ABS/trusted/mw --commit FULL_SHA
hive tasks hosted plan --binding /ABS/trusted/binding.json --repo /ABS/notes --mw-bin /ABS/trusted/mw --commit FULL_SHA
hive tasks hosted verify-main --binding /ABS/trusted/binding.json --repo /ABS/notes --mw-bin /ABS/trusted/mw --commit FULL_SHA
```

Verify-main requires separate live GitHub-read authorization; no live GitHub token
read or call occurred this step. It emits `google-tasks-hosted-main-summary`;
production-origin current-main verification flags are collection observations only,
with push authentication, publication, hosted execution, dispatch and portable
evidence authority false. Check/plan flags above are unchanged.

Next gates: production GitHub HTTPS write-credential delivery; reviewed inception/root
pin and quiescent local-to-hosted handoff; production coordinator/writer with auth
pause/recovery; then pinned workflow/secrets, cost hard stop, Linux runtime validation
before deployment and explicit activation. See the [new checkpoint](release.md#tasks-remote-authority-checkpoint-2026-11-04).
No workflow is added now.

## Related files

- [Hosted orchestration](../crates/hive-tasks/src/hosted.rs), [policy](../crates/hive-tasks/src/policy.rs), [GET/resource transport](../crates/hive-tasks/src/publisher_http.rs)
- [CLI and fixed secret lookup](../crates/hive/src/tasks_hosted.rs), [routing](../crates/hive/src/tasks.rs)
- [Immutable credential backend](../crates/hive-google-auth/src/hosted.rs), [strict codec](../crates/hive-google-auth/src/storage.rs), [refresh ordering](../crates/hive-google-auth/src/provider.rs)
- [Pinned task facade](../crates/hive-calendar/src/task_snapshot.rs), [shared source reader](../crates/hive-calendar/src/source.rs)
- [Hosted tests](../crates/hive-tasks/src/hosted_tests.rs), [synthetic CLI binding/claims fixture](../crates/hive/tests/tasks_hosted.rs)
- [Auth backend contract](google-auth.md#hosted-immutable-taskswrite-backend), [writeback stages](google-tasks-writeback.md#staged-activation-plan), [local publisher](tasks-local-publisher.md)
- [Validation checkpoint and remaining limits](release.md#tasks-hosted-read-only-foundation-checkpoint-2026-11-04)
