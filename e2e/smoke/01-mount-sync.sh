#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_path="${TMPDIR}/mount-01-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")
assert_node_at_path "$folder_id" "/"
