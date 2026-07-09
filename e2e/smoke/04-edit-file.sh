#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT
start_daemon HOME_A DAEMON_PID_A

mount_path="${TMPDIR}/mount-04-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")
file="${mount_path}/notes.txt"
printf 'hello\n' > "$file"
sync_mount "$HOME_A" "$folder_id"
printf 'world\n' >> "$file"
sync_mount "$HOME_A" "$folder_id"

node_id=$(node_id_at_path "$folder_id" "/notes.txt")
versions=$(api GET "/api/folders/${folder_id}/versions/${node_id}")
printf '%s' "$versions" | json_eval "if (!Array.isArray(data) || data.length !== 2) process.exit(1)"
chunk_hash=$(first_chunk_hash_for_path "$folder_id" "/notes.txt")
assert_bucket_key "$(expected_chunk_key "$chunk_hash")"
