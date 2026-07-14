#!/usr/bin/env bash

# Installs a real systemd user unit / launchd agent instead of start_daemon;
# self-gates via headless_mount_unsafe_reason.

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

HOME_D="${VALV_SMOKE_HEADLESS_HOME:-${TMPDIR}/home-headless}"
export HOME_D

headless_mount_unsafe_reason() {
  case "$(uname -s)" in
    Linux)
      if ! command -v systemctl >/dev/null 2>&1; then
        printf 'systemctl is not installed on this host'
        return 0
      fi
      if ! systemctl --user show-environment >/dev/null 2>&1; then
        printf 'no reachable systemd --user session (needs a user bus / XDG_RUNTIME_DIR)'
        return 0
      fi
      # valvd writes the unit under $HOME, but systemd --user resolves its unit
      # search path from the manager's own HOME, so the install only lands where
      # the manager looks when both are the same home. run-headless-linux.sh is
      # the job that satisfies this.
      if [ "$HOME_D" != "$HOME" ]; then
        printf 'install home %s is not the systemd --user manager home %s' "$HOME_D" "$HOME"
        return 0
      fi
      ;;
    Darwin)
      local uid
      uid=$(id -u)
      if ! launchctl print "gui/${uid}" >/dev/null 2>&1; then
        printf 'no reachable gui/%s launchd domain on this host' "$uid"
        return 0
      fi
      if launchctl print "gui/${uid}/dev.drnkn.valvd" >/dev/null 2>&1; then
        printf 'dev.drnkn.valvd is already loaded in gui/%s; refusing to touch a real install' "$uid"
        return 0
      fi
      ;;
    *)
      printf 'unsupported platform for a headless daemon install'
      return 0
      ;;
  esac
  return 1
}

if reason=$(headless_mount_unsafe_reason); then
  skip "$reason"
fi

DAEMON_PID_A=""
trap 'HOME="$HOME_D" "$VALV_BIN" daemon uninstall >/dev/null 2>&1 || true; stop_daemon DAEMON_PID_A' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-34-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")
printf 'seeded before headless mount\n' > "${mount_a}/seed.txt"
sync_mount "$HOME_A"

share_output=$(HOME="$HOME_A" "$VALV_BIN" --json share "$mount_a" --key "Headless Smoke Key")
grant_token=$(printf '%s' "$share_output" | json_eval "process.stdout.write(data.token)")

assert_path_absent "${HOME_D}/.config/valv/config.toml"

data_path="${HOME_D}/data"
mkdir -p "$data_path"
HOME="$HOME_D" VALV_BACKEND_URL="$BACKEND_URL" "$VALV_BIN" mount "$data_path" --key "$grant_token" >/dev/null

assert_path_present "${HOME_D}/.config/valv/config.toml"
if grep -q "device_token" "${HOME_D}/.config/valv/config.toml"; then
  fail "headless config.toml must hold no device_token"
fi

HOME="$HOME_D" "$VALV_BIN" sync >/dev/null
assert_file_contains "${data_path}/seed.txt" "seeded before headless mount"

printf 'pushed from the headless machine\n' > "${data_path}/from-headless.txt"
HOME="$HOME_D" "$VALV_BIN" sync >/dev/null
wait_for_node_at_path "$folder_id" "/from-headless.txt"
