#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
DAEMON_PID_C=""
HOME_C="${TMPDIR}/home-c"
export HOME_C
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B; stop_daemon DAEMON_PID_C' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-24-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mkdir -p "${mount_a}/a/b/c/d/e"
printf 'leaf\n' > "${mount_a}/a/b/c/d/e/leaf.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"
assert_nodes_at_paths "$folder_id" "/a" "/a/b" "/a/b/c" "/a/b/c/d" "/a/b/c/d/e" "/a/b/c/d/e/leaf.txt"

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-24-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B" "$folder_id"
assert_file_contains "${mount_b}/a/b/c/d/e/leaf.txt" "leaf"

scope_node_id=$(get_node_id_at_path "$folder_id" "/a/b/c")
grant=$(api POST "/api/folders/${folder_id}/grants" "{\"scope_node_id\":\"${scope_node_id}\",\"name\":\"Deep Scoped Smoke\",\"can_read\":true,\"can_write\":true}")
DEVICE_ID_C=$(printf '%s' "$grant" | json_eval 'process.stdout.write(data.device_id)')
DEVICE_TOKEN_C=$(printf '%s' "$grant" | json_eval 'process.stdout.write(data.token)')
write_device_config "$HOME_C" "$DEVICE_ID_C" "$DEVICE_TOKEN_C" "Smoke Device C"
start_daemon HOME_C DAEMON_PID_C
mount_c="${TMPDIR}/mount-24-c"
mount_folder "$HOME_C" "$mount_c" --grant "$DEVICE_TOKEN_C" >/dev/null
sync_mount "$HOME_C" "$folder_id"
assert_file_contains "${mount_c}/d/e/leaf.txt" "leaf"
assert_path_absent "${mount_c}/a"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
rm -rf "${mount_a}/a/b/c/d"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"
assert_node_at_path "$folder_id" "/a/b/c"
wait_for_deleted_node_at_path "$folder_id" "/a/b/c/d"
assert_no_live_node_at_path "$folder_id" "/a/b/c/d/e"
assert_no_live_node_at_path "$folder_id" "/a/b/c/d/e/leaf.txt"
