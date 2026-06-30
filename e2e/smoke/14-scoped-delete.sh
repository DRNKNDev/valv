#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_C=""
HOME_C="${TMPDIR}/home-c"
export HOME_C
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_C' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-14-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
mkdir -p "${mount_a}/scope-dir"
printf 'inside\n' > "${mount_a}/scope-dir/inside.txt"
printf 'outside\n' > "${mount_a}/outside.txt"
sync_mount "$HOME_A"

scope_node_id=$(node_id_at_path "$folder_id" "/scope-dir")
grant=$(api POST "/api/folders/${folder_id}/grants" "{\"scope_node_id\":\"${scope_node_id}\",\"name\":\"Scoped Delete Smoke\",\"can_read\":true,\"can_write\":true}")
DEVICE_ID_C=$(printf '%s' "$grant" | json_eval "process.stdout.write(data.device_id)")
DEVICE_TOKEN_C=$(printf '%s' "$grant" | json_eval "process.stdout.write(data.token)")
write_device_config "$HOME_C" "$DEVICE_ID_C" "$DEVICE_TOKEN_C" "Smoke Device C"

start_daemon HOME_C DAEMON_PID_C
mount_c="${TMPDIR}/mount-14-c"
mount_folder "$HOME_C" "$mount_c" --grant "$DEVICE_TOKEN_C" >/dev/null
sync_mount "$HOME_C"
assert_path_absent "${mount_c}/outside.txt"

rm "${mount_c}/inside.txt"
sync_mount "$HOME_C"
wait_for_deleted_node_at_path "$folder_id" "/scope-dir/inside.txt"
assert_node_at_path "$folder_id" "/outside.txt"

rm -rf "$mount_c"
mkdir -p "$mount_c"
sync_mount "$HOME_C"
assert_node_at_path "$folder_id" "/scope-dir"
