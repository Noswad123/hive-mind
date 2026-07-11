# Hive Sync Protocol

This document captures the current wire contract shared by the legacy Go sync
API, the PWA, and the new Rust `hive` workspace.

The Rust server entrypoint is:

```bash
hive serve --addr 127.0.0.1:8080 --device-tokens desktop=token --permissive-cors
```

Basic Rust client checks:

```bash
hive sync doctor --endpoint http://127.0.0.1:8080 --token token
hive sync register --endpoint http://127.0.0.1:8080 --device-id desktop --token token
hive sync token check --endpoint http://127.0.0.1:8080 --device-id desktop --token token
hive sync pull --endpoint http://127.0.0.1:8080 --token token --cursor 0
```

Desktop note safety: pulled `note` operations are staged locally for review via
`hive sync notes review`; they are not written directly to canonical Markdown.

## Endpoints

- `GET /healthz`
- `GET /v1/sync/state`
- `POST /v1/sync/push`
- `GET /v1/sync/pull?cursor=<cursor>&limit=<limit>`
- `POST /v1/devices/register`

## Auth

Authenticated deployments use bearer device tokens:

```http
Authorization: Bearer <device-token>
```

The authenticated token's device id must match request `device_id` when a request
body includes one.

Rust `hive` sync client commands can resolve this bearer token from an explicit
`--token`, `HIVE_SYNC_TOKEN`, `--token-command`, or macOS Keychain via
`--token-from-keychain`. Keychain storage is managed with `hive sync token store`.

## Operation shape

```json
{
  "op_id": "op-uuid-or-stable-id",
  "idempotency_key": "stable-client-generated-key",
  "entity_type": "note",
  "entity_id": "notes/example.md",
  "op_type": "upsert",
  "payload": {},
  "client_updated_at": "2026-07-10T00:00:00Z",
  "base_version": 0
}
```

Allowed values:

- `entity_type`: `note`, `todo`
- `op_type`: `upsert`, `delete`

## Push

Request:

```json
{
  "device_id": "desktop",
  "device_name": "MacBook",
  "platform": "darwin",
  "app_version": "hive-dev",
  "operations": []
}
```

Response:

```json
{
  "accepted": ["op-1"],
  "rejected": [{ "op_id": "op-2", "reason": "base_version must be >= 0" }],
  "server_cursor": "42",
  "conflicts": []
}
```

## Pull

Response:

```json
{
  "operations": [],
  "next_cursor": "42",
  "has_more": false
}
```

## Conflict event shape

```json
{
  "entity_type": "todo",
  "entity_id": "todo-1",
  "winner": "op-newer",
  "loser": "op-older",
  "reason": "base_version_mismatch",
  "winner_device_id": "phone",
  "loser_device_id": "desktop"
}
```

## Current resolution rule

Conflict resolution is currently last-write-wins using `client_updated_at`, with
tie-breakers by `op_id`, then `device_id`.

Before expanding this protocol, add a Rust fixture/test in `hive-core` and keep
the PWA TypeScript types aligned.
