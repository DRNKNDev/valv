#!/usr/bin/env bash
set -euo pipefail

MODE="${1:---dry-run}"
BUNDLE_ID="dev.drnkn.valv"
FILE_PROVIDER_BUNDLE_ID="dev.drnkn.valv.fileprovider"
FILE_PROVIDER_UI_BUNDLE_ID="dev.drnkn.valv.fileprovider-ui"
APP_GROUP_ID="group.dev.drnkn.valv"
LAUNCH_AGENT_LABEL="dev.drnkn.valvd"

usage() {
  cat <<'USAGE'
Usage: oss/macos/scripts/reset-local-state.sh [--dry-run | --execute]

  --dry-run  Show what would be removed (default).
  --execute  Reset local Valv state and Xcode DerivedData.

Use Sign Out in Valv before --execute so the File Provider domain is removed
through the supported macOS API. Synced folder contents and backend data are
never removed.
USAGE
}

case "$MODE" in
  --dry-run|--execute)
    ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 1
    ;;
esac

[[ "$#" -le 1 ]] || {
  usage >&2
  exit 1
}
[[ "$(uname -s)" == "Darwin" ]] || {
  printf 'This reset script only supports macOS.\n' >&2
  exit 1
}

SIGNED_IN="$(defaults read "$BUNDLE_ID" dev.drnkn.valv.hasSignedIn 2>/dev/null || true)"
DOMAIN_ID="$(defaults read "$BUNDLE_ID" dev.drnkn.valv.fileProviderDomainIdentifier 2>/dev/null || true)"
if [[ "$MODE" == "--execute" ]] && \
    [[ "$SIGNED_IN" == "1" || "$SIGNED_IN" == "true" || -n "$DOMAIN_ID" ]]; then
  printf 'Sign Out in Valv before resetting local state.\n' >&2
  exit 1
fi

remove_path() {
  local path="$1"

  if [[ ! -e "$path" && ! -L "$path" ]]; then
    printf 'absent: %s\n' "$path"
  elif [[ "$MODE" == "--dry-run" ]]; then
    printf 'would remove: %s\n' "$path"
  else
    rm -rf "$path"
    printf 'removed: %s\n' "$path"
  fi
}

stop_process() {
  local name="$1"

  if ! pgrep -x "$name" >/dev/null 2>&1; then
    return
  fi
  if [[ "$MODE" == "--dry-run" ]]; then
    printf 'would stop: %s\n' "$name"
  else
    pkill -x "$name" >/dev/null 2>&1 || true
    printf 'stopped: %s\n' "$name"
  fi
}

HOME_DIR="${HOME:?HOME is not set}"
LAUNCH_AGENT_PATH="${HOME_DIR}/Library/LaunchAgents/${LAUNCH_AGENT_LABEL}.plist"

printf 'Valv local reset (%s)\n\n' "${MODE#--}"

stop_process "Valv"
stop_process "ValvFileProvider"
stop_process "ValvFinderSync"

if launchctl print "gui/$(id -u)/${LAUNCH_AGENT_LABEL}" >/dev/null 2>&1; then
  if [[ "$MODE" == "--dry-run" ]]; then
    printf 'would boot out: %s\n' "$LAUNCH_AGENT_LABEL"
  else
    launchctl bootout "gui/$(id -u)/${LAUNCH_AGENT_LABEL}" >/dev/null 2>&1 || \
      launchctl bootout "gui/$(id -u)" "$LAUNCH_AGENT_PATH" >/dev/null 2>&1 || true
    printf 'booted out: %s\n' "$LAUNCH_AGENT_LABEL"
  fi
fi
stop_process "valvd"

if [[ "$MODE" == "--dry-run" ]]; then
  printf 'would delete defaults: %s\n' "$BUNDLE_ID"
else
  defaults delete "$BUNDLE_ID" >/dev/null 2>&1 || true
  printf 'deleted defaults: %s\n' "$BUNDLE_ID"
fi

PATHS=(
  "${HOME_DIR}/.config/valv"
  "${HOME_DIR}/.local/share/valv"
  "${HOME_DIR}/Library/Application Support/Valv"
  "${HOME_DIR}/Library/Logs/Valv"
  "$LAUNCH_AGENT_PATH"
  "${HOME_DIR}/Library/Preferences/${BUNDLE_ID}.plist"
  "${HOME_DIR}/Library/Containers/${BUNDLE_ID}"
  "${HOME_DIR}/Library/Containers/${FILE_PROVIDER_BUNDLE_ID}"
  "${HOME_DIR}/Library/Containers/${FILE_PROVIDER_UI_BUNDLE_ID}"
  "${HOME_DIR}/Library/Group Containers/${APP_GROUP_ID}"
  "${HOME_DIR}/Library/Saved Application State/${BUNDLE_ID}.savedState"
  "/tmp/valvd.log"
)

for path in "${PATHS[@]}"; do
  remove_path "$path"
done

shopt -s nullglob
DERIVED_DATA_PATHS=("${HOME_DIR}"/Library/Developer/Xcode/DerivedData/Valv-*)
shopt -u nullglob
for path in "${DERIVED_DATA_PATHS[@]}"; do
  remove_path "$path"
done

printf '\nPreserved: synced folders, backend data, and global CLI installations.\n'
if [[ "$MODE" == "--dry-run" ]]; then
  printf 'Run with --execute after reviewing this list.\n'
else
  printf 'Open oss/macos/Valv/Valv.xcodeproj and run the Valv scheme.\n'
fi
