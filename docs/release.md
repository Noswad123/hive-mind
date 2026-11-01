# Hive Release and Packaging

## Current release path

The canonical binary name is `hive`. For now, releases are source-built Rust
binaries plus optional GitHub release artifacts.

Recommended local install:

```bash
make install
```

This builds `crates/hive` in release mode and installs the binary to
`~/.local/bin/hive` by default.

### Local installation checkpoint: 2026-11-01

The stable release was successfully built and installed at
`/Users/jdawson/.local/bin/hive` on 2026-10-31, then rebuilt and installed at that
same path after the 2026-11-01 process-inventory fixes. The user reconfirmed latest
foreground `auth status` and `lists --connection personal` success. Latest-build
no-UI background Keychain access is positively proven while unlocked, not for
denial/locked scenarios. Earlier manual Android capture and
duplicate-free repeat proof remain intact; read-only queue confirmation still
found one delivered receipt with one writer attempt.

For foreground verification after a binary change, use that stable path, then
exercise refresh and Google access with `lists`, handling any OS prompt there.
These checks already succeeded for the current build:

```sh
/Users/jdawson/.local/bin/hive captures auth status --connection personal
/Users/jdawson/.local/bin/hive captures lists --connection personal
```

The worker is configured against the existing personal queue/list/baseline with
fixed public environment and MindWeaver cwd (interval 60 seconds, timeout 45 seconds,
limit 1); `configValid` and `queueScopeValid` are true. After clean/idle checks, the
live editor was hotloaded in place preserving `mw_ops` identity and its active
guard across four shared references, and the bridge registered. After user-authorized
orphaned ChatGPT helper cleanup, installed `editor-check` reports `ready: true`,
`noEditor: false`, `deferred: null`. No identity check was bypassed. A live unsaved
hidden read-only normal-buffer probe verified `dirty_buffers` readiness deferral,
without an on-disk file or note edits. The guard stayed unchanged, `busy: false`;
the probe was removed after changed-tick/name/visibility checks and clean readiness
restored. Full pending-job dirty-buffer delivery deferral is not live-proven.

Two controlled `run-once` cycles completed in about 0.5 seconds each: observed 1,
unchanged 1, staged/delivered 0, `authActionRequired: false`, generation 1.
Explicit background install/start then **enabled scheduled polling**:
`serviceActive: true`, `serviceState: loaded_owned`. Actual LaunchAgent startup and
next scheduled cycles completed about 60.7 seconds apart, with no pending work,
observed 1 unchanged, zero staged/delivered, and no auth action required. Native
LaunchAgent ownership/status parsing is live-verified. The existing receipt is
still attempt 1; no duplicates or Inbox/dashboard hash, byte, or mtime changes.
Earlier 45-second unknown outcome and 12-second local status stall did not reproduce;
inherited/minimal local diagnostics succeeded in 0.054/0.028 seconds. The same
build/credentials work; the credential-operation failure is resolved operationally,
with no established root cause or speculative source fix.

See the [background activation runbook](google-tasks-capture.md#background-activation-enabled-and-verified)
for cycle evidence and stable-path `background status`, `stop`, `start`, and
`uninstall` controls. Stop retains config/queue; uninstall removes only the owned
plist and retains all state. Polling runs in the GUI user session at a 60-second
interval, 45-second timeout, limit 1. Google holds work while the computer is off;
no server is required. New phone-task-to-background-source-write delivery and live
computer-off catch-up are not yet live-verified; source-write/guarded delivery has
isolated integration and earlier manual capture proof only. `background install`
writes the owned plist only; `background start` loads it with `RunAtLoad`.
Neither `make install` nor building the binary activates the worker. Calendar,
export, completion writeback, lifelog, and PWA migration remain pending.

Latest validation after the inventory fixes: 270 normal workspace tests passed
with five ignored; strict all-targets Clippy passed for background/auth/capture;
both ignored isolated real-Neovim tests passed. The narrowly scoped SDK-typed C
`sysctl` zombie verification and checked 32-bit Darwin UID normalization do not
ignore generic inventory errors or change the group-death guard; review found no
P1/P2 issues. The inherited-FD test was isolated in a single-test child to avoid
parallel fork-test leakage, retaining original assertions and production semantics.
Both real-writer integrations passed earlier, not rerun after these fixes. The
fifth ignored crash fixture is exercised by normal recovery tests. Earlier format
and Neovim background/safety checks (32/54 tests) passed on 2026-10-31. None of these
checks alone proves live scheduled operation or computer-off catch-up. Scheduled
polling has separate live evidence above. No additional tests ran during activation;
this docs-only update changes no source code and adds or reruns no tests. The
270-test record is unchanged.

Override the install prefix when needed:

```bash
make install PREFIX=/usr/local
```

## Build targets

```bash
make validate
make build
make pwa-build
```

`make docker-build` builds the local `hive-sync-api` image when a Docker-compatible
runtime is available:

```bash
make docker-build IMAGE=hive-sync-api:local
```

## First packaged release shape

Use GitHub Releases as the first distribution channel:

1. Run `make validate`.
2. Run `make build`.
3. Package `target/release/hive` with README/license/checksum.
4. Attach platform-specific archives to a GitHub release.

Suggested archive names:

- `hive-<version>-aarch64-apple-darwin.tar.gz`
- `hive-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `hive-<version>-aarch64-unknown-linux-gnu.tar.gz`

## Deferred package-manager paths

- Homebrew tap for macOS once CLI surface stabilizes.
- Arch/CachyOS PKGBUILD once release artifacts are stable.
- Container image publishing once Docker/Cloud Build verification is available.

Do not publish `hive` under the MindWeaver `mw` binary name; Hive remains the
sync/API/PWA companion boundary.
