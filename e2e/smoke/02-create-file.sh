#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT

start_daemon HOME_A DAEMON_PID_A

mount_path="${TMPDIR}/mount-02-a"
folder_id=$(mount_folder "$HOME_A" "$mount_path")
printf 'hello world\n' > "${mount_path}/hello.txt"
sync_mount "$HOME_A" "$folder_id"

assert_node_at_path "$folder_id" "/hello.txt"
chunk_hash=$(first_chunk_hash_for_path "$folder_id" "/hello.txt")
assert_bucket_key "$(expected_chunk_key "$chunk_hash")"
