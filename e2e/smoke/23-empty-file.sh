#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-23-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")

touch "${mount_a}/empty.txt"
sync_mount "$HOME_A" "$folder_id"
assert_node_at_path "$folder_id" "/empty.txt"
node_id=$(get_node_id_at_path "$folder_id" "/empty.txt")
version=$(api GET "/api/folders/${folder_id}/versions/${node_id}" | json_eval 'process.stdout.write(JSON.stringify(data[0]))')
size=$(printf '%s' "$version" | json_eval 'process.stdout.write(String(data.size_bytes))')
manifest_len=$(printf '%s' "$version" | json_eval 'process.stdout.write(String((data.manifest || []).length))')
[ "$size" = "0" ] || fail "expected empty file size 0, got ${size}"
[ "$manifest_len" = "0" ] || fail "expected empty manifest [], got length ${manifest_len}"
printf 'zero-byte manifest: []\n'

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-23-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B" "$folder_id"
assert_path_present "${mount_b}/empty.txt"
bytes=$(wc -c < "${mount_b}/empty.txt" | tr -d ' ')
[ "$bytes" = "0" ] || fail "expected 0 bytes on Device B, got ${bytes}"

printf 'hello' > "${mount_a}/empty.txt"
sync_mount "$HOME_A" "$folder_id"
versions=$(api GET "/api/folders/${folder_id}/versions/${node_id}")
latest=$(printf '%s' "$versions" | json_eval 'process.stdout.write(JSON.stringify(data[0]))')
size=$(printf '%s' "$latest" | json_eval 'process.stdout.write(String(data.size_bytes))')
manifest_len=$(printf '%s' "$latest" | json_eval 'process.stdout.write(String((data.manifest || []).length))')
[ "$size" = "5" ] || fail "expected edited file size 5, got ${size}"
[ "$manifest_len" -ge 1 ] || fail "expected edited file to have chunks"
chunk_hash=$(printf '%s' "$latest" | json_eval 'process.stdout.write(data.manifest[0].chunk_hash)')
assert_bucket_key "$(expected_chunk_key "$chunk_hash")"

truncate -s 0 "${mount_a}/empty.txt"
sync_mount "$HOME_A" "$folder_id"
latest=$(api GET "/api/folders/${folder_id}/versions/${node_id}" | json_eval 'process.stdout.write(JSON.stringify(data[0]))')
size=$(printf '%s' "$latest" | json_eval 'process.stdout.write(String(data.size_bytes))')
manifest_len=$(printf '%s' "$latest" | json_eval 'process.stdout.write(String((data.manifest || []).length))')
[ "$size" = "0" ] || fail "expected truncated file size 0, got ${size}"
[ "$manifest_len" = "0" ] || fail "expected truncated file manifest [], got length ${manifest_len}"
