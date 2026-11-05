#!/usr/bin/env bash
# Read-only development prerequisites; never installs, builds, or invokes Hive.
set -euo pipefail

git_version_supported() {
    local major minor
    [[ "$1" =~ ^git\ version\ ([0-9]+)\.([0-9]+) ]] || return 1
    major=$((10#${BASH_REMATCH[1]}))
    minor=$((10#${BASH_REMATCH[2]}))
    (( major > 2 || (major == 2 && minor >= 50) ))
}

rust_version_supported() {
    local major minor
    [[ "$1" =~ ^rustc\ ([0-9]+)\.([0-9]+) ]] || return 1
    major=$((10#${BASH_REMATCH[1]}))
    minor=$((10#${BASH_REMATCH[2]}))
    (( major > 1 || (major == 1 && minor >= 89) ))
}

main() {
    local pwa=false failures=0 tool output os
    case "${1:-}" in
        --help|-h)
            printf '%s\n' 'Usage: bash scripts/dev/linux-preflight.sh [--pwa]' \
                'Check Linux, /usr/bin/git >= 2.50, Rust >= 1.89, Cargo, rustfmt, Clippy, cc and make.' \
                '--pwa also checks Node.js >= 20 and npm.' \
                'No builds, package installations, application credentials, or service operations.'
            return 0 ;;
        --pwa) pwa=true ;;
        '') ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; return 2 ;;
    esac
    if (( $# > 1 )); then
        printf '%s\n' 'Pass at most one option; see --help.' >&2
        return 2
    fi
    os="$(uname -s)"
    if [[ "$os" != Linux ]]; then
        printf 'Run this preflight on Linux (CachyOS); current OS: %s\n' "$os" >&2
        return 2
    fi
    for tool in bash make cc cargo rustc; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            printf 'MISSING: %s\n' "$tool" >&2
            failures=$((failures + 1))
        fi
    done
    if output="$(/usr/bin/git --version 2>/dev/null)" && git_version_supported "$output"; then
        printf 'OK: /usr/bin/git: %s\n' "$output"
    else
        printf '%s\n' 'MISSING/OLD: Git >= 2.50 specifically at /usr/bin/git (PATH alone is insufficient).' >&2
        failures=$((failures + 1))
    fi
    if output="$(rustc --version 2>/dev/null)" && rust_version_supported "$output"; then
        printf 'OK: %s\n' "$output"
    else
        printf '%s\n' 'MISSING/OLD: Rust >= 1.89; select an installed toolchain with rustfmt and Clippy.' >&2
        failures=$((failures + 1))
    fi
    for tool in version fmt clippy; do
        if [[ "$tool" == version ]]; then
            if output="$(cargo --version 2>/dev/null)"; then
                printf 'OK: %s\n' "$output"
                continue
            fi
        elif output="$(cargo "$tool" --version 2>/dev/null)"; then
            printf 'OK: %s\n' "$output"
            continue
        fi
        printf 'MISSING: cargo %s\n' "$tool" >&2
        failures=$((failures + 1))
    done
    if [[ "$pwa" == true ]]; then
        if output="$(node --version 2>/dev/null)" && [[ "$output" =~ ^v([0-9]+)\. ]] && (( 10#${BASH_REMATCH[1]} >= 20 )); then
            printf 'OK: Node.js %s\n' "$output"
        else
            printf '%s\n' 'MISSING/OLD: Node.js >= 20 for optional PWA work.' >&2
            failures=$((failures + 1))
        fi
        if output="$(npm --version 2>/dev/null)"; then
            printf 'OK: npm %s\n' "$output"
        else
            printf '%s\n' 'MISSING: npm for optional PWA work.' >&2
            failures=$((failures + 1))
        fi
    fi
    if (( failures > 0 )); then
        printf 'Preflight failed: %s prerequisite check(s). See docs/development-linux.md.\n' "$failures" >&2
        return 1
    fi
    printf '%s\n' 'Prerequisites found. This is not a Linux build/runtime validation.' \
        'Next: follow docs/development-linux.md and docs/development-handoff.md.'
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
