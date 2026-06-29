#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-07-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
printf 'hello world\n' > "${mount_a}/hello.txt"
sync_mount "$HOME_A"
assert_node_at_path "$folder_id" "/hello.txt"

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-07-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B"

assert_file_contains "${mount_b}/hello.txt" "hello world"
