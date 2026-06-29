#!/usr/bin/env bash

set -euo pipefail

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  printf 'harness.sh must be sourced by run-all.sh\n' >&2
  exit 1
fi

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "${SMOKE_DIR}/../../.." && pwd)
REAL_HOME=${HOME:-}

source "${SMOKE_DIR}/helpers.sh"

find_tool() {
  local name="$1"
  local fallback="$2"
  if command -v "$name" >/dev/null 2>&1; then
    command -v "$name"
    return 0
  fi
  if [ -x "$fallback" ]; then
    printf '%s\n' "$fallback"
    return 0
  fi
  return 1
}

require_tools() {
  TSX_BIN="${REPO_ROOT}/oss/core/node_modules/.bin/tsx"
  [ -x "$TSX_BIN" ] || fail "tsx is required at oss/core/node_modules/.bin/tsx. Run: cd oss && pnpm install"

  if ! command -v mc >/dev/null 2>&1; then
    fail "mc (MinIO Client) is required. Install: brew install minio/stable/mc"
  fi

  VALVD_BIN=$(find_tool valvd "${REPO_ROOT}/oss/crates/target/debug/valvd") \
    || fail "valvd is required. Build it with: cd oss/crates && cargo build"
  VALV_BIN=$(find_tool valv "${REPO_ROOT}/oss/crates/target/debug/valv") \
    || fail "valv is required. Build it with: cd oss/crates && cargo build"
  export TSX_BIN VALVD_BIN VALV_BIN
}

apply_sqlite_migrations() {
  local db_path="$1"
  node - "$db_path" "$REPO_ROOT" <<'NODE'
const fs = require("fs");
const path = require("path");
const dbPath = process.argv[2];
const repoRoot = process.argv[3];
const Database = require(path.join(repoRoot, "oss/core/node_modules/better-sqlite3"));
const migrationsDir = path.join(repoRoot, "oss/core/src/db/migrations/sqlite");
const db = new Database(dbPath);
for (const file of fs.readdirSync(migrationsDir).filter((name) => name.endsWith(".sql")).sort()) {
  db.exec(fs.readFileSync(path.join(migrationsDir, file), "utf8"));
}
db.close();
NODE
}

wait_for_backend() {
  for _ in $(seq 1 60); do
    if curl -fsS "${BACKEND_URL}/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  [ -f "${TMPDIR}/backend.log" ] && tail -n 80 "${TMPDIR}/backend.log" >&2 || true
  fail "backend did not become healthy at ${BACKEND_URL}/health"
}

start_backend() {
  export VALV_DATABASE_URL="file:${TMPDIR}/backend.db"
  export VALV_AUTH_SECRET="12345678901234567890123456789012"
  export VALV_PORT="14747"
  export VALV_BASE_URL="${BACKEND_URL}"
  export BUCKET_NAME
  export BUCKET_ENDPOINT="http://localhost:9000"
  export BUCKET_ACCESS_KEY_ID="minioadmin"
  export BUCKET_SECRET_ACCESS_KEY="minioadmin"
  export BUCKET_FORCE_PATH_STYLE="true"

  apply_sqlite_migrations "${TMPDIR}/backend.db"
  HOME="$REAL_HOME" "$TSX_BIN" "${REPO_ROOT}/oss/core/src/server.ts" > "${TMPDIR}/backend.log" 2>&1 &
  BACKEND_PID=$!
  export BACKEND_PID
  wait_for_backend
}

register_primary_devices() {
  local headers body
  headers="${TMPDIR}/signup.headers"
  body="${TMPDIR}/signup.body"
  curl -fsS -D "$headers" -o "$body" -X POST "${BACKEND_URL}/api/auth/sign-up/email" \
    -H "Content-Type: application/json" \
    --data '{"name":"Smoke User","email":"smoke@example.com","password":"password1234"}'
  SESSION_COOKIE_A=$(node -e '
    const fs = require("fs");
    const headers = fs.readFileSync(process.argv[1], "utf8");
    const cookie = headers.split(/\r?\n/).find((line) => /^set-cookie:/i.test(line));
    if (!cookie) process.exit(1);
    process.stdout.write(cookie.replace(/^set-cookie:\s*/i, "").split(";")[0]);
  ' "$headers")
  export SESSION_COOKIE_A

  local device
  device=$(register_device "Smoke Device A")
  DEVICE_ID_A=${device%%$'\t'*}
  DEVICE_TOKEN_A=${device#*$'\t'}
  device=$(register_device "Smoke Device B")
  DEVICE_ID_B=${device%%$'\t'*}
  DEVICE_TOKEN_B=${device#*$'\t'}
  export DEVICE_ID_A DEVICE_TOKEN_A DEVICE_ID_B DEVICE_TOKEN_B
}

cleanup() {
  local status=$?
  if [ -n "${BACKEND_PID:-}" ] && kill -0 "$BACKEND_PID" 2>/dev/null; then
    kill "$BACKEND_PID" 2>/dev/null || true
    wait "$BACKEND_PID" 2>/dev/null || true
  fi
  if [ -n "${BUCKET_NAME:-}" ]; then
    mc rb --force "local/${BUCKET_NAME}" >/dev/null 2>&1 || true
  fi
  if [ -n "${TMPDIR:-}" ] && [[ "$TMPDIR" == /tmp/valv-e2e-* ]]; then
    rm -rf "$TMPDIR"
  fi
  exit "$status"
}

require_tools

RUN_ID=$(node -e "process.stdout.write(require('crypto').randomUUID().slice(0, 8))")
TMPDIR=$(mktemp -d /tmp/valv-e2e-XXXX)
BACKEND_URL="http://localhost:14747"
BUCKET_NAME="valv-smoke-${RUN_ID}"
HOME_A="${TMPDIR}/home-a"
HOME_B="${TMPDIR}/home-b"
export RUN_ID TMPDIR BACKEND_URL BUCKET_NAME HOME_A HOME_B

trap cleanup EXIT

mc alias set local http://localhost:9000 minioadmin minioadmin >/dev/null
mc mb "local/${BUCKET_NAME}" >/dev/null

start_backend
register_primary_devices
write_device_config "$HOME_A" "$DEVICE_ID_A" "$DEVICE_TOKEN_A" "Smoke Device A"
write_device_config "$HOME_B" "$DEVICE_ID_B" "$DEVICE_TOKEN_B" "Smoke Device B"
