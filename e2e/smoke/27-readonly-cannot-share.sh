#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
trap 'stop_daemon DAEMON_PID_A' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-27-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
mkdir -p "${mount_a}/shared"
sync_mount "$HOME_A"

scope_node_id=$(get_node_id_at_path "$folder_id" "/shared")
grant=$(api POST "/api/folders/${folder_id}/grants" "{\"scope_node_id\":\"${scope_node_id}\",\"name\":\"Read Only Smoke\",\"can_read\":true,\"can_write\":false}")
readonly_token=$(printf '%s' "$grant" | json_eval 'process.stdout.write(data.token)')

invite_status=$(curl -s -o /dev/null -w '%{http_code}' -X POST "${BACKEND_URL}/api/folders/${folder_id}/invites" \
  -H "Authorization: Bearer ${readonly_token}" \
  -H "Content-Type: application/json" \
  --data '{"invited_email":"friend@example.com"}')
if [ "$invite_status" != "403" ]; then
  fail "expected 403 from read-only invite creation, got ${invite_status}"
fi

grant_status=$(curl -s -o /dev/null -w '%{http_code}' -X POST "${BACKEND_URL}/api/folders/${folder_id}/grants" \
  -H "Authorization: Bearer ${readonly_token}" \
  -H "Content-Type: application/json" \
  --data "{\"scope_node_id\":\"${scope_node_id}\",\"name\":\"Should Fail\"}")
if [ "$grant_status" != "403" ]; then
  fail "expected 403 from read-only grant provisioning, got ${grant_status}"
fi
