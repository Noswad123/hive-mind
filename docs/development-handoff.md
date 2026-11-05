# Development handoff — Mac to CachyOS (2026-11-05)

## Overview

Continue Hive development on CachyOS, then retire **only the Mac Hive checkout**
after verifying the push. Baseline: `b19020b` (`more proof and task testing`),
initially clean on `main`, matching GitHub `origin/main`; prior work was committed.
The coordinator supplies the final pushed handoff commit hash. This handoff is
documentation/preflight tooling, **not Linux validation or integration activation**.

The accepted target remains: canonical Markdown edit → commit **and PUSH** private
notes `main` → hosted workflow reads that **exact pushed commit** → first due-only
update of the **same existing Google Task**. Calendar is deferred. Remaining work
includes production code, not merely deployment.

## API / Interfaces

| Present interface | Actual boundary |
| --- | --- |
| `tasks fake …` | Synthetic executor and recovery, not provider evidence. |
| `tasks publish init/inspect/prepare/apply/recover` | Real local single-target publisher with explicit prepare/apply/recover; not automatically activated or hosted. |
| `tasks hosted check/plan` | Production read-only foundation: pinned source/parser and immutable injected TasksWrite record. Check has no HTTP; plan refreshes, verifies identity and GETs the exact task. Push claims alone are not authentication. |
| `tasks hosted verify-main` | Production read-only GitHub identity/current-main verifier; no Google record read and no write authority. |
| Hosted remote Git CAS and coordinator | **Entirely `cfg(test)`**, not production-compiled writer infrastructure. Temporary bare Git repositories and synthetic HTTP prove laboratory behavior only. |

There is no production GitHub HTTPS write-credential bridge, production coordinator
constructor/writer wiring, or activated worker workflow. The immutable hosted auth
store cannot persist rotated credentials: a different refresh token pauses before
UserInfo/task GET with `auth_action_required`; the old injected grant may already be
spent. No automatic secret update or repair exists.

## Data Flow

Next engineer's order:

1. Follow [Linux setup](development-linux.md): verify CachyOS compilation and
   credential-free fixtures **first**. Fix/review any OS gating needed; do not assume
   Linux works from Mac tests. Record failures as well as passes.
2. Implement/review production HTTPS Git credential delivery with process isolation:
   no secrets in URLs, argv, Git config or logs.
3. Review independently trusted inception/root pins and a **quiescent** local-to-hosted
   handoff, then production coordinator construction and writer wiring, including
   auth pause and unknown-outcome recovery. Test-only constructors are not deployment
   switches, and local CAS alone does not authorize remote dispatch.
4. Only after those code gates: pinned workflow and secret provisioning, effective
   cost hard stop, pinned Linux validation, and separate explicit activation approval.
   Do not trigger new real-task writes from Linux without approval.

Retirement gate: push the focused handoff, verify the exact remote commit and
required private preservation, and only then remove the Mac Hive checkout.

## Edge Cases

### Evidence is scoped, not a portability or production claim

- Limited live Google proof used only a user-approved disposable task in My Tasks:
  Nov 5 → Nov 6 due-only update accepted HTTP 200; stale old-ETag attempt rejected
  HTTP 412; final read retained Nov 6. It is not production writeback proof or
  authorization for another write.
- The real `Verify scheduled phone capture` note has due Nov 10; it has **not** been
  published back to its Google Task. Do not use it as a Linux smoke-test target.
- Prior verifier testing made no real GitHub API call. No real Google/GitHub
  credential environment is needed for the ordinary Linux fixtures.
- Reported prior Mac validation: 79 task tests; 78 auth tests at the earlier
  foundation checkpoint; source 31 passed / 2 ignored; probe 36; CLI 6. These are
  separate historical checkpoints, not fresh tests of this handoff or one full-suite
  result. Full current workspace validation is not established; earlier intermittent
  failures/timeouts remain unconfirmed/unexplained. **Linux remains UNTESTED.**
- Handoff-only validation on the Mac: seven preflight tests passed (including
  isolated mocked Linux tools), shell syntax/help, Rust formatting, workspace
  `cargo check --locked --workspace`, and whitespace checks passed. These do not
  establish Linux compilation or runtime behavior.

### Retire a checkout, not the working Mac environment

Keep the existing Mac capture worker running as configured. Its installed
`~/.local/bin/hive` and `mw`, working directory `~/Projects/mind-weaver`, notes at
`~/Projects/darkness`, queue, keys, LaunchAgent and application state are outside
the Hive checkout. **Do not remove the MindWeaver checkout, notes, queue,
LaunchAgents or installed binaries. Do not activate a second capture worker.**

MindWeaver's known checkpoint is `7df7cc3`, with a documentation-only dirty change
left on the Mac. Do not edit or push that sibling repository as part of this handoff.
Real integration can later use a separately trusted MindWeaver clone/build; ordinary
Hive builds/fixtures do not require it or the private notes repository.

The ignored root OAuth client file was copied and byte-verified outside the Hive
checkout under `~/Library/Application Support/Hive/private-oauth-clients/`, with
directory mode `0700` and file mode `0600`, before retirement. The saved Mac
TasksWrite profile stays in native storage and has not been exported. Never put
tokens/client files in Git, logs or artifacts, or blindly copy credentials to Linux. Linux development
starts without user secrets. Keychain/LaunchAgent behavior is not a Linux contract.

Local publisher bindings use absolute paths and exact parser hashes; cloning cannot
resume them. A trusted Linux parser needs its own reviewed binary SHA-256, never the
Mac artifact's digest. Missing state must not silently initialize or overwrite a
production authority. Follow the active runbooks rather than historical Calendar
stories; those stories are preserved as historical evidence.

## Example usage

The [Linux guide's ordered commands](development-linux.md#example-usage) are the
credential-free starting point. A useful validation handback records:

```text
Hive commit: <exact checked-out commit; coordinator supplies pushed handoff SHA>
OS / architecture: <CachyOS release and architecture>
/usr/bin/git version: <must be >= 2.50>
rustc / cargo versions and toolchain selection: <actual values>
Commands and results: <build, fixture pass/fail/ignored counts, retained failures>
Live integration: not activated; no user credentials provisioned
```

This is a reporting template, not a claim that any Linux command has run.

## Related files

- [Linux setup](development-linux.md), [accepted Tasks writeback plan](google-tasks-writeback.md)
- Active runbooks: [local publisher](tasks-local-publisher.md),
  [hosted foundation](tasks-hosted-foundation.md),
  [remote coordination and remaining code gates](tasks-remote-coordination.md),
  [conditional probe](tasks-conditional-probe.md), [capture worker](google-tasks-capture.md)
- [Hosted interfaces/types](../crates/hive-tasks/src/hosted.rs),
  [GitHub GET verifier](../crates/hive-tasks/src/hosted_github.rs),
  test-only [remote CAS](../crates/hive-tasks/src/hosted_remote.rs) and
  [coordinator](../crates/hive-tasks/src/hosted_coordinator.rs)
- [Immutable auth store](../crates/hive-google-auth/src/hosted.rs),
  [pinned task reader](../crates/hive-calendar/src/task_snapshot.rs),
  [source isolation](../crates/hive-calendar/src/source.rs),
  [CLI caller](../crates/hive/src/tasks_hosted.rs)
- [Validation checkpoints](release.md), [current implementation checklist](../plan.md)
