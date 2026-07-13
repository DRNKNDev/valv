#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-28-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
mkdir -p "${mount_a}/docs"
printf 'shared content\n' > "${mount_a}/docs/notes.txt"
sync_mount "$HOME_A"

notes_node_id=$(get_node_id_at_path "$folder_id" "/docs/notes.txt")
docs_node_id=$(get_node_id_at_path "$folder_id" "/docs")

# POST /fp/share creates a real, acceptable invite.
share_response=$(daemon POST "/fp/share" HOME_A "{\"node_id\":\"${notes_node_id}\",\"invited_email\":\"friend28@example.com\"}")
invite_url=$(printf '%s' "$share_response" | json_eval 'process.stdout.write(data.invite_url)')
if [ -z "$invite_url" ]; then
  fail "POST /fp/share did not return an invite_url"
fi

# GET /nodes/:node_id/path resolves a real nested file's path correctly.
docs_path=$(daemon GET "/nodes/${docs_node_id}/path" HOME_A | json_eval 'process.stdout.write(data.path)')
if [ "$docs_path" != "docs" ]; then
  fail "expected /nodes/:node_id/path to resolve docs to \"docs\", got \"${docs_path}\""
fi
notes_path=$(daemon GET "/nodes/${notes_node_id}/path" HOME_A | json_eval 'process.stdout.write(data.path)')
if [ "$notes_path" != "docs/notes.txt" ]; then
  fail "expected /nodes/:node_id/path to resolve notes.txt to \"docs/notes.txt\", got \"${notes_path}\""
fi

# GET /fp/watch wakes on a remote change instead of timing out.
start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-28-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B"

since_seq=$(daemon GET "/fp/anchor?folder_id=${folder_id}" HOME_B | json_eval 'process.stdout.write(String(data.server_seq))')
watch_out="${TMPDIR}/28-watch-output.json"
: > "$watch_out"
(daemon GET "/fp/watch?folder_id=${folder_id}&since_seq=${since_seq}" HOME_B > "$watch_out") &
watch_pid=$!

printf 'a new file from device A\n' > "${mount_a}/from-a.txt"
sync_mount "$HOME_A"

watch_started=$SECONDS
wait "$watch_pid"
watch_elapsed=$((SECONDS - watch_started))
if [ "$watch_elapsed" -ge 20 ]; then
  fail "GET /fp/watch took ${watch_elapsed}s, expected it to wake on notify well before its ~25s timeout"
fi
new_seq=$(json_eval 'process.stdout.write(String(data.server_seq))' < "$watch_out")
if [ "$new_seq" -le "$since_seq" ]; then
  fail "expected GET /fp/watch to return an advanced server_seq (was ${since_seq}, got ${new_seq})"
fi
