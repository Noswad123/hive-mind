#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

PROJECT_ID="${PROJECT_ID:-}"
REGION="${REGION:-us-central1}"
SERVICE_NAME="${SERVICE_NAME:-hive-sync-api}"

ARTIFACT_REPOSITORY="${ARTIFACT_REPOSITORY:-hive-sync}"
IMAGE_NAME="${IMAGE_NAME:-hive-sync-api}"
IMAGE_TAG="${IMAGE_TAG:-$(date +%Y%m%d-%H%M%S)}"

USE_CLOUD_SQL="${USE_CLOUD_SQL:-false}"
CLOUD_SQL_INSTANCE="${CLOUD_SQL_INSTANCE:-hive-sync-pg}"

RUN_SA_NAME="${RUN_SA_NAME:-hive-sync-api-runner}"
RUN_SA_EMAIL="${RUN_SA_NAME}@${PROJECT_ID}.iam.gserviceaccount.com"

DEVICE_TOKENS_SECRET_NAME="${DEVICE_TOKENS_SECRET_NAME:-hive-sync-api-device-tokens}"

ALLOW_UNAUTHENTICATED="${ALLOW_UNAUTHENTICATED:-true}"
CORS_ALLOWED_ORIGINS="${CORS_ALLOWED_ORIGINS:-}"
PERMISSIVE_CORS="${PERMISSIVE_CORS:-true}"

MIN_INSTANCES="${MIN_INSTANCES:-0}"
MAX_INSTANCES="${MAX_INSTANCES:-10}"

if [[ -z "${PROJECT_ID}" ]]; then
  echo "❌ PROJECT_ID is required"
  echo "   Example: PROJECT_ID=my-gcp-project scripts/cloud/deploy-hive-sync-api.sh"
  exit 1
fi

if ! command -v gcloud >/dev/null 2>&1; then
  echo "❌ gcloud CLI is required"
  exit 1
fi

DOCKERFILE_PATH="${REPO_ROOT}/Dockerfile.hive-sync-api"
if [[ ! -f "${DOCKERFILE_PATH}" ]]; then
  echo "❌ Dockerfile not found at ${DOCKERFILE_PATH}"
  exit 1
fi

CLOUDBUILD_CONFIG_PATH="${CLOUDBUILD_CONFIG_PATH:-${REPO_ROOT}/scripts/cloud/cloudbuild-hive-sync-api.yaml}"
if [[ ! -f "${CLOUDBUILD_CONFIG_PATH}" ]]; then
  echo "❌ Cloud Build config not found at ${CLOUDBUILD_CONFIG_PATH}"
  exit 1
fi

echo "🔧 Project:      ${PROJECT_ID}"
echo "🔧 Region:       ${REGION}"
echo "🔧 Service:      ${SERVICE_NAME}"
echo "🔧 Cloud SQL:    ${USE_CLOUD_SQL}"
if [[ "${USE_CLOUD_SQL}" == "true" ]]; then
  echo "🔧 SQL instance: ${CLOUD_SQL_INSTANCE}"
fi
if [[ -n "${CORS_ALLOWED_ORIGINS}" ]]; then
  echo "🔧 CORS origins:  ${CORS_ALLOWED_ORIGINS} (Rust server currently treats this as permissive CORS)"
fi

gcloud config set project "${PROJECT_ID}" >/dev/null

echo "📦 Ensuring Artifact Registry repository (${ARTIFACT_REPOSITORY}) exists..."
if ! gcloud artifacts repositories describe "${ARTIFACT_REPOSITORY}" \
  --location "${REGION}" \
  --project "${PROJECT_ID}" >/dev/null 2>&1; then
  gcloud artifacts repositories create "${ARTIFACT_REPOSITORY}" \
    --repository-format=docker \
    --location "${REGION}" \
    --description="Hive sync container images" \
    --project "${PROJECT_ID}"
fi

INSTANCE_CONNECTION_NAME=""
if [[ "${USE_CLOUD_SQL}" == "true" ]]; then
  INSTANCE_CONNECTION_NAME="$(gcloud sql instances describe "${CLOUD_SQL_INSTANCE}" --project "${PROJECT_ID}" --format='value(connectionName)')"
  if [[ -z "${INSTANCE_CONNECTION_NAME}" ]]; then
    echo "❌ Could not resolve Cloud SQL connection name for ${CLOUD_SQL_INSTANCE}"
    exit 1
  fi
fi

IMAGE_URI="${REGION}-docker.pkg.dev/${PROJECT_ID}/${ARTIFACT_REPOSITORY}/${IMAGE_NAME}:${IMAGE_TAG}"

echo "🏗️  Building and pushing container image: ${IMAGE_URI}"
gcloud builds submit "${REPO_ROOT}" \
  --project "${PROJECT_ID}" \
  --config "${CLOUDBUILD_CONFIG_PATH}" \
  --substitutions "_IMAGE_URI=${IMAGE_URI}"

if ! gcloud iam service-accounts describe "${RUN_SA_EMAIL}" --project "${PROJECT_ID}" >/dev/null 2>&1; then
  echo "❌ Runtime service account not found: ${RUN_SA_EMAIL}"
  echo "   Run scripts/cloud/bootstrap-hive-sync-infra.sh first"
  exit 1
fi

DEPLOY_ARGS=(
  run deploy "${SERVICE_NAME}"
  --project "${PROJECT_ID}"
  --region "${REGION}"
  --platform managed
  --image "${IMAGE_URI}"
  --port "8080"
  --service-account "${RUN_SA_EMAIL}"
  --min-instances "${MIN_INSTANCES}"
  --max-instances "${MAX_INSTANCES}"
)

SECRETS_SPEC=""
if gcloud secrets describe "${DEVICE_TOKENS_SECRET_NAME}" --project "${PROJECT_ID}" >/dev/null 2>&1; then
  SECRETS_SPEC="HIVE_SYNC_DEVICE_TOKENS=${DEVICE_TOKENS_SECRET_NAME}:latest"
fi
if [[ -n "${SECRETS_SPEC}" ]]; then
  DEPLOY_ARGS+=(--set-secrets "${SECRETS_SPEC}")
fi
if [[ "${USE_CLOUD_SQL}" == "true" ]]; then
  DEPLOY_ARGS+=(--add-cloudsql-instances "${INSTANCE_CONNECTION_NAME}")
fi

DEPLOY_ARGS+=(--set-env-vars "HIVE_ADDR=0.0.0.0:8080,HIVE_SYNC_PERMISSIVE_CORS=${PERMISSIVE_CORS}")

if [[ "${ALLOW_UNAUTHENTICATED}" == "true" ]]; then
  DEPLOY_ARGS+=(--allow-unauthenticated)
else
  DEPLOY_ARGS+=(--no-allow-unauthenticated)
fi

echo "🚀 Deploying Cloud Run service..."
gcloud "${DEPLOY_ARGS[@]}"

SERVICE_URL="$(gcloud run services describe "${SERVICE_NAME}" --project "${PROJECT_ID}" --region "${REGION}" --format='value(status.url)')"

echo
echo "✅ Deployment complete"
echo "Service URL: ${SERVICE_URL}"
echo "Image:       ${IMAGE_URI}"
echo
echo "Health check:"
echo "  curl ${SERVICE_URL}/healthz"
echo
echo "Sync doctor remote check:"
echo "  hive sync doctor --endpoint ${SERVICE_URL} --token <device-token>"
