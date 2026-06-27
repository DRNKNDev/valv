#!/usr/bin/env bash
set -euo pipefail

SYNCED_FOLDER="${1:-}"
COUNT="${2:-10}"

if [ -z "$SYNCED_FOLDER" ] || [ ! -d "$SYNCED_FOLDER" ]; then
  printf 'Synced folder missing: %s\n' "$SYNCED_FOLDER" >&2
  exit 1
fi

find "$SYNCED_FOLDER" -maxdepth 1 -type f | sort | head -n "$COUNT" | while IFS= read -r path; do
  cat "$path" >/dev/null
  printf '%s\n' "$path"
done
