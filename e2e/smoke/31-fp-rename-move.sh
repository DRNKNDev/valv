#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

daemon_status() {
  local method="$1"
  local path="$2"
  local home_var="$3"
  local body="$4"
  local output_file="$5"
  local home_dir="${!home_var}"
  local socket="${home_dir}/.local/share/valv/valvd.sock"

  curl -sS -o "$output_file" -w '%{http_code}' -X "$method" \
    --unix-socket "$socket" "http://localhost${path}" \
    -H "Content-Type: application/json" \
    --data "$body"
}

fp_move_expect_200() {
  local home_var="$1"
  local body="$2"
  local output_file="${TMPDIR}/fp-move-200.json"
  local status
  status=$(daemon_status POST "/fp/move" "$home_var" "$body" "$output_file")
  [ "$status" = "200" ] || fail "expected /fp/move 200, got ${status}: $(cat "$output_file")"
  cat "$output_file"
}

fp_move_expect_error() {
  local home_var="$1"
  local body="$2"
  local expected_status="$3"
  local expected_error="$4"
  local output_file="${TMPDIR}/fp-move-error.json"
  local status error
  status=$(daemon_status POST "/fp/move" "$home_var" "$body" "$output_file")
  [ "$status" = "$expected_status" ] || fail "expected /fp/move ${expected_status}, got ${status}: $(cat "$output_file")"
  error=$(json_eval 'process.stdout.write(data.error || "")' < "$output_file")
  [ "$error" = "$expected_error" ] || fail "expected /fp/move error ${expected_error}, got ${error}: $(cat "$output_file")"
}

submit_remote_new_version() {
  local folder_id="$1"
  local node_id="$2"
  local based_on_seq="$3"
  local content="$4"
  local tmp_file="${TMPDIR}/remote-version-${node_id}"
  local hash size version_id content_hash
  printf '%s' "$content" > "$tmp_file"
  read -r hash size < <(upload_file_for_api_version "$tmp_file")
  version_id=$(uuid)
  content_hash=$(manifest_content_hash "$hash")
  api POST "/api/folders/${folder_id}/ops" "{\"op_type\":\"new_version\",\"node_id\":\"${node_id}\",\"based_on_seq\":${based_on_seq},\"payload\":{\"version_id\":\"${version_id}\",\"content_hash\":\"${content_hash}\",\"size_bytes\":${size},\"manifest\":[{\"chunk_hash\":\"${hash}\",\"offset\":0,\"length\":${size}}]}}" >/dev/null
}

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-31-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
mkdir -p "${mount_a}/dir-a" "${mount_a}/dir-b"
printf 'rename via fp\n' > "${mount_a}/dir-a/rename-me.txt"
printf 'collision target\n' > "${mount_a}/dir-b/existing.txt"
printf 'stale base\n' > "${mount_a}/stale.txt"
sync_mount "$HOME_A" "$folder_id"

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-31-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B" "$folder_id"

rename_node_id=$(get_node_id_at_path "$folder_id" "/dir-a/rename-me.txt")
rename_seq=$(node_seq_at_path "$folder_id" "/dir-a/rename-me.txt")
fp_move_expect_200 HOME_A "{\"node_id\":\"${rename_node_id}\",\"based_on_seq\":${rename_seq},\"new_name\":\"renamed-via-fp.txt\"}" >/dev/null
wait_for_node_at_path "$folder_id" "/dir-a/renamed-via-fp.txt"

sync_mount "$HOME_B" "$folder_id"
assert_path_present "${mount_b}/dir-a/renamed-via-fp.txt"
assert_path_absent "${mount_b}/dir-a/rename-me.txt"

move_seq=$(node_seq_at_path "$folder_id" "/dir-a/renamed-via-fp.txt")
dir_b_id=$(get_node_id_at_path "$folder_id" "/dir-b")
fp_move_expect_200 HOME_A "{\"node_id\":\"${rename_node_id}\",\"based_on_seq\":${move_seq},\"new_parent_id\":\"${dir_b_id}\"}" >/dev/null
wait_for_node_at_path "$folder_id" "/dir-b/renamed-via-fp.txt"

sync_mount "$HOME_B" "$folder_id"
assert_path_present "${mount_b}/dir-b/renamed-via-fp.txt"
assert_path_absent "${mount_b}/dir-a/renamed-via-fp.txt"

collision_seq_before=$(node_seq_at_path "$folder_id" "/dir-b/renamed-via-fp.txt")
fp_move_expect_error HOME_A "{\"node_id\":\"${rename_node_id}\",\"based_on_seq\":${collision_seq_before},\"new_name\":\"existing.txt\"}" 409 "name_collision"
collision_seq_after=$(node_seq_at_path "$folder_id" "/dir-b/renamed-via-fp.txt")
[ "$collision_seq_after" = "$collision_seq_before" ] || fail "collision advanced node seq from ${collision_seq_before} to ${collision_seq_after}"
assert_live_node_count_at_path "$folder_id" "/dir-b/existing.txt" 1
assert_node_at_path "$folder_id" "/dir-b/renamed-via-fp.txt"

stale_node_id=$(get_node_id_at_path "$folder_id" "/stale.txt")
stale_seq=$(node_seq_at_path "$folder_id" "/stale.txt")
submit_remote_new_version "$folder_id" "$stale_node_id" "$stale_seq" "content from B\n"
fp_move_expect_error HOME_A "{\"node_id\":\"${stale_node_id}\",\"based_on_seq\":${stale_seq},\"new_name\":\"stale-renamed.txt\"}" 409 "superseded"

sync_mount "$HOME_A" "$folder_id"
sync_mount "$HOME_B" "$folder_id"
assert_path_present "${mount_a}/stale.txt"
assert_path_absent "${mount_a}/stale-renamed.txt"
assert_file_contains "${mount_a}/stale.txt" "content from B"
assert_path_present "${mount_b}/stale.txt"
assert_path_absent "${mount_b}/stale-renamed.txt"
assert_file_contains "${mount_b}/stale.txt" "content from B"
