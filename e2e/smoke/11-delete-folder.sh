#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT

start_daemon HOME_A DAEMON_PID_A

mount_path="${TMPDIR}/mount-11-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mkdir -p "${mount_path}/tree/a" "${mount_path}/tree/b"
printf 'deep\n' > "${mount_path}/tree/a/deep.txt"
printf 'other\n' > "${mount_path}/tree/b/other.txt"
printf 'keep\n' > "${mount_path}/sibling-kept.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
rm -rf "${mount_path}/tree"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"
wait_for_deleted_node_at_path "$folder_id" "/tree"
wait_for_no_live_node_at_path "$folder_id" "/tree/a"
wait_for_no_live_node_at_path "$folder_id" "/tree/a/deep.txt"

sync_mount "$HOME_A" "$folder_id"
assert_path_absent "${mount_path}/tree"

assert_node_at_path "$folder_id" "/sibling-kept.txt"
assert_path_present "${mount_path}/sibling-kept.txt"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
rm "${mount_path}/sibling-kept.txt"
printf 'new content\n' > "${mount_path}/sibling-kept.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"
assert_node_at_path "$folder_id" "/sibling-kept.txt"
assert_live_node_count_at_path "$folder_id" "/sibling-kept.txt" 1
