#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-30-unmount"
folder_id=$(mount_folder "$HOME_A" "$mount_a")

echo "hello unmount" > "${mount_a}/keep-me.txt"
sync_mount "$HOME_A" "$folder_id"
assert_node_at_path "$folder_id" "/keep-me.txt"

# DELETE /mount unmounts locally only - no backend endpoint is called, and the
# locally materialized file is left untouched on disk.
daemon DELETE "/mount" HOME_A "{\"folder_id\":\"${folder_id}\"}" >/dev/null

still_listed=$(daemon GET "/mounts" HOME_A | json_eval "process.stdout.write(data.some(m => m.folder_id === '${folder_id}') ? 'yes' : 'no')")
if [ "$still_listed" != "no" ]; then
  fail "expected folder ${folder_id} to no longer appear in GET /mounts after unmount"
fi

assert_path_present "${mount_a}/keep-me.txt"
assert_file_contains "${mount_a}/keep-me.txt" "hello unmount"

# The backend folder/grant itself is untouched - still resolvable directly.
folder_name=$(api GET "/api/folders/${folder_id}" | json_eval 'process.stdout.write(data.name)')
if [ -z "$folder_name" ]; then
  fail "expected the backend folder to still exist and be resolvable after a local unmount"
fi

# A daemon restart shouldn't resurrect the mount registration.
stop_daemon DAEMON_PID_A
start_daemon HOME_A DAEMON_PID_A
still_listed_after_restart=$(daemon GET "/mounts" HOME_A | json_eval "process.stdout.write(data.some(m => m.folder_id === '${folder_id}') ? 'yes' : 'no')")
if [ "$still_listed_after_restart" != "no" ]; then
  fail "expected unmounted folder ${folder_id} to stay gone after a daemon restart"
fi

# Unknown folder_id returns 404, not a false success.
unknown_status=$(curl -s -o /dev/null -w '%{http_code}' -X DELETE --unix-socket "${HOME_A}/.local/share/valv/valvd.sock" \
  "http://localhost/mount" -H "Content-Type: application/json" --data '{"folder_id":"does-not-exist"}')
if [ "$unknown_status" != "404" ]; then
  fail "expected DELETE /mount for an unknown folder_id to return 404, got ${unknown_status}"
fi
