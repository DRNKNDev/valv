#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A

mount_a="${TMPDIR}/mount-15-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
printf 'remote delete\n' > "${mount_a}/remote-deleted.txt"
mkdir -p "${mount_a}/tombstoned-dir"
printf 'nested\n' > "${mount_a}/tombstoned-dir/file.txt"
printf 'old content\n' > "${mount_a}/replace-me.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-15-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null

api_delete_node_at_path "$folder_id" "/remote-deleted.txt"
sync_mount "$HOME_B" "$folder_id"
assert_path_absent "${mount_b}/remote-deleted.txt"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
rm -rf "${mount_a}/tombstoned-dir"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"
sync_mount "$HOME_B" "$folder_id"
assert_path_absent "${mount_b}/tombstoned-dir"

HOME="$HOME_B" "$VALV_BIN" pause >/dev/null
read -r empty_node_id _ < <(api_create_node "$folder_id" "/" "empty-dir" folder)
HOME="$HOME_B" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_B" "$folder_id"
assert_path_present "${mount_b}/empty-dir"

empty_seq=$(node_seq_at_path "$folder_id" "/empty-dir")
api POST "/api/folders/${folder_id}/ops" "{\"op_type\":\"delete\",\"node_id\":\"${empty_node_id}\",\"based_on_seq\":${empty_seq},\"payload\":{}}" >/dev/null
sync_mount "$HOME_B" "$folder_id"
assert_path_absent "${mount_b}/empty-dir"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
rm "${mount_a}/replace-me.txt"
printf 'new content\n' > "${mount_a}/replace-me.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"
assert_node_at_path "$folder_id" "/replace-me.txt"
assert_live_node_count_at_path "$folder_id" "/replace-me.txt" 1
