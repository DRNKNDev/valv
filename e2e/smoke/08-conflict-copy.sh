#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT
start_daemon HOME_A DAEMON_PID_A

mount_path="${TMPDIR}/mount-08-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")
file="${mount_path}/report.txt"
printf 'original\n' > "$file"
sync_mount "$HOME_A" "$folder_id"

node_id=$(node_id_at_path "$folder_id" "/report.txt")
based_on_seq=$(node_seq_at_path "$folder_id" "/report.txt")

HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
printf 'local edit\n' > "$file"

remote_file="${TMPDIR}/remote-conflict.txt"
printf 'remote edit\n' > "$remote_file"
read -r remote_hash remote_size < <(upload_file_for_api_version "$remote_file")
remote_version_id=$(uuid)
remote_content_hash=$(manifest_content_hash "$remote_hash")
api POST "/api/folders/${folder_id}/ops" "{\"op_type\":\"new_version\",\"node_id\":\"${node_id}\",\"based_on_seq\":${based_on_seq},\"payload\":{\"version_id\":\"${remote_version_id}\",\"content_hash\":\"${remote_content_hash}\",\"size_bytes\":${remote_size},\"manifest\":[{\"chunk_hash\":\"${remote_hash}\",\"offset\":0,\"length\":${remote_size}}]}}" >/dev/null

HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A" "$folder_id"

[ -f "$file" ] || fail "original file missing after conflict"
conflicts=$(find "$mount_path" -maxdepth 1 -type f -name '*conflicted*' -print)
[ -n "$conflicts" ] || fail "conflict copy was not materialized"
