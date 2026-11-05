# Tasks remote coordination

## Overview

**2026-11-04 remote-authority code/review milestone: no remaining P1/P2 in the
reviewed scope, not production writeback.** The new normal-build interface is ONLY
read-only `hive tasks hosted verify-main`. The entire remote Git CAS transport and
integrated coordinator are `cfg(test)` laboratories, absent from normal compilation.
No GitHub HTTPS write-credential bridge, production bootstrap/local quiescent
handoff, production coordinator constructor, hosted writer command or deployed
workflow exists. Earlier [foundation](tasks-hosted-foundation.md), local publisher
and live Google probe checkpoints retain their separate scope.

## API / Interfaces

- `tasks hosted verify-main --binding ABS --repo ABS --mw-bin ABS --commit FULL_SHA`
  and `hosted::verify_main(Inputs, PushClaims, token_callback)` reuse the foundation's
  binding, job claims, committed-source policy and exact private parser-copy pin.
  Only after preflight does the CLI look up `GITHUB_TOKEN`; it never reads
  `HIVE_HOSTED_TASKS_WRITE_RECORD` or invokes Google/native auth on this route.
- Fixed production HTTPS GETs to `api.github.com`: `repos/{owner}/{name}` must return
  the pinned numeric `id`, exact `full_name` and `private: true`; then
  `repos/{owner}/{name}/git/ref/heads/main` must return `ref: refs/heads/main`,
  `object.type: commit` and the exact candidate OID. Both use the same bearer.
  There are no endpoint/token CLI overrides, redirects, proxies or retries.
  Each response is at most 64 KiB, recursively duplicate-key-free JSON with HTTP
  200; requests use a 10-second cap within the shared 120-second admission budget.
  Errors are sanitized; source HEAD/parser changes after lookup or collection stop
  success. The budget is not a hard filesystem wall-clock limit.
- Redacted `google-tasks-hosted-main-summary`: `currentMainVerified`,
  `currentMainVerifiedDuringCollection` and `githubVerified` can be true only for a
  fresh actual production API collection (`githubOrigin: true`). Loopback cannot
  claim them. `pushVerified`, `githubPushAuthenticated`, `publicationReady`,
  `hostedExecutionEnabled`, `dispatchAllowed` and `portableEvidenceAuthenticated`
  remain false. This is an observation, not persisted authority, push provenance
  or permission to dispatch. Old `check/plan` still admit claims only and do not
  verify current main.

## Data Flow

The following is **test-only protocol**, using real Git push/receive-pack against
temporary local bare repositories and synthetic OAuth/GitHub/Tasks loopback HTTP:

1. Explicit synthetic inception pins an independently retained root and namespace.
   Observe the fixed `refs/heads/hive-tasks-publisher-state` through ephemeral
   private bare staging. Validate canonical state shape, sole-parent continuity,
   root, origin/namespace, bounded history (256 commits, 16 MiB aggregate state)
   and resource limits. Missing state never automatically initializes or falls
   back to genesis.
2. Persist frozen `Prepared`, then fresh account-bound UserInfo/same-bearer exact
   task basis and owner nonce in `Attempting`, before any due-only If-Match PATCH.
   Each push uses explicit `--force-with-lease=REF:OLD`, not REST `force:false` or
   ordinary fast-forward push. The actual proposed commit must have exactly OLD
   as its sole parent.
3. Only a fresh exact porcelain acknowledgement yields a private non-Clone,
   non-Serde receipt/creator permit bound to authority/root/namespace, OLD/NEW,
   operation and invocation nonce. Up-to-date `=` and even matching readback cannot
   upgrade Unknown. Lost acknowledgements grant no dispatch; subsequent handling
   is recovery only.
4. Recheck remote main, state, committed source and parser before dispatch. Consume
   the creator permit for one strong-If-Match due-only PATCH with the retained
   bearer/basis. Persist validated response evidence before confirmation GET and
   final acknowledged `Applied` state. Lost API response, evidence push or outcome
   push is unknown, never permission to replay.
5. Recovery reads frozen intent and exact task only, not current source/parser or
   current-main eligibility. It may record `RecoveredSatisfied` (unknown causality)
   or `Conflict`; unchanged reads stay `Uncertain` without a state commit. They
   neither take over nor revoke a paused creator. No expiry/takeover exists.

## Edge Cases

- The external transport-origin floor cannot be promoted by editing serialized
  artifacts: fake/test state never becomes Google authority. The coordinator has
  no real production constructor or credential helper.
- Git test children deliberately inherit no `GITHUB_TOKEN`, proxy or helper and
  allow only local file transport. This proves isolation of the laboratory, **not
  solved HTTPS Git secret delivery**.
- Source/Git/Google remain non-atomic: rechecks cannot fence every subsequent race.
  Cooperative exclusive administration is assumed; hostile reset/admin ABA is not
  solved. No exactly-once, host-loss durability or distributed fencing claim.
- Race tests use independent workers/staging/Git processes, not independent
  coordinator OS processes. Latest targeted green results do not establish long-run
  stability; see the [validation checkpoint](release.md#tasks-remote-authority-checkpoint-2026-11-04).

### Remaining production gate

Next: implement/review GitHub HTTPS credential delivery keeping secrets out of
URLs, argv, config and logs with process isolation; independently trusted
inception/root provisioning and a quiescent local-to-hosted handoff; production
coordinator construction and writer wiring with auth pause/recovery. Only then:
pinned workflow/secrets deployment, effective cost hard stop, Linux validation
and separate activation approval. The missing code bridge is not merely deployment.

## Example usage

Interface placeholder only, **not an instruction to run or provision credentials**:

```text
hive tasks hosted verify-main --binding /ABS/trusted/binding.json --repo /ABS/notes --mw-bin /ABS/trusted/mw --commit FULL_SHA
```

Requires the independently trusted foundation binding/artifacts and admitted job
context. No actual `GITHUB_TOKEN` was read or live GitHub call executed this step;
verification tests used synthetic loopback only. Earlier live Google probe auth
and provider evidence remain historically true. This documentation pass runs no
commands, tests or native/provider calls.

## Related files

- [Foundation interfaces and binding](tasks-hosted-foundation.md)
- [Production orchestration](../crates/hive-tasks/src/hosted.rs), [GitHub GET verifier](../crates/hive-tasks/src/hosted_github.rs), [CLI](../crates/hive/src/tasks_hosted.rs)
- Test-only [remote transport](../crates/hive-tasks/src/hosted_remote.rs) and [coordinator](../crates/hive-tasks/src/hosted_coordinator.rs)
- [Remote tests](../crates/hive-tasks/src/hosted_remote_tests.rs), [coordinator/verifier tests](../crates/hive-tasks/src/hosted_coordinator_tests.rs), [CLI tests](../crates/hive/tests/tasks_hosted.rs)
- [Writeback stages](google-tasks-writeback.md#staged-activation-plan), [current checklist](../plan.md)
