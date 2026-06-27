#!/usr/bin/env bash
set -euo pipefail

SPIKE_FOLDER_NAME="${1:-SpikeApp-ValvSpike}"
DERIVED_DATA_GLOB="${HOME}/Library/Developer/Xcode/DerivedData/SpikeApp-*"
CLOUD_STORAGE_PATH="${HOME}/Library/CloudStorage/${SPIKE_FOLDER_NAME}"
TRACE_LOG_PATH="${HOME}/Library/Group Containers/group.dev.drnkn.SpikeApp/spike-trace.log"
OBJECT_CACHE_PATH="${HOME}/Library/Group Containers/group.dev.drnkn.SpikeApp/object-list-cache.json"

printf 'Resetting File Provider spike state for %s\n' "$SPIKE_FOLDER_NAME"

pkill -f SpikeApp >/dev/null 2>&1 || true
pkill -f SpikeExtension >/dev/null 2>&1 || true
pkill -f presign-helper.js >/dev/null 2>&1 || true

rm -f "$TRACE_LOG_PATH"
rm -f "$OBJECT_CACHE_PATH"
rm -rf $DERIVED_DATA_GLOB

printf 'Did not touch synced CloudStorage contents: %s\n' "$CLOUD_STORAGE_PATH"
printf 'Removed trace log: %s\n' "$TRACE_LOG_PATH"
printf 'Removed object cache: %s\n' "$OBJECT_CACHE_PATH"
printf 'Removed DerivedData matching: %s\n' "$DERIVED_DATA_GLOB"

printf '\nNext steps:\n'
printf '1. Restart presign-helper.js\n'
printf '2. Relaunch SpikeApp from Xcode\n'
printf '3. If you need a clean bucket, reseed with gen-test-data.sh + seed-r2.sh\n'
printf '4. Open the domain in Finder and recount files\n'
