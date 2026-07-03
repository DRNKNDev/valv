#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT
start_daemon HOME_A DAEMON_PID_A

mount_path="${TMPDIR}/mount-03-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")
mkdir -p "${mount_path}/subdir"
sync_mount "$HOME_A" "$folder_id"

node=$(node_json_at_path "$folder_id" "/subdir")
printf '%s' "$node" | json_eval "if (data.type !== 'folder' || data.deleted_at) process.exit(1)"
