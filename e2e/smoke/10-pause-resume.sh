#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT
start_daemon HOME_A DAEMON_PID_A

mount_path="${TMPDIR}/mount-10-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
printf 'paused content\n' > "${mount_path}/paused.txt"
sleep 2
assert_no_live_node_at_path "$folder_id" "/paused.txt"

HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"
assert_node_at_path "$folder_id" "/paused.txt"
