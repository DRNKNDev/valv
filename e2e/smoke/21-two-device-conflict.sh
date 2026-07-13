#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-21-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
printf 'original content\n' > "${mount_a}/shared.txt"
sync_mount "$HOME_A"

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-21-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B"
assert_file_contains "${mount_b}/shared.txt" "original content"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
HOME="$HOME_B" "$VALV_BIN" pause >/dev/null
printf 'content from A\n' > "${mount_a}/shared.txt"
printf 'content from B\n' > "${mount_b}/shared.txt"

HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
sleep 1
HOME="$HOME_B" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_B"

conflict_file=$(ls "${mount_b}" | grep -i conflict || true)
[ -n "$conflict_file" ] || fail "no conflict copy found in ${mount_b}"
file_count=$(ls "${mount_b}" | wc -l | tr -d ' ')
[ "$file_count" = "2" ] || fail "expected exactly two files in ${mount_b}, found ${file_count}"
assert_file_contains "${mount_b}/${conflict_file}" "content from B"
assert_file_contains "${mount_b}/shared.txt" "content from A"

sync_mount "$HOME_A"
conflict_file_a=$(ls "${mount_a}" | grep -i conflict || true)
[ -n "$conflict_file_a" ] || fail "no conflict copy found in ${mount_a}"
assert_file_contains "${mount_a}/${conflict_file_a}" "content from B"
