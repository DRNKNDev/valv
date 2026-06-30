#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A

mount_a="${TMPDIR}/mount-16-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mkdir -p "${mount_a}/dir-a" "${mount_a}/dir-b"
printf 'move me\n' > "${mount_a}/dir-a/mover.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"

node_id_before=$(get_node_id_at_path "$folder_id" "/dir-a/mover.txt")
HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mv "${mount_a}/dir-a/mover.txt" "${mount_a}/dir-b/mover.txt"
sleep 1
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/dir-b/mover.txt"
assert_no_live_node_at_path "$folder_id" "/dir-a/mover.txt"
node_id_after=$(get_node_id_at_path "$folder_id" "/dir-b/mover.txt")
printf 'node_id: %s -> %s\n' "$node_id_before" "$node_id_after"
[ "$node_id_before" = "$node_id_after" ] || fail "node_id changed on move"

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-16-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B"
assert_path_present "${mount_b}/dir-b/mover.txt"
assert_path_absent "${mount_b}/dir-a/mover.txt"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mv "${mount_a}/dir-b/mover.txt" "${mount_a}/dir-b/moved-renamed.txt"
sleep 1
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/dir-b/moved-renamed.txt"
assert_no_live_node_at_path "$folder_id" "/dir-b/mover.txt"

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mkdir -p "${mount_a}/parent/child" "${mount_a}/parent2"
printf 'deep\n' > "${mount_a}/parent/child/deep.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
child_id_before=$(get_node_id_at_path "$folder_id" "/parent/child")

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
mv "${mount_a}/parent/child" "${mount_a}/parent2/child"
sleep 1
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/parent2/child/deep.txt"
assert_no_live_node_at_path "$folder_id" "/parent/child/deep.txt"
child_id_after=$(get_node_id_at_path "$folder_id" "/parent2/child")
[ "$child_id_before" = "$child_id_after" ] || fail "folder node_id changed on move"

sync_mount "$HOME_B"
assert_path_present "${mount_b}/parent2/child/deep.txt"
assert_path_absent "${mount_b}/parent/child/deep.txt"
