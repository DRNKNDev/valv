#!/usr/bin/env bash
set -euo pipefail

SYNCED_FOLDER="${1:-}"
COUNT="${2:-100}"

if [ -z "$SYNCED_FOLDER" ] || [ ! -d "$SYNCED_FOLDER" ]; then
  printf 'Synced folder missing: %s\n' "$SYNCED_FOLDER" >&2
  exit 1
fi

modified=0
timestamp="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

while IFS= read -r path; do
  printf 'modified_at=%s\n' "$timestamp" >> "$path"
  modified=$((modified + 1))
done < <(find "$SYNCED_FOLDER" -maxdepth 1 -type f | sort | head -n "$COUNT")

printf 'Modified %d files\n' "$modified"
