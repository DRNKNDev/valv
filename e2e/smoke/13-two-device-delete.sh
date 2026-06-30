#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A

mount_a="${TMPDIR}/mount-13-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
printf 'shared\n' > "${mount_a}/shared.txt"
mkdir -p "${mount_a}/shared-dir"
printf 'inner\n' > "${mount_a}/shared-dir/inner.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-13-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B"
assert_path_present "${mount_b}/shared.txt"
assert_path_present "${mount_b}/shared-dir/inner.txt"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
rm "${mount_a}/shared.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
sync_mount "$HOME_B"
assert_path_absent "${mount_b}/shared.txt"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
rm -rf "${mount_a}/shared-dir"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
sync_mount "$HOME_B"
assert_path_absent "${mount_b}/shared-dir"

api_create_file_with_content "$folder_id" "/" "undownloaded.txt" "remote only\n" >/dev/null
sleep 1
wait_for_idle "$HOME_B"
stop_daemon DAEMON_PID_B
start_daemon HOME_B DAEMON_PID_B
sync_mount "$HOME_B"
assert_node_at_path "$folder_id" "/undownloaded.txt"
