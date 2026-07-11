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
