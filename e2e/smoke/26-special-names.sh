#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-26-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-26-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null

printf 'spaces\n' > "${mount_a}/my file.txt"
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/my file.txt"
sync_mount "$HOME_B"
assert_file_contains "${mount_b}/my file.txt" "spaces"

printf 'hidden\n' > "${mount_a}/.hidden"
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/.hidden"
sync_mount "$HOME_B"
assert_path_present "${mount_b}/.hidden"

printf 'café\n' > "${mount_a}/café.txt"
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/café.txt"
sync_mount "$HOME_B"
ls "${mount_b}" | grep -qF "café.txt" || fail "unicode filename missing on Device B"

LONG=$(python3 -c "print('a' * 200, end='')")
printf 'long\n' > "${mount_a}/${LONG}.txt"
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/${LONG}.txt"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mv "${mount_a}/my file.txt" "${mount_a}/my renamed file.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/my renamed file.txt"
assert_no_live_node_at_path "$folder_id" "/my file.txt"
sync_mount "$HOME_B"
assert_path_present "${mount_b}/my renamed file.txt"
assert_path_absent "${mount_b}/my file.txt"
