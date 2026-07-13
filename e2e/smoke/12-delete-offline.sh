#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT

start_daemon HOME_A DAEMON_PID_A

mount_path="${TMPDIR}/mount-12-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")

printf 'paused\n' > "${mount_path}/paused.txt"
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/paused.txt"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
rm "${mount_path}/paused.txt"
sleep 2
assert_node_at_path "$folder_id" "/paused.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
wait_for_deleted_node_at_path "$folder_id" "/paused.txt"
sync_mount "$HOME_A"
assert_path_absent "${mount_path}/paused.txt"

printf 'offline\n' > "${mount_path}/offline.txt"
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/offline.txt"

stop_daemon DAEMON_PID_A
rm "${mount_path}/offline.txt"
start_daemon HOME_A DAEMON_PID_A
sync_mount "$HOME_A"
wait_for_deleted_node_at_path "$folder_id" "/offline.txt"
sync_mount "$HOME_A"
assert_path_absent "${mount_path}/offline.txt"
