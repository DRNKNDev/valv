#!/usr/bin/env bash

# Regression guard: `valv unshare` must revoke a grant issued to someone
# else, not just the caller's own.

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_C=""
HOME_C="${TMPDIR}/home-c-33"
export HOME_C
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_C' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-33-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
printf 'before revoke\n' > "${mount_a}/pre-revoke.txt"
sync_mount "$HOME_A"

share_output=$(HOME="$HOME_A" "$VALV_BIN" --json share "$mount_a" --key "Unshare Regression Device")
grant_token=$(printf '%s' "$share_output" | json_eval "process.stdout.write(data.token)")
grant_id=$(printf '%s' "$share_output" | json_eval "process.stdout.write(data.grant_id)")
device_id_c=$(api GET "/api/folders/${folder_id}/grants" | json_eval "process.stdout.write((data.find(g => g.grant_id === $(json_string "$grant_id")) || {}).device_id || '')")
write_device_config "$HOME_C" "$device_id_c" "$grant_token" "Unshare Regression Device"

start_daemon HOME_C DAEMON_PID_C
mount_c="${TMPDIR}/mount-33-c"
mount_folder "$HOME_C" "$mount_c" --key "$grant_token" >/dev/null
sync_mount "$HOME_C"
assert_file_contains "${mount_c}/pre-revoke.txt" "before revoke"

HOME="$HOME_A" "$VALV_BIN" unshare "$mount_a" --key "Unshare Regression Device" --yes

printf 'after revoke\n' > "${mount_a}/post-revoke.txt"
sync_mount "$HOME_A"
HOME="$HOME_C" "$VALV_BIN" sync >/dev/null 2>&1 || true
assert_path_absent "${mount_c}/post-revoke.txt"
assert_path_present "${mount_c}/pre-revoke.txt"
