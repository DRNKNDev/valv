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
mkdir -p "${mount_path}/subdir"
printf 'delete nested\n' > "${mount_path}/subdir/nested.txt"
sync_mount "$HOME_A" "$folder_id"
assert_node_at_path "$folder_id" "/delete-me.txt"
assert_node_at_path "$folder_id" "/subdir/nested.txt"

rm "$file"
rm -rf "${mount_path}/subdir"
sync_mount "$HOME_A" "$folder_id"
wait_for_deleted_node_at_path "$folder_id" "/delete-me.txt"
wait_for_deleted_node_at_path "$folder_id" "/subdir/nested.txt"

sync_mount "$HOME_A" "$folder_id"
assert_path_absent "${mount_path}/delete-me.txt"
assert_path_absent "${mount_path}/subdir"
