#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-29-design-docs"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
sync_mount "$HOME_A"

# Whole-folder mount: name resolves via the GET /folders/:id fallback (the
# folder's root node always has an empty name by construction), matching the
# basename valv used to create it.
whole_folder_name=$(daemon GET "/status" HOME_A | json_eval "process.stdout.write((data.mounts.find(m => m.folder_id === '${folder_id}') || {}).name || '')")
if [ "$whole_folder_name" != "mount-29-design-docs" ]; then
  fail "expected whole-folder mount name \"mount-29-design-docs\", got \"${whole_folder_name}\""
fi

mkdir -p "${mount_a}/Drafts"
sync_mount "$HOME_A"
drafts_node_id=$(get_node_id_at_path "$folder_id" "/Drafts")
grant=$(api POST "/api/folders/${folder_id}/grants" "{\"scope_node_id\":\"${drafts_node_id}\",\"name\":\"Drafts Smoke Device\",\"can_read\":true,\"can_write\":true}")
grant_token=$(printf '%s' "$grant" | json_eval 'process.stdout.write(data.token)')

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-29-b"
mount_folder "$HOME_B" "$mount_b" --key "$grant_token" >/dev/null

# Subfolder-scoped mount: name resolves entirely from the local mirror (the
# scope node's own name), with no GET /folders/:id call needed.
subfolder_name=$(daemon GET "/status" HOME_B | json_eval "process.stdout.write((data.mounts.find(m => m.folder_id === '${folder_id}') || {}).name || '')")
if [ "$subfolder_name" != "Drafts" ]; then
  fail "expected subfolder-scoped mount name \"Drafts\", got \"${subfolder_name}\""
fi
