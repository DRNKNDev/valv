#!/usr/bin/env bash

# CI-only entry point: exercises 34-headless-access-key-mount.sh's real
# install-then-poll path on Linux by running it as a disposable, lingering
# user with a real login session, since `systemctl --user` resolves its unit
# search path from the manager's own environment, not from HOME overrides
# passed to the client. Not part of run-all.sh; the normal 33/34 suite is
# unaffected.

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/harness.sh"

HEADLESS_USER="${VALV_SMOKE_HEADLESS_USER:-valv-smoke}"

if ! id "$HEADLESS_USER" >/dev/null 2>&1; then
  sudo useradd -m "$HEADLESS_USER"
fi
sudo loginctl enable-linger "$HEADLESS_USER"

HEADLESS_UID=$(id -u "$HEADLESS_USER")
HEADLESS_RUNTIME_DIR="/run/user/${HEADLESS_UID}"
HEADLESS_BUS_SOCKET="${HEADLESS_RUNTIME_DIR}/bus"

deadline=$((SECONDS + 30))
while [ "$SECONDS" -lt "$deadline" ] && [ ! -S "$HEADLESS_BUS_SOCKET" ]; do
  sleep 1
done
if [ ! -S "$HEADLESS_BUS_SOCKET" ]; then
  sudo systemctl status "user@${HEADLESS_UID}.service" || true
  fail "no reachable systemd --user session for ${HEADLESS_USER} after enabling linger"
fi

HEADLESS_HOME=$(getent passwd "$HEADLESS_USER" | cut -d: -f6)
[ -n "$HEADLESS_HOME" ] || fail "could not resolve home directory for ${HEADLESS_USER}"

sudo chmod o+x "$HOME"
sudo chmod -R a+rwX "$TMPDIR"

log_file="${TMPDIR}/headless-linux-run.log"
set +e
sudo -H -u "$HEADLESS_USER" \
  env \
    XDG_RUNTIME_DIR="$HEADLESS_RUNTIME_DIR" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=${HEADLESS_BUS_SOCKET}" \
    BACKEND_URL="$BACKEND_URL" \
    DEVICE_TOKEN_A="$DEVICE_TOKEN_A" \
    TMPDIR="$TMPDIR" \
    HOME_A="$HOME_A" \
    VALV_BIN="$VALV_BIN" \
    VALVD_BIN="$VALVD_BIN" \
    VALV_NO_UPDATE_CHECK=1 \
    VALV_SMOKE_HEADLESS_HOME="$HEADLESS_HOME" \
    PATH="$PATH" \
  bash "${SMOKE_DIR}/34-headless-access-key-mount.sh" 2>&1 | tee "$log_file"
status=${PIPESTATUS[0]}
set -e

sudo chmod -R a+rwX "$TMPDIR" || true

if [ "$status" -eq 0 ]; then
  exit 0
fi
if [ "$status" -eq "$SMOKE_SKIP_STATUS" ]; then
  reason=$(grep -m1 '^SKIP: ' "$log_file" | sed 's/^SKIP: //')
  fail "34-headless-access-key-mount.sh skipped even under the disposable lingering user (${reason})"
fi
exit "$status"
