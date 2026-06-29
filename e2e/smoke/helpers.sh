#!/usr/bin/env bash

set -euo pipefail

fail() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

api() {
  local method="$1"
  local path="$2"
  local body="${3:-}"
  local url="${BACKEND_URL}${path}"

  if [ -n "$body" ]; then
    curl -fsS -X "$method" "$url" \
      -H "Authorization: Bearer ${DEVICE_TOKEN_A}" \
      -H "Content-Type: application/json" \
      --data "$body"
  else
    curl -fsS -X "$method" "$url" \
      -H "Authorization: Bearer ${DEVICE_TOKEN_A}"
  fi
}

api_with_token() {
  local token="$1"
  local method="$2"
  local path="$3"
  local body="${4:-}"
  local url="${BACKEND_URL}${path}"

  if [ -n "$body" ]; then
    curl -fsS -X "$method" "$url" \
      -H "Authorization: Bearer ${token}" \
      -H "Content-Type: application/json" \
      --data "$body"
  else
    curl -fsS -X "$method" "$url" \
      -H "Authorization: Bearer ${token}"
  fi
}

json_eval() {
  local expr="$1"
  node -e "const fs = require('fs'); const data = JSON.parse(fs.readFileSync(0, 'utf8')); ${expr}"
}

json_string() {
  local value="$1"
  node -e "process.stdout.write(JSON.stringify(process.argv[1]))" "$value"
}

uuid() {
  node -e "const { randomUUID } = require('crypto'); process.stdout.write(randomUUID())"
}

sha256_hex() {
  local file="$1"
  node -e "const fs = require('fs'); const crypto = require('crypto'); process.stdout.write(crypto.createHash('sha256').update(fs.readFileSync(process.argv[1])).digest('hex'))" "$file"
}

manifest_content_hash() {
  node -e "const crypto = require('crypto'); const hashes = process.argv.slice(1); const h = crypto.createHash('sha256'); for (const hash of hashes) h.update(hash); process.stdout.write(h.digest('hex'))" "$@"
}

write_device_config() {
  local home_dir="$1"
  local device_id="$2"
  local token="$3"
  local device_name="$4"
  mkdir -p "${home_dir}/.config/valv" "${home_dir}/.local/share/valv"
  cat > "${home_dir}/.config/valv/config.toml" <<EOF
backend_url = "${BACKEND_URL}"
device_id = "${device_id}"
device_token = "${token}"
device_name = "${device_name}"
EOF
}

register_device() {
  local name="$1"
  curl -fsS -X POST "${BACKEND_URL}/auth/device" \
    -H "Cookie: ${SESSION_COOKIE_A}" \
    -H "Content-Type: application/json" \
    --data "{\"name\":$(json_string "$name")}" \
    | json_eval "process.stdout.write(data.device_id + '\\t' + data.token)"
}

start_daemon() {
  local home_var="$1"
  local pid_var="$2"
  local home_dir="${!home_var}"
  local socket="${home_dir}/.local/share/valv/valvd.sock"
  mkdir -p "${home_dir}/.local/share/valv"

  HOME="$home_dir" "$VALVD_BIN" run > "${TMPDIR}/${pid_var}.log" 2>&1 &
  local pid=$!
  printf -v "$pid_var" '%s' "$pid"

  for _ in $(seq 1 100); do
    if [ -S "$socket" ]; then
      return 0
    fi
    sleep 0.1
  done

  kill "$pid" 2>/dev/null || true
  fail "valvd did not create socket at ${socket}; see ${TMPDIR}/${pid_var}.log"
}

stop_daemon() {
  local pid_var="$1"
  local pid="${!pid_var:-}"
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  printf -v "$pid_var" ''
}

wait_for_idle() {
  local home_dir="${1:-$HOME_A}"
  local deadline=$((SECONDS + 30))
  while [ "$SECONDS" -lt "$deadline" ]; do
    local status
    status=$(HOME="$home_dir" "$VALV_BIN" status 2>/dev/null || true)
    if [ -n "$status" ] && ! printf '%s\n' "$status" | grep -q $'\ttrue\t'; then
      return 0
    fi
    sleep 0.5
  done
  HOME="$home_dir" "$VALV_BIN" status >&2 || true
  fail "daemon did not become idle within 30s"
}

sync_mount() {
  local home_dir="${1:-$HOME_A}"
  HOME="$home_dir" "$VALV_BIN" sync >/dev/null
  wait_for_idle "$home_dir"
}

mount_folder() {
  local home_dir="$1"
  local mount_path="$2"
  shift 2
  mkdir -p "$mount_path"
  local output
  output=$(HOME="$home_dir" "$VALV_BIN" mount "$mount_path" "$@")
  printf '%s\n' "$output" | node -e "const fs = require('fs'); const text = fs.readFileSync(0, 'utf8'); const m = text.match(/folder ([^ ]+)/); if (!m) process.exit(1); process.stdout.write(m[1]);"
}

assert_bucket_key() {
  local key="$1"
  mc stat "local/${BUCKET_NAME}/${key}" >/dev/null 2>&1 || fail "MinIO key not found: ${key}"
}

tree_json() {
  local folder_id="$1"
  api GET "/api/folders/${folder_id}/tree"
}

node_json_at_path() {
  local folder_id="$1"
  local path="$2"
  tree_json "$folder_id" | node -e '
    const fs = require("fs");
    const target = process.argv[1];
    const data = JSON.parse(fs.readFileSync(0, "utf8"));
    const nodes = data.nodes || [];
    const byParent = new Map();
    for (const node of nodes) {
      const key = node.parent_id || "__root__";
      if (!byParent.has(key)) byParent.set(key, []);
      byParent.get(key).push(node);
    }
    let node = (byParent.get("__root__") || [])[0];
    if (!node) {
      process.exit(1);
    }
    if (target === "/") {
      process.stdout.write(JSON.stringify(node));
      process.exit(0);
    }
    for (const part of target.split("/").filter(Boolean)) {
      node = (byParent.get(node.node_id) || []).find((candidate) => candidate.name === part);
      if (!node) process.exit(1);
    }
    process.stdout.write(JSON.stringify(node));
  ' "$path"
}

assert_node_at_path() {
  local folder_id="$1"
  local path="$2"
  local node
  node=$(node_json_at_path "$folder_id" "$path") || fail "node not found at ${path} in folder ${folder_id}"
  printf '%s' "$node" | json_eval "if (data.deleted_at) process.exit(1)" || fail "node at ${path} is deleted"
}

wait_for_node_at_path() {
  local folder_id="$1"
  local path="$2"
  local deadline=$((SECONDS + 30))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if assert_node_at_path "$folder_id" "$path" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  fail "node did not appear at ${path} in folder ${folder_id}"
}

assert_no_live_node_at_path() {
  local folder_id="$1"
  local path="$2"
  local node
  if ! node=$(node_json_at_path "$folder_id" "$path" 2>/dev/null); then
    return 0
  fi
  printf '%s' "$node" | json_eval "if (!data.deleted_at) process.exit(1)" || fail "unexpected live node at ${path}"
}

wait_for_no_live_node_at_path() {
  local folder_id="$1"
  local path="$2"
  local deadline=$((SECONDS + 30))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if assert_no_live_node_at_path "$folder_id" "$path" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  fail "live node still exists at ${path} in folder ${folder_id}"
}

wait_for_deleted_node_at_path() {
  local folder_id="$1"
  local path="$2"
  local deadline=$((SECONDS + 30))
  while [ "$SECONDS" -lt "$deadline" ]; do
    local node
    if node=$(node_json_at_path "$folder_id" "$path" 2>/dev/null); then
      if printf '%s' "$node" | json_eval "if (!data.deleted_at) process.exit(1)"; then
        return 0
      fi
    fi
    sleep 0.5
  done
  fail "node at ${path} was not tombstoned in folder ${folder_id}"
}

node_id_at_path() {
  local folder_id="$1"
  local path="$2"
  node_json_at_path "$folder_id" "$path" | json_eval "process.stdout.write(data.node_id)"
}

node_seq_at_path() {
  local folder_id="$1"
  local path="$2"
  node_json_at_path "$folder_id" "$path" | json_eval "process.stdout.write(String(data.server_seq))"
}

node_version_at_path() {
  local folder_id="$1"
  local path="$2"
  node_json_at_path "$folder_id" "$path" | json_eval "process.stdout.write(data.current_version_id || '')"
}

version_manifest() {
  local node_id="$1"
  local version_id="$2"
  api GET "/api/folders/${CURRENT_FOLDER_ID}/versions/${node_id}" | node -e '
    const fs = require("fs");
    const versionId = process.argv[1];
    const versions = JSON.parse(fs.readFileSync(0, "utf8"));
    const found = versions.find((version) => version.version_id === versionId) || versions[0];
    if (!found) process.exit(1);
    process.stdout.write(JSON.stringify(found.manifest || []));
  ' "$version_id"
}

first_chunk_hash_for_path() {
  local folder_id="$1"
  local path="$2"
  local node_id version_id
  node_id=$(node_id_at_path "$folder_id" "$path")
  version_id=$(node_version_at_path "$folder_id" "$path")
  CURRENT_FOLDER_ID="$folder_id" version_manifest "$node_id" "$version_id" | json_eval "if (!data[0]) process.exit(1); process.stdout.write(data[0].chunk_hash)"
}

assert_file_contains() {
  local local_path="$1"
  local substring="$2"
  [ -f "$local_path" ] || fail "file not found: ${local_path}"
  grep -Fq "$substring" "$local_path" || fail "file ${local_path} does not contain ${substring}"
}

upload_file_for_api_version() {
  local file="$1"
  local hash size href
  hash=$(sha256_hex "$file")
  size=$(wc -c < "$file" | tr -d ' ')
  href=$(api POST "/api/objects/batch" "{\"operation\":\"upload\",\"objects\":[{\"oid\":\"${hash}\",\"size\":${size}}]}" \
    | json_eval "process.stdout.write(data.objects[0].actions?.upload?.href || '')")
  if [ -n "$href" ]; then
    curl -fsS -X PUT "$href" -H "Content-Type: application/octet-stream" --data-binary "@${file}" >/dev/null
  fi
  printf '%s\t%s\n' "$hash" "$size"
}
