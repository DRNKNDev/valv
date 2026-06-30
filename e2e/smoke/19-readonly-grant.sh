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
mount_a="${TMPDIR}/mount-19-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
mkdir -p "${mount_a}/read-target"
printf 'top secret\n' > "${mount_a}/read-target/secret.txt"
sync_mount "$HOME_A"

scope_node_id=$(get_node_id_at_path "$folder_id" "/read-target")
grant=$(api POST "/api/folders/${folder_id}/grants" "{\"scope_node_id\":\"${scope_node_id}\",\"name\":\"Read Only Smoke\",\"can_read\":true,\"can_write\":false}")
DEVICE_ID_C=$(printf '%s' "$grant" | json_eval 'process.stdout.write(data.device_id)')
DEVICE_TOKEN_C=$(printf '%s' "$grant" | json_eval 'process.stdout.write(data.token)')
write_device_config "$HOME_C" "$DEVICE_ID_C" "$DEVICE_TOKEN_C" "Smoke Device C"

start_daemon HOME_C DAEMON_PID_C
mount_c="${TMPDIR}/mount-19-c"
mount_folder "$HOME_C" "$mount_c" --grant "$DEVICE_TOKEN_C" >/dev/null
sync_mount "$HOME_C"
assert_file_contains "${mount_c}/secret.txt" "top secret"

printf 'injected\n' > "${mount_c}/attempt.txt"
HOME="$HOME_C" "$VALV_BIN" sync >/dev/null 2>&1 || true
assert_no_live_node_at_path "$folder_id" "/read-target/attempt.txt"

HOME="$HOME_C" "$VALV_BIN" pause >/dev/null
mv "${mount_c}/secret.txt" "${mount_c}/renamed.txt"
HOME="$HOME_C" "$VALV_BIN" resume >/dev/null
HOME="$HOME_C" "$VALV_BIN" sync >/dev/null 2>&1 || true
assert_node_at_path "$folder_id" "/read-target/secret.txt"
assert_no_live_node_at_path "$folder_id" "/read-target/renamed.txt"
