#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A

mount_a="${TMPDIR}/mount-18-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
printf 'v1 content\n' > "${mount_a}/versioned.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
node_id=$(get_node_id_at_path "$folder_id" "/versioned.txt")
HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
printf 'v2 content expanded\n' > "${mount_a}/versioned.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"
HOME="$HOME_A" "$VALV_BIN" pause >/dev/null
printf 'v3 content expanded again\n' > "${mount_a}/versioned.txt"
HOME="$HOME_A" "$VALV_BIN" resume >/dev/null
sync_mount "$HOME_A"

versions=$(api GET "/api/folders/${folder_id}/versions/${node_id}")
version_count=$(printf '%s' "$versions" | json_eval 'process.stdout.write(String(data.length))')
[ "$version_count" = "3" ] || fail "expected 3 versions, got ${version_count}"
printf '%s' "$versions" | json_eval 'if (!data.every((v) => v.is_conflict_copy === false)) process.exit(1)' || fail "expected only canonical versions"
printf '%s' "$versions" | json_eval 'if (!data.every((v) => v.author_device_id && v.created_at && v.size_bytes > 0 && Array.isArray(v.manifest))) process.exit(1)' || fail "version metadata incomplete"

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-18-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B"
assert_file_contains "${mount_b}/versioned.txt" "v3 content expanded again"

version_1_id=$(HOME="$HOME_A" "$VALV_BIN" versions "${mount_a}/versioned.txt" | tail -1 | awk '{print $1}')
[ -n "$version_1_id" ] || fail "could not determine oldest version id"
restore_output=$(HOME="$HOME_A" "$VALV_BIN" restore "${mount_a}/versioned.txt" "$version_1_id")
case "$restore_output" in
  *Restored*) ;;
  *) fail "restore output did not contain Restored: ${restore_output}" ;;
esac
wait_for_file_content "$mount_a" "versioned.txt" "v1 content" 30
assert_file_contains "${mount_a}/versioned.txt" "v1 content"
# `valv versions` prints a 2-line header (column names + separator) before
# one line per version, so skip those to count actual versions.
version_count=$(HOME="$HOME_A" "$VALV_BIN" versions "${mount_a}/versioned.txt" | tail -n +3 | wc -l | tr -d ' ')
[ "$version_count" = "4" ] || fail "expected 4 versions after restore, got ${version_count}"

wait_for_file_content "$mount_b" "versioned.txt" "v1 content" 30
assert_file_contains "${mount_b}/versioned.txt" "v1 content"
