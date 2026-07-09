#!/usr/bin/env bash

# C-2 regression guard: a background pull (WS-notify driven, not an explicit
# `valv sync`) must never resurrect a file that a device deleted locally while
# offline, even when another device pushed a `new_version` for that same node
# in the meantime.
#
# Scenario:
#   1. Device A materializes shared.txt (so it has a real local
#      materialized-content marker for the node), then goes offline (daemon
#      stopped) and deletes its local copy - the delete is never pushed while
#      offline.
#   2. Device B edits and pushes a new version of the same file before A's
#      delete reaches the server.
#   3. Device A's daemon restarts. Its background pull (triggered by a WS
#      push from an unrelated, later change on the same folder - not an
#      explicit `valv sync` for A) must apply B's `new_version` op to A's
#      local mirror (current_version_id advances) but must NOT write B's
#      content back to the mount path: A previously had this node's content
#      on disk and the user deleted it, so writing it back now would be an
#      unwanted resurrection of an offline delete.

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

wait_for_local_node_version() {
  local home_dir="$1"
  local node_id="$2"
  local expected_version="$3"
  local timeout_s="${4:-30}"
  local deadline=$((SECONDS + timeout_s))
  while [ "$SECONDS" -lt "$deadline" ]; do
    local current
    current=$(sqlite3 "${home_dir}/.local/share/valv/sync.db" \
      "SELECT current_version_id FROM nodes WHERE node_id = '${node_id}'" 2>/dev/null || true)
    if [ "$current" = "$expected_version" ]; then
      return 0
    fi
    sleep 0.5
  done
  fail "local mirror for ${node_id} in ${home_dir} did not reach version ${expected_version} within ${timeout_s}s"
}

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-32-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
printf 'original content\n' > "${mount_a}/shared.txt"
sync_mount "$HOME_A" "$folder_id"
assert_file_contains "${mount_a}/shared.txt" "original content"
node_id=$(node_id_at_path "$folder_id" "/shared.txt")

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-32-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B" "$folder_id"
assert_file_contains "${mount_b}/shared.txt" "original content"

# Device A goes offline and deletes its local copy. The delete is never
# submitted while the daemon is stopped.
stop_daemon DAEMON_PID_A
rm "${mount_a}/shared.txt"

# Device B edits the same file and pushes a new version before A's delete
# ever reaches the server.
printf 'edited by B while A was offline\n' > "${mount_b}/shared.txt"
sync_mount "$HOME_B" "$folder_id"
edited_version=$(node_version_at_path "$folder_id" "/shared.txt")
[ -n "$edited_version" ] || fail "expected a new version id for /shared.txt after B's edit"

# Device A comes back online. Give its websocket subscription a moment to
# (re)connect before triggering the notification it will react to.
start_daemon HOME_A DAEMON_PID_A
sleep 1

# An unrelated, later push on the same folder gives A's freshly reconnected
# WS subscription something to notify on, so its background pull runs
# promptly instead of waiting for the 30s timer. A's own pull_delta call
# still catches up on B's earlier edit in the same page, since A's cursor is
# far behind - this is the "receives both the pre-existing tombstone-eligible
# state and the other device's edit" case from the task.
printf 'trigger\n' > "${mount_b}/trigger.txt"
sync_mount "$HOME_B" "$folder_id"

wait_for_local_node_version "$HOME_A" "$node_id" "$edited_version" 30

# The background pull has applied B's new_version op to A's local mirror
# (current_version_id now matches B's edit), but must not have materialized
# the content back onto disk.
assert_path_absent "${mount_a}/shared.txt"
sleep 2
assert_path_absent "${mount_a}/shared.txt"

# A's own explicit sync now runs, submitting (or, per design.md D4's
# documented race, potentially losing the race to re-materialization if A
# cannot yet prove it holds a copy of B's version) A's delete. Either outcome
# is acceptable here - what matters is that nothing resurrected during the
# pure background-only phase asserted above. If the file does reappear, it
# must be with B's real, correct content - never stale or corrupt.
sync_mount "$HOME_A" "$folder_id"
if [ -e "${mount_a}/shared.txt" ]; then
  assert_file_contains "${mount_a}/shared.txt" "edited by B while A was offline"
else
  wait_for_deleted_node_at_path "$folder_id" "/shared.txt"
fi
