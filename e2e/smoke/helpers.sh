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

daemon() {
  local method="$1"
  local path="$2"
  local home_var="$3"
  local body="${4:-}"
  local home_dir="${!home_var}"
  local socket="${home_dir}/.local/share/valv/valvd.sock"

  if [ -n "$body" ]; then
    curl -fsS -X "$method" --unix-socket "$socket" "http://localhost${path}" \
      -H "Content-Type: application/json" \
      --data "$body"
  else
    curl -fsS -X "$method" --unix-socket "$socket" "http://localhost${path}"
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
  local folder_id="${2:-}"
  local mount_path=""
  if [ -n "$folder_id" ]; then
    mount_path=$(sqlite3 "${home_dir}/.local/share/valv/sync.db" "SELECT path FROM mounts WHERE folder_id = '${folder_id}' LIMIT 1" 2>/dev/null || true)
  fi
  local deadline=$((SECONDS + 30))
  while [ "$SECONDS" -lt "$deadline" ]; do
    local status
    status=$(HOME="$home_dir" "$VALV_BIN" status 2>/dev/null || true)
    if [ -n "$status" ]; then
      if [ -n "$mount_path" ]; then
        local mount_status
        mount_status=$(printf '%s\n' "$status" | awk -v path="$mount_path" 'BEGIN { FS = "\t" } $1 == path { print; found = 1 } END { if (!found) exit 1 }' || true)
        if [ -n "$mount_status" ] && ! printf '%s\n' "$mount_status" | grep -q $'\ttrue\t'; then
          return 0
        fi
      elif ! printf '%s\n' "$status" | grep -q $'\ttrue\t'; then
        return 0
      fi
    fi
    sleep 0.5
  done
  HOME="$home_dir" "$VALV_BIN" status >&2 || true
  fail "daemon did not become idle within 30s"
}

sync_mount() {
  local home_dir="${1:-$HOME_A}"
  local folder_id="${2:-}"
  if [ -n "$folder_id" ]; then
    HOME="$home_dir" "$VALV_BIN" sync --folder "$folder_id" >/dev/null
  else
    HOME="$home_dir" "$VALV_BIN" sync >/dev/null
  fi
  wait_for_idle "$home_dir" "$folder_id"
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

assert_path_absent() {
  local path="$1"
  [ ! -e "$path" ] || fail "expected path absent: ${path}"
}

assert_path_present() {
  local path="$1"
  [ -e "$path" ] || fail "expected path present: ${path}"
}

wait_for_path_absent() {
  local path="$1"
  local timeout_s="${2:-30}"
  local deadline=$((SECONDS + timeout_s))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if [ ! -e "$path" ]; then
      return 0
    fi
    sleep 0.5
  done
  fail "path still exists after ${timeout_s}s: ${path}"
}

live_node_count_at_path() {
  local folder_id="$1"
  local path="$2"
  tree_json "$folder_id" | node -e '
    const fs = require("fs");
    const target = process.argv[1];
    const data = JSON.parse(fs.readFileSync(0, "utf8"));
    const byParent = new Map();
    for (const node of data.nodes || []) {
      const key = node.parent_id || "__root__";
      if (!byParent.has(key)) byParent.set(key, []);
      byParent.get(key).push(node);
    }
    let matches = (byParent.get("__root__") || []).filter((node) => !node.deleted_at);
    if (target !== "/") {
      for (const part of target.split("/").filter(Boolean)) {
        matches = matches.flatMap((node) =>
          (byParent.get(node.node_id) || []).filter((child) => child.name === part && !child.deleted_at),
        );
      }
    }
    process.stdout.write(String(matches.length));
  ' "$path"
}

assert_live_node_count_at_path() {
  local folder_id="$1"
  local path="$2"
  local expected="$3"
  local actual
  actual=$(live_node_count_at_path "$folder_id" "$path")
  [ "$actual" = "$expected" ] || fail "expected ${expected} live nodes at ${path}, found ${actual}"
}

node_id_at_path() {
  local folder_id="$1"
  local path="$2"
  node_json_at_path "$folder_id" "$path" | json_eval "process.stdout.write(data.node_id)"
}

get_node_id_at_path() {
  local folder_id="$1"
  local path="$2"
  local node_id
  node_id=$(node_id_at_path "$folder_id" "$path") || fail "node not found at ${path} in folder ${folder_id}"
  [ -n "$node_id" ] || fail "node not found at ${path} in folder ${folder_id}"
  printf '%s\n' "$node_id"
}

assert_nodes_at_paths() {
  local folder_id="$1"
  shift
  local path
  for path in "$@"; do
    assert_node_at_path "$folder_id" "$path"
  done
}

wait_for_file_on_device() {
  local mount_path="$1"
  local filename="$2"
  local timeout_s="${3:-30}"
  local deadline=$((SECONDS + timeout_s))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if assert_path_present "${mount_path}/${filename}" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  fail "file ${filename} did not appear under ${mount_path} within ${timeout_s}s"
}

wait_for_file_content() {
  local mount_path="$1"
  local filename="$2"
  local expected="$3"
  local timeout_s="${4:-30}"
  local deadline=$((SECONDS + timeout_s))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if grep -qF "$expected" "${mount_path}/${filename}" 2>/dev/null; then
      return 0
    fi
    sleep 0.5
  done
  fail "timeout waiting for '${expected}' in ${mount_path}/${filename}"
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

api_create_node() {
  local folder_id="$1"
  local parent_path="$2"
  local name="$3"
  local node_type="$4"
  local parent_id node_id response
  parent_id=$(node_id_at_path "$folder_id" "$parent_path")
  node_id=$(uuid)
  response=$(api POST "/api/folders/${folder_id}/ops" "{\"op_type\":\"create\",\"payload\":{\"node_id\":\"${node_id}\",\"parent_id\":\"${parent_id}\",\"name\":$(json_string "$name"),\"type\":\"${node_type}\"}}")
  printf '%s\t%s\n' \
    "$(printf '%s' "$response" | json_eval "process.stdout.write(data.node_id)")" \
    "$(printf '%s' "$response" | json_eval "process.stdout.write(String(data.server_seq))")"
}

api_create_file_with_content() {
  local folder_id="$1"
  local parent_path="$2"
  local name="$3"
  local content="$4"
  local node_id based_on_seq tmp_file hash size version_id content_hash
  read -r node_id based_on_seq < <(api_create_node "$folder_id" "$parent_path" "$name" file)
  tmp_file="${TMPDIR}/api-file-${node_id}"
  printf '%s' "$content" > "$tmp_file"
  read -r hash size < <(upload_file_for_api_version "$tmp_file")
  version_id=$(uuid)
  content_hash=$(manifest_content_hash "$hash")
  api POST "/api/folders/${folder_id}/ops" "{\"op_type\":\"new_version\",\"node_id\":\"${node_id}\",\"based_on_seq\":${based_on_seq},\"payload\":{\"version_id\":\"${version_id}\",\"content_hash\":\"${content_hash}\",\"size_bytes\":${size},\"manifest\":[{\"chunk_hash\":\"${hash}\",\"offset\":0,\"length\":${size}}]}}" >/dev/null
  printf '%s\n' "$node_id"
}

api_delete_node_at_path() {
  local folder_id="$1"
  local path="$2"
  local node_id based_on_seq
  node_id=$(node_id_at_path "$folder_id" "$path")
  based_on_seq=$(node_seq_at_path "$folder_id" "$path")
  api POST "/api/folders/${folder_id}/ops" "{\"op_type\":\"delete\",\"node_id\":\"${node_id}\",\"based_on_seq\":${based_on_seq},\"payload\":{}}" >/dev/null
}
