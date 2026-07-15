# Hive Sync Cloud Deployment (Cloud Run + Rust `hive serve`)

> Status: active Rust `hive` deployment notes. The current Rust server supports
> in-memory and SQLite stores. Cloud SQL/Postgres scripts are kept as a deferred
> hardening path and must not be treated as an active durable backend until a
> Postgres store is implemented.

This guide deploys `hive-sync-api` to Google Cloud using:

- **Cloud Run** (API runtime)
- **Secret Manager** (device tokens)
- **Artifact Registry** (container images)

## Current persistence modes

- `HIVE_STORE=memory` (default): safe for API/PWA smoke checks, but data is lost
  on restart, scale-out, or revision replacement.
- `HIVE_STORE=sqlite`: useful only for single-instance experiments unless a real
  persistent volume is attached. The deploy script requires `MAX_INSTANCES=1` for
  this mode to avoid divergent per-instance databases.
- Cloud SQL/Postgres: **not implemented in Rust `hive serve` yet**. The old
  Cloud SQL bootstrap/backup scripts are retained for future work and are opt-in.

## 1) Prerequisites

1. Install and authenticate `gcloud`.
2. Ensure your account can administer:
   - Cloud Run
   - Cloud SQL
   - Artifact Registry
   - Secret Manager
   - IAM
3. From the repository root, make scripts executable once:

```bash
chmod +x scripts/cloud/bootstrap-hive-sync-infra.sh scripts/cloud/deploy-hive-sync-api.sh
```

If `gcloud` reports that `PROJECT_ID` is empty, either:

- pass the literal `--project "<project-id>"` flag to commands, or
- run `gcloud config set project <project-id>`

## 2) Bootstrap cloud resources

At minimum set `PROJECT_ID` and run:

```bash
PROJECT_ID="your-gcp-project" \
REGION="us-central1" \
scripts/cloud/bootstrap-hive-sync-infra.sh
```

This creates/ensures by default:

- required GCP APIs
- Artifact Registry repo (`hive-sync`)
- Secret Manager secrets:
  - `hive-sync-api-device-tokens` (placeholder by default)
- runtime service account (`hive-sync-api-runner@<project>.iam.gserviceaccount.com`)
- IAM grants for Secret Manager access

To provision legacy/future Cloud SQL resources anyway, run bootstrap with
`PROVISION_CLOUD_SQL=true`. The current Rust server will not use `DATABASE_URL`
until a Postgres store is added.

### Important after bootstrap

If you keep auth enabled, rotate placeholder device tokens immediately:

```bash
printf '%s' "desktop=<real-token>,phone=<real-token>" \
  | gcloud secrets versions add hive-sync-api-device-tokens --data-file=- --project "$PROJECT_ID"
```

For full rotation procedure, see:

- `docs/hive-sync-token-rotation.md`

### Verify token mapping (read-only)

```bash
gcloud secrets versions access latest \
  --secret "hive-sync-api-device-tokens" \
  --project "$PROJECT_ID"
```

If output is `desktop=abc123,phone=def456`, then use:

- `desktop` as `--device-id`
- `abc123` as bearer token value

Do **not** pass `desktop=abc123` as bearer token.

If you add a new device, write a full replacement map that includes **all** devices (for example
`work=...,personal=...,phone=...`). A new secret version replaces the active mapping; it does not
merge automatically with older versions.

Quick validation:

```bash
hive sync token check \
  --endpoint "https://<service-url>" \
  --device-id "desktop" \
  --token "abc123"
```

## 3) Deploy service to Cloud Run

```bash
PROJECT_ID="your-gcp-project" \
REGION="us-central1" \
scripts/cloud/deploy-hive-sync-api.sh
```

This deploys `HIVE_STORE=memory` by default. For an explicit SQLite single-instance
smoke deployment:

```bash
PROJECT_ID="your-gcp-project" \
REGION="us-central1" \
HIVE_STORE="sqlite" \
HIVE_SQLITE_PATH="/data/hive-sync.db" \
MAX_INSTANCES=1 \
scripts/cloud/deploy-hive-sync-api.sh
```

Do not use SQLite mode as a durable multi-instance production backend without a
real persistent volume and an operational backup story.

The deploy script:

1. builds/pushes container image using `Dockerfile.hive-sync-api`
2. deploys Cloud Run service
3. injects device-token secret as `HIVE_SYNC_DEVICE_TOKENS` when present
4. sets `HIVE_STORE`, `HIVE_SQLITE_PATH` when applicable, and
   `HIVE_SYNC_PERMISSIVE_CORS`

## 4) Verify deployment

Use the service URL printed by deploy script:

```bash
curl "https://<service-url>/healthz"
```

Optional remote doctor check (token required if auth enabled):

```bash
hive sync doctor --endpoint "https://<service-url>" --device-id desktop --token "<device-token>"
```

Safer local storage/fetch on macOS (Keychain):

```bash
printf '%s' '<device-token>' | hive sync token store --token-stdin --device-id desktop
hive sync doctor --endpoint "https://<service-url>" --device-id desktop --token-from-keychain
```

Alternative (dynamic fetch via Secret Manager, no local persistence):

```bash
hive sync doctor \
  --endpoint "https://<service-url>" \
  --device-id desktop \
  --token-command "gcloud secrets versions access latest --secret hive-sync-api-device-tokens --project $PROJECT_ID | tr ',' '\\n' | awk -F= '$1==\"desktop\"{print $2}'"
```

## 5) Client usage against cloud endpoint

```bash
hive sync run \
  --endpoint "https://<service-url>" \
  --db-path /path/to/notes.db \
  --device-id "desktop" \
  --token "<device-token>"
```

Or via Keychain on macOS:

```bash
hive sync run \
  --endpoint "https://<service-url>" \
  --db-path /path/to/notes.db \
  --device-id "desktop" \
  --token-from-keychain
```

## 6) Common overrides

Both scripts support env overrides:

- `PROJECT_ID`
- `REGION`
- `RUN_SA_NAME`
- `ARTIFACT_REPOSITORY`
- `DEVICE_TOKENS_SECRET_NAME`

Additional bootstrap-only:

- `CLOUD_SQL_DATABASE_VERSION` (default `POSTGRES_15`)
- `PROVISION_CLOUD_SQL` (default `false`; future/deferred Postgres path)
- `CLOUD_SQL_INSTANCE`
- `CLOUD_SQL_TIER` (default `db-custom-1-3840`)
- `CLOUD_SQL_STORAGE_GB` (default `20`)
- `CLOUD_SQL_DB_NAME` (default `hive_sync`)
- `CLOUD_SQL_DB_USER` (default `hive_sync_app`)
- `DB_URL_SECRET_NAME`
- `DB_PASSWORD` (auto-generated if omitted)

Additional deploy-only:

- `SERVICE_NAME` (default `hive-sync-api`)
- `IMAGE_NAME` / `IMAGE_TAG`
- `HIVE_STORE` (`memory` or `sqlite`; default `memory`)
- `HIVE_SQLITE_PATH` (default `/data/hive-sync.db` when `HIVE_STORE=sqlite`)
- `PERMISSIVE_CORS` (default `true`)
- `CORS_ALLOWED_ORIGINS` is retained for operator notes, but the current Rust
  server exposes a boolean permissive-CORS mode via `PERMISSIVE_CORS`.
- `ALLOW_UNAUTHENTICATED` (default `true`)
- `MIN_INSTANCES` / `MAX_INSTANCES`

### Browser/PWA note

The current Rust server uses permissive CORS when `PERMISSIVE_CORS=true`, which is
the deploy-script default for PWA/browser development.

If/when origin-specific CORS is added, use `CORS_ALLOWED_ORIGINS` during deploy.

Example:

```bash
PROJECT_ID="your-gcp-project" \
REGION="us-east1" \
CORS_ALLOWED_ORIGINS="https://hive-pwa-abc-ue.a.run.app;http://localhost:5173" \
scripts/cloud/deploy-hive-sync-api.sh
```

For LAN phone testing against the local Vite dev server, include the machine's LAN origin too:

```bash
PROJECT_ID="your-gcp-project" \
REGION="us-east1" \
CORS_ALLOWED_ORIGINS="http://localhost:5173;http://<lan-ip>:5173" \
bash scripts/cloud/deploy-hive-sync-api.sh
```

Remember:

- `http://<lan-ip>:5173` is the **PWA URL**
- `https://<service-url>` is the **API endpoint** entered into the app

## 7) Rollback (quick)

List revisions:

```bash
gcloud run revisions list --service hive-sync-api --region "$REGION" --project "$PROJECT_ID"
```

Send traffic to previous revision (example):

```bash
gcloud run services update-traffic hive-sync-api \
  --region "$REGION" \
  --project "$PROJECT_ID" \
  --to-revisions "hive-sync-api-00009-abc=100"
```

## 8) Next operational hardening

After initial deployment, set up:

- monitoring and alerting: `docs/hive-sync-monitoring.md`
- backup and recovery baseline only after a durable backend is selected:
  `docs/hive-sync-backup-recovery.md`
