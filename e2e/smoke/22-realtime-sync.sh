#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-22-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-22-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sleep 1

printf 'ws-push content\n' > "${mount_a}/ws-file.txt"
sync_mount "$HOME_A"
wait_for_file_on_device "$mount_b" "ws-file.txt" 30
assert_file_contains "${mount_b}/ws-file.txt" "ws-push content"

printf 'second file\n' > "${mount_a}/ws-file-2.txt"
sync_mount "$HOME_A"
wait_for_file_on_device "$mount_b" "ws-file-2.txt" 30
assert_file_contains "${mount_b}/ws-file-2.txt" "second file"

mount_x="${TMPDIR}/mount-22-x"
mount_folder "$HOME_A" "$mount_x" >/dev/null
count_before=$(ls "${mount_b}" | wc -l | tr -d ' ')
printf 'folder-x content\n' > "${mount_x}/x-file.txt"
sync_mount "$HOME_A"
sleep 2
count_after=$(ls "${mount_b}" | wc -l | tr -d ' ')
[ "$count_before" = "$count_after" ] || fail "Device B mount changed after out-of-folder op"
