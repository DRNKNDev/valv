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
mount_a="${TMPDIR}/mount-09-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
mkdir -p "${mount_a}/subdir-a" "${mount_a}/subdir-b"
printf 'inside\n' > "${mount_a}/subdir-a/inside.txt"
printf 'outside\n' > "${mount_a}/subdir-b/outside.txt"
sync_mount "$HOME_A"

scope_node_id=$(node_id_at_path "$folder_id" "/subdir-a")
grant=$(api POST "/api/folders/${folder_id}/grants" "{\"scope_node_id\":\"${scope_node_id}\",\"name\":\"Scoped Smoke Device\",\"can_read\":true,\"can_write\":true}")
DEVICE_ID_C=$(printf '%s' "$grant" | json_eval "process.stdout.write(data.device_id)")
DEVICE_TOKEN_C=$(printf '%s' "$grant" | json_eval "process.stdout.write(data.token)")
write_device_config "$HOME_C" "$DEVICE_ID_C" "$DEVICE_TOKEN_C" "Smoke Device C"

start_daemon HOME_C DAEMON_PID_C
mount_c="${TMPDIR}/mount-09-c"
mount_folder "$HOME_C" "$mount_c" --grant "$DEVICE_TOKEN_C" >/dev/null
sync_mount "$HOME_C"

[ ! -e "${mount_c}/subdir-b/outside.txt" ] || fail "scoped mount contains out-of-scope file"
