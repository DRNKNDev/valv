#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT
start_daemon HOME_A DAEMON_PID_A

mount_path="${TMPDIR}/mount-06-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")
printf 'rename me\n' > "${mount_path}/original.txt"
sync_mount "$HOME_A" "$folder_id"

mv "${mount_path}/original.txt" "${mount_path}/renamed.txt"
sync_mount "$HOME_A" "$folder_id"
wait_for_node_at_path "$folder_id" "/renamed.txt"
wait_for_no_live_node_at_path "$folder_id" "/original.txt"
