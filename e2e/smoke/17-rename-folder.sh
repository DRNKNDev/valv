#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A

mount_a="${TMPDIR}/mount-17-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mkdir -p "${mount_a}/old-name"
printf 'child content\n' > "${mount_a}/old-name/child.txt"
mkdir -p "${mount_a}/raced-folder"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"

folder_node_id=$(get_node_id_at_path "$folder_id" "/old-name")

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-17-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B"
assert_path_present "${mount_b}/old-name/child.txt"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mv "${mount_a}/old-name" "${mount_a}/new-name"
sleep 1
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/new-name"
assert_node_at_path "$folder_id" "/new-name/child.txt"
assert_no_live_node_at_path "$folder_id" "/old-name"
[ "$folder_node_id" = "$(get_node_id_at_path "$folder_id" "/new-name")" ] || fail "folder node_id changed on rename"

sync_mount "$HOME_B"
assert_path_present "${mount_b}/new-name/child.txt"
assert_path_absent "${mount_b}/old-name"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
HOME="$HOME_B" "$VALV_BIN" pause >/dev/null
mv "${mount_b}/raced-folder" "${mount_b}/name-from-b"
mv "${mount_a}/raced-folder" "${mount_a}/name-from-a"
HOME="$HOME_B" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_B"
assert_node_at_path "$folder_id" "/name-from-b"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/name-from-b"
assert_no_live_node_at_path "$folder_id" "/name-from-a"
