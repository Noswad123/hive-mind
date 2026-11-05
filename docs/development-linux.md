# CachyOS / Linux development

## Overview

Continue development on CachyOS (Arch Linux) without migrating credentials or
activating integrations. **Linux build/runtime behavior is not yet validated.**
The handoff adds documentation and preflight tooling, not Linux proof. Start with
the checks below and record any OS-gating fixes needed; do not infer Linux success
from the prior macOS results in the [handoff](development-handoff.md).

MindWeaver is a separate sibling repository, **not a Cargo build dependency**.
Ordinary fixtures use stub parsers, synthetic credentials and temporary repositories;
neither real `mw` nor private notes are needed for the first checks.

## API / Interfaces

- Rust stable supporting workspace edition **2024**, plus `rustfmt` and `clippy`.
  Preflight enforces **Rust >= 1.89** as the tested baseline, not an established MSRV.
  Prior macOS validation used `rustc`/`cargo` **1.89.0**; this is evidence, not a
  new toolchain pin or a Linux compatibility guarantee.
- **Git >= 2.50 at `/usr/bin/git` is required.** The isolated source reader invokes
  that path; a newer `git` elsewhere on `PATH` is insufficient.
- `bash scripts/dev/linux-preflight.sh` (or `make linux-preflight`) reports Linux OS,
  tool presence and Git/Rust versions. It does not install packages, access native
  auth or call providers. Invoking with `bash` needs no executable-bit change.
- Optional `--pwa` checks Node 20+ and npm presence; these are not Rust prerequisites.
  `hive-pwa/package.json` has no `engines` declaration. Prefer a currently supported
  Node LTS satisfying the installed dependencies, not a claim that Node 20 was tested.

## Data Flow

1. Choose package/toolchain management, clone Hive, and run preflight.
2. Check formatting and workspace compilation; build the CLI without installing it.
3. Run the targeted credential-free fixtures before broadening validation.
4. Record exact commit, OS/tool versions, commands, results and failures. Only later
   consider separately approved real-MW integration or production work.

## Edge Cases

- Use an existing working distro Rust or rustup setup when available. Do not
  automatically remove distro Rust or install both providers. The package example
  below is for choosing rustup on a machine without a conflicting Rust setup.
  Use a full system upgrade (`-Syu`), not a partial `-Sy` upgrade.
- Commands below are proposed **Linux** setup steps; none was executed on the Mac
  for this documentation change. Package installation changes the Linux host.
- A fresh clone normally needs registry/network access. Use `--locked` first;
  add `--offline` only when all required registry metadata and crates are cached.
- `make validate` also checks the PWA, so it is not the first Rust-only check on a
  machine without Node/npm. No `make install` is required; do not replace the Mac
  worker or copy its installed binaries.
- Ordinary tests use fakes/loopback rather than real Google/GitHub credentials or
  native auth commands. Do not populate provider-secret environment variables.
  The ignored real-MW tests are not part of the commands below.
- Full workspace validation is optional follow-up, not currently established green.
  Historical Calendar HTTP/loopback timeouts/issues may recur; record failures
  rather than discarding them or claiming success from targeted checks.
- For later real integration, separately clone/build trusted MindWeaver and review
  the **Linux artifact's own SHA-256**. Do not reuse a Mac parser hash. Local publisher
  bindings contain absolute paths and parser pins: copying a checkout cannot resume
  publication safely. Neither private notes nor secrets are first-check prerequisites.
- macOS Keychain/LaunchAgents do not provide Linux native-auth/background equivalence.
  Do not start a second capture worker or run live provider diagnostics here.

## Example usage

Choose the package route first. For a new rustup-based CachyOS setup:

```sh
sudo pacman -Syu --needed base-devel git rustup
rustup toolchain install stable --profile minimal --component rustfmt --component clippy
```

Toolchain selection is your choice: `rustup default stable` is optional and changes
the user default. To avoid that change, run preflight as
`rustup run stable bash scripts/dev/linux-preflight.sh` and replace each `cargo`
invocation below with `rustup run stable cargo`. This ensures preflight checks the
selected toolchain too. With an existing distro toolchain, use its matching
formatting/lint components instead. Preflight does not configure toolchains.

From a parent directory of your choice (use normal Git authentication, or choose
SSH yourself; never embed credentials in a clone URL):

```sh
git clone https://github.com/Noswad123/hive-mind.git
cd hive-mind
bash scripts/dev/linux-preflight.sh
# Equivalent preflight entry point: make linux-preflight

cargo fmt --all -- --check
cargo check --locked --workspace
cargo build --locked -p hive

cargo test --locked -p hive-google-auth
cargo test --locked -p hive-tasks hosted::
cargo test --locked -p hive --test tasks_hosted --test tasks_publish
cargo test --locked -p hive-calendar source::
```

Then broaden to `cargo test --locked -p hive-tasks` (allow roughly two minutes,
not a fixed runtime guarantee). If pursuing complete workspace evidence, run
`cargo test --locked --workspace` and retain failures/ignored-test counts explicitly.
Do not add `--ignored` for initial validation.

Optional PWA checks, only after choosing Node/npm:

```sh
bash scripts/dev/linux-preflight.sh --pwa
npm ci --prefix hive-pwa
npm run check --prefix hive-pwa
```

The nondefault `tasks-conditional-probe` feature is not needed for setup. Build/test
it explicitly only when working on that diagnostic, using
`cargo build --locked -p hive --features tasks-conditional-probe` and
`cargo test --locked -p hive --features tasks-conditional-probe`.
Feature compilation is not permission to run its live Google commands; see the
[probe approval gates](tasks-conditional-probe.md).

## Related files

- [Workspace manifest](../Cargo.toml), [CLI features](../crates/hive/Cargo.toml),
  [PWA manifest](../hive-pwa/package.json), [Makefile](../Makefile)
- [Pinned source reader](../crates/hive-calendar/src/source.rs),
  [task snapshot facade](../crates/hive-calendar/src/task_snapshot.rs)
- [Hosted CLI fixtures](../crates/hive/tests/tasks_hosted.rs),
  [publisher CLI fixtures](../crates/hive/tests/tasks_publish.rs)
- [Development handoff](development-handoff.md), [historical validation](release.md)
