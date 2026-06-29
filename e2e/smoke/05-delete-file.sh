#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT
start_daemon HOME_A DAEMON_PID_A

mount_path="${TMPDIR}/mount-05-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")
file="${mount_path}/delete-me.txt"
printf 'delete me\n' > "$file"
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/delete-me.txt"

rm "$file"
sync_mount "$HOME_A"
wait_for_deleted_node_at_path "$folder_id" "/delete-me.txt"
