# Google Desktop sign-in

The live capture lane below remains Tasks-readonly. A separate, locally tested
[Calendar auth profile](#separate-calendar-authorization-local-only) is available in
the development build; it has not been live-authorized or connected to publication.

## Overview

Desktop sign-in, refresh, and native macOS Keychain storage are implemented in
`hive-google-auth` and `hive captures auth`. **OAuth is now live-verified:** the
provided Desktop client was used for browser consent, the `personal` credential was
stored in native macOS Keychain, and refresh plus account verification succeeded.
Google Tasks list access now succeeds after API enablement. The dedicated
`MindWeaver Inbox` list has been selected and baselined with zero tasks. Its private
queue is initialized at `~/Library/Application Support/Hive/google-captures.sqlite`;
`capture-activation.txt` alongside it records the source ID and setup checkpoint
(operational metadata only, not automatically loaded configuration). The first
Android-to-Markdown capture is now live-verified: one task, one writer attempt, and
a repeated poll/delivery with no duplicate or note/dashboard rewrite. The task is
still unchecked. New scheduled phone-to-editor-owned writer delivery and repeat
nonduplication are also live-verified on 2026-11-01, and the
live editor is registered and ready. The downloaded root client
file is Git-ignored.
Do not paste client files, authorization codes, or tokens into chat.

**Latest-build credential access and scheduled polling verified (2026-11-01):**
the user reconfirmed foreground `auth status` and `lists --connection personal`
success with `/Users/jdawson/.local/bin/hive`. Bounded local status diagnostics in
inherited and minimal environments both succeeded in 0.054 and 0.028 seconds.
Earlier 45-second background `unknown_outcome` and 12-second local status stall
did not reproduce. The credential-operation failure is resolved operationally;
its root cause is not established. The same build/credentials now work, without
a speculative source fix or a new grant.

Configuration binds the existing personal queue/list/baseline and fixed public
environment to the MindWeaver working directory (60-second interval, 45-second
timeout, limit 1); `configValid` and `queueScopeValid` are true. A clean/idle check
preceded in-place `mw_ops` hotload preserving module identity and the active guard
across four shared references, then bridge registration. After user-authorized
orphaned ChatGPT helper cleanup, installed `editor-check` reports `ready: true`,
`noEditor: false`, `deferred: null`. A live unsaved hidden-buffer probe verified
`dirty_buffers` readiness deferral without note edits, then clean readiness was
restored with the guard unchanged and `busy: false`.
At the earlier polling checkpoint, two controlled `run-once` cycles completed in about 0.5 seconds each, observing one
unchanged task, staging/delivering zero, with `authActionRequired: false` and
generation 1. Background install/start then enabled the owned LaunchAgent:
`serviceActive: true`, `serviceState: loaded_owned`. Actual startup and next
scheduled cycles completed about 60.7 seconds apart, each with no pending work,
one unchanged observation, zero staged/delivered, and no auth action required.
This positively proves latest-build no-UI background Keychain access while
unlocked, not denial/locked scenarios. Those cycles left Inbox/dashboard hashes,
bytes, and mtimes unchanged and the original receipt at attempt 1.
The user subsequently added the undated, title-only `Verify scheduled phone capture`
on the phone. An automatic scheduled cycle (not `run-once`) observed 2, staged 1,
unchanged 1, delivered 1, pending 0, and completed through the native Neovim bridge
with `writerExited: true` and zero conflicts. Its one-attempt durable receipt matches
the actual Inbox revision; Markdown and a recursive whole-notes-root search confirm
exactly one unchecked stable-marker source entry. The next actual scheduled cycle
observed 2 unchanged, staged/delivered 0, `no_pending`, `authActionRequired: false`,
with `serviceActive: true`, `serviceState: loaded_owned`. Both receipts retain one
attempt; Inbox/dashboard bytes, sizes, and mtimes were unchanged after delivery
through follow-up. Dashboard content was unchanged even on delivery, but its mtime
changed then. See the
[activation runbook](google-tasks-capture.md#background-activation-enabled-and-verified)
for evidence and stable-path status/stop/start/uninstall controls. Polling runs in
the GUI user session; Google holds work while the computer is off, with no server.

This inbound lane requests exactly `https://www.googleapis.com/auth/tasks.readonly`
and `openid`, with offline access. It requests no email, profile, Calendar, or write
scope. `openid` supports account binding through Google's fixed UserInfo endpoint;
Hive does not decode a JWT to infer identity. No hosted Hive service is required.

## API / Interfaces

| Command | Contract |
| --- | --- |
| `hive captures auth login --connection personal --client-json '/absolute/path/desktop-client.json'` | Explicit browser consent using the selected Desktop client file; persists an account-bound grant. |
| `hive captures auth status --connection personal` | Local-only credential presence; no Google request or remote grant validation. |
| `hive captures auth logout --connection personal` | Clears the local grant (client material and refresh token), retains the identity tombstone; does not revoke Google's grant. |

Connection names contain 1–64 lowercase letters, digits, `-`, or `_`. Successful
auth JSON exposes only `schemaVersion: "1"`, `connection`, `credentialStored`,
`accountNamespace`, and `scopes`, not credentials or raw subject. A stored credential
is not proof that Google still accepts it.
Local-only does not mean prompt-free: foreground Keychain access can require OS
approval, including after building/installing a different binary.

`lists`, `poll`, and `run` require **exactly one** of:

- `--connection NAME`: refresh and verify the saved identity; **no `--account-key`**.
- `--access-token-env HIVE_GOOGLE_ACCESS_TOKEN`: explicit legacy access-token mode.
  Other valid `HIVE_GOOGLE_`-prefixed environment names are supported. `poll`/`run`
  also require a stable operator `--account-key`; reserved `google-sub-v1:` keys are
  refused. This mode does not verify the operator's account binding or renew tokens.

The prototype's implicit environment default is intentionally removed. Missing,
locked, or denied saved credentials never fall back to ambient environment tokens.
`deliver`, `reconcile`, queue `status`, and `recover-store` require no Google credentials.

## Data Flow

1. Login reads only the explicitly selected JSON file, read-only, as a bounded
   regular file (64 KiB maximum). There is no private credential discovery. Production
   authorization, token, and UserInfo endpoints are fixed to Google; no custom
   production endpoints are accepted.
2. PKCE S256 uses independently random 256-bit state and verifier. A one-shot
   listener binds numeric `127.0.0.1` on an ephemeral port with the fixed root
   callback `/` (`http://127.0.0.1:PORT/`), not `/callback`. It allows five minutes,
   at most 16 connections, 8 KiB request headers, and two seconds per connection.
   Duplicate fields, wrong state/path/Host, and invalid bodies are rejected;
   static browser responses never reflect submitted values.
3. macOS launches the system browser via `/usr/bin/open` with a cleared environment.
   Credentials are not browser argv. Failure closes the flow: no manual code-copy
   or deprecated out-of-band (OOB) fallback.
4. Initial exchange must explicitly return exactly the two required scopes and a
   refresh token. Fixed Google UserInfo verifies `sub`; `GoogleIdentity` derives the
   queue namespace from the Google issuer/subject, not the alias or OAuth client ID.
5. Native Keychain service `hive/google-captures/v1` stores the refresh grant and
   client material required for refresh in encrypted Keychain storage, with
   synchronization explicitly false. Native SecItemAdd/update avoids delete/re-add;
   there is no `security` shell command, secret argv, or plaintext fallback.
   Access tokens stay in memory, not the queue or Keychain. Typed secret buffers
   redact Debug output and zeroize owned storage (not a guarantee about all HTTP copies).
6. Each credential-backed `lists`/`poll`/`run` refreshes once at command start, without
   an access-token cache or implicit browser. If Google rotates R1 to R2, Hive saves
   R2 durably under the unchanged identity/client binding **before UserInfo**. No
   access token is returned until UserInfo matches the stored subject. An omitted
   refresh `scope` retains the validated prior scopes per OAuth; explicit scopes
   must still match exactly.

All per-connection load/network/save operations, browser login, status, and logout
share an OS lock. Its root is the effective OS user's home resolved with
`getpwuid_r(geteuid())`, followed by `Library/Application Support/Hive/google-auth`;
it is **not derived from `HOME` or XDG settings**. Different process environments
must serialize access to the same Keychain credential. Private directories are
`0700`, hashed lock files `0600`; `flock` waits at most five seconds before `Busy`.
RAII/kernel release handles exit/crash. Lock files persist: never delete a supposed
stale lock, which could split the inode lock domain.

Background cycles use the same credential and lock domain with native Keychain
authentication UI explicitly forbidden on reads and writes. They never open a
browser or fall back to environment tokens. An action-required failure records a
sanitized status and pauses refresh for the same credential generation; metadata
checks remain no-UI. Busy locks and transient transport failures are not permanent
auth pauses. A successful explicit foreground credential operation that saves a
new generation allows refresh to resume. `auth status` never advances it, and
`lists` advances it only on persisted refresh-token rotation; if the same-generation
pause remains after foreground recovery, explicit login is required. Refresh
rotation failures retain the
generation of the attempted grant so a newly rotated invalid grant is not retried
on every tick. This pauses provider authentication, not necessarily delivery of
already-staged local items. Worker state is separately fixed under the OS-user
home at `Library/Application Support/Hive/background`, not `HOME` or XDG overrides.

## Edge Cases

- `invalid_grant` becomes a typed reauthentication-required error: explicitly log
  in again. There is no automatic retry, browser prompt, or credential fallback.
- A transient UserInfo failure after rotation retains R2 for the next explicit
  command. If saving fails after Google has spent R1, reauthentication may be
  necessary. There is no cross-service atomicity promise.
- A different verified subject cannot reuse an alias, even after logout. For a
  genuinely different account, explicitly choose a new alias; aliases are not
  queue namespaces. Do not use new aliases/namespaces/databases to recapture old items.
- Saved mode fails closed on unsupported operating systems. Explicit token-env
  mode remains available; Keychain denial/lock must be resolved explicitly on macOS.
- Queue V2 migration does not adopt legacy operator identities. Same-list verified
  and unverified connections are forbidden in either order, even if the old
  baseline had no jobs. Stop for future explicit binding migration; a fresh DB or
  namespace is **not** a safe workaround because earlier captures may duplicate.
  See the [capture runbook](google-tasks-capture.md#data-flow).
- Logout is local only. To revoke Google's grant, separately review/remove the app
  in [Google Account third-party connections](https://myaccount.google.com/connections).
  This is a user-controlled manual operation, not something logout performs.

### Troubleshooting

| Symptom | Action |
| --- | --- |
| Invalid client/file | Use the downloaded **Desktop app** JSON, not Web/Android credentials; check the selected path and regular-file/size constraints. Do not share its contents. |
| Consent denied, blocked, or missing scopes | Review project consent/test-user settings and organization policy. Hive cannot bypass Workspace admin or Google verification requirements. |
| Browser unavailable / callback timeout | Check local browser/loopback availability, then explicitly retry login. Do not copy authorization codes into a shell or chat. |
| Storage denied/locked | Resolve the macOS Keychain prompt or access state, then retry explicitly; no plaintext/env fallback occurs. |
| Foreground status hangs without output | The historical 12-second stall did not reproduce; latest bounded local diagnostics and user-confirmed foreground status/lists succeeded. If it recurs, use current `~/.local/bin/hive captures auth status --connection personal`, then `~/.local/bin/hive captures lists --connection personal` in the foreground and handle any OS prompt. The earlier root cause is not established. Do not reset credentials/queue or request a fresh grant unless explicitly needed. |
| Registered editor reports `editor_unavailable` | Inspect readiness without bypassing discovery. The historical orphan-helper blocker is cleared; current readiness is true. Readiness and credential access are separate checks, both now positively verified. |
| Background `authActionRequired` | Stop activation and recover explicitly in the foreground with the configured stable binary. Run `auth status`, then `lists` to exercise refresh; use explicit login only if reauthentication is required. Same-generation refresh stays paused until a successful foreground save advances the generation. |
| Busy | Let the active credential operation finish (including browser consent); retry later, without deleting lock files. |
| Account mismatch | Sign in as the alias's original account; use a new alias only for an intentionally different account, not a migration bypass. |

Refresh tokens are not permanent. Google's external consent screen in **Testing**
can issue seven-day refresh tokens unless only the documented basic identity
scopes are requested; Tasks readonly is outside that exception. Publishing and
verification requirements depend on audience, scopes, and policy. Do not promise
indefinite access or bypass consent/admin restrictions.

## Example usage

### User-controlled Google setup

1. In Google Cloud Console, create or select your own project and enable the
   **Google Tasks API**. These are user actions, not automatic account changes.
2. Configure the OAuth consent screen/audience. Add your account as a test user
   where required for external Testing, or follow the publishing/verification and
   organization policies appropriate to your project.
3. Create an OAuth client of type **Desktop app**, not Web application or Android.
   Download its JSON and keep it private, outside version control and shared notes.
   Tell the assistant only its absolute filepath, never its contents or tokens.
4. With the compatible built Hive binary, explicitly run login below. Select the
   intended Google account in the system browser, review consent, and handle any
    OS Keychain prompt. The current `personal` connection has completed this live
    step; future connections still require their own explicit consent.
5. Prefer a dedicated, initially empty personal Google Tasks list (for example,
   `MindWeaver Inbox`), created in the Google Tasks app. Run `lists` to exercise
   refresh and choose its actual list ID. Baseline that empty list **before**
   creating the Android test task; otherwise the first baseline deliberately
   ignores that task as pre-existing. Then follow
   the [capture activation/bootstrap runbook](google-tasks-capture.md#example-usage)
   before any live queue or Markdown writes.

For a new connection, login is explicit. The existing `personal` grant does not
need a fresh Cloud setup merely because the binary changed. Examples use the
stable binary path. The user reconfirmed latest-build status and `lists` success on
2026-11-01; no-UI background access while unlocked is also verified. For a future
binary change, check status/lists in the foreground and handle any OS prompt.
The login example is for a new connection or explicit reauthentication need only,
not a requirement for the currently working connection:

```sh
/Users/jdawson/.local/bin/hive captures auth login \
  --connection personal --client-json '/absolute/path/desktop-client.json'
/Users/jdawson/.local/bin/hive captures auth status --connection personal
/Users/jdawson/.local/bin/hive captures lists --connection personal
```

After selecting the list, private queue, bootstrap policy, and trusted writer,
use the runbook's `poll`/`run` examples; saved mode never needs `--account-key`.

### Validation boundary

Latest checks on 2026-11-01 after the inventory fixes:
**270 normal workspace tests passed, five ignored**; strict Clippy passed for
`hive-background`, `hive-google-auth`, and `hive-capture` with `--all-targets -- -D warnings`.
Two explicitly ignored real isolated Neovim tests passed after the fixes. Both
real-writer integration gates (real mw CLI; fake Google with real Hive/mw) passed
earlier and were **not rerun after the fixes**. Inherited-FD test isolation now uses
a single-test child to avoid parallel fork-test leakage, without changing original
assertions or production semantics.
The fifth ignored crash-fixture helper is already exercised by normal recovery
tests. Earlier format validation and Neovim background/safety suites (32/54 tests)
passed on 2026-10-31; these are not newly rerun results. See the
[capture validation record](google-tasks-capture.md#recorded-implementation-validation).
Legacy server/desktop workspace-wide Clippy warnings remain outside this strict gate.

Automated auth validation uses fake backends and OS-lock fixtures, never live
credentials. Subsequent explicitly requested activation has also verified browser
callback, exact requested scopes, native Keychain persistence, and token refresh.
The separate Tasks API HTTP 403 was resolved by API enablement, and task-list
enumeration now succeeds. The subsequent live capture proof delivered one title-only,
undated Android task into the Markdown Inbox and verified a duplicate-free repeat
without source/dashboard rewrites. No completion or archival was performed.
Scope rejection diagnostics expose
only known-permission booleans/counts, never tokens or provider-controlled values.
The real-writer checks use isolated temporary notes, not the live notes tree.
Separately, two controlled cycles and actual LaunchAgent startup/next scheduled
cycles now prove enabled polling and no-UI Keychain access while unlocked.
The subsequent new-phone automatic scheduled delivery and next-scheduled-cycle
nonduplication proof establish the editor-owned source-write path separately from
those earlier polling checks. Live computer-off catch-up remains unverified.
Live dirty-buffer readiness deferral is proven, not full pending-job dirty-buffer
delivery deferral. No Google/source completion or archival was performed. Calendar, export,
completion writeback, lifelog, and PWA migration remain pending. No additional tests
ran during activation; this docs-only update changes no source code and adds or
reruns no tests. The 270-test record above is unchanged.

## Separate Calendar authorization (local only)

`hive calendar auth login/status/logout` explicitly selects the Calendar Events
profile. It requests exactly `https://www.googleapis.com/auth/calendar.events`
and `openid`, not Tasks, full Calendar administration, email, or profile scopes.
The events scope permits reading and writing events across accessible calendars;
OAuth itself does **not** restrict access to one pilot calendar. A future publisher
must independently bind the selected calendar and verified Google account.

Calendar grants use native Keychain service `hive/google-calendar/v1` and the
OS-user lock directory `Library/Application Support/Hive/google-calendar-auth`.
Tasks retains `hive/google-captures/v1`, `google-auth`, and the existing wire format
and defaults. Even identical aliases select separate local records. The flow,
provider and storage codec enforce the selected profile's exact scope set; mixed,
extra, duplicate and wrong-profile scopes fail closed. Account tombstones and
refresh-token rotation safeguards apply to both profiles.

Separate local records do not establish independent Google-side consent,
refresh-token or revocation lifecycles when sharing a client/project/account.
For the pilot, prefer a dedicated Calendar Desktop OAuth client in a separate
Google Cloud project to avoid sharing the working capture application's consent
configuration. Do not revoke or broaden the existing Tasks grant as a workaround.
Enable the Google Calendar API and configure the audience/test user for that
project. Download the Desktop client JSON to a private location outside version
control; share only its absolute path, never its contents or tokens. Calendar
scopes also fall outside the basic-identity exception to external Testing's
conditional seven-day refresh-token expiration.

Only after explicit authorization to start browser consent and persist a separate
local grant, run the development build (the installed capture worker binary need
not be replaced):

```sh
./target/debug/hive calendar auth login \
  --connection calendar-personal --client-json '/absolute/private/path/calendar-desktop.json'
./target/debug/hive calendar auth status --connection calendar-personal
```

Login opens browser consent and, on success, stores an account-bound grant in the
native, nonsynchronizing macOS Keychain. Status is local-only, not proof that
Google still accepts the grant or that a particular calendar is writable. Logout
clears the local grant but retains its account tombstone; it does not revoke the
Google grant. No fallback to the Tasks connection or environment credentials is
provided by these commands.

This slice adds no live calendar-access probe, event publication, hosted credential
storage or runnable hosted worker. The macOS Keychain is not a hosted Linux secret
backend. Browser login, credential installation/export, hosted secrets, cost
controls, commits/pushes and hosted runs remain separate activation decisions.
Validation uses fake credentials and temporary local fixtures only; no live
Calendar authorization or native Keychain I/O has been exercised for this profile.

## Related files

- [Auth types and interfaces](../crates/hive-google-auth/src/lib.rs)
- [Login/refresh/rotation orchestration](../crates/hive-google-auth/src/provider.rs)
- [Fixed Google transport and client-file reader](../crates/hive-google-auth/src/google.rs)
- [PKCE, callback, and browser launch](../crates/hive-google-auth/src/loopback.rs)
- [Native storage and OS-user lock root](../crates/hive-google-auth/src/storage.rs)
- [CLI credential selection](../crates/hive/src/captures_auth.rs)
- [Verified identity namespace](../crates/hive-core/src/google_identity.rs)
- [Capture queue identity bindings](../crates/hive-capture/src/store.rs)
- [Google native-app OAuth guidance](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Google refresh-token expiration guidance](https://developers.google.com/identity/protocols/oauth2#expiration)

Official guidance checked for this delivery on 2026-10-30: Desktop loopback/PKCE
and conditional external-Testing refresh-token expiration.
