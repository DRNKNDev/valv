#!/usr/bin/env bash
set -euo pipefail

pid="$(pgrep -f SpikeExtension | head -n 1 || true)"

if [ -z "$pid" ]; then
  printf 'SpikeExtension process not found\n' >&2
  exit 1
fi

log_file="rss-log.txt"

trap 'printf "Stopping RSS monitor\n"; exit 0' INT TERM

while true; do
  if ! ps -p "$pid" >/dev/null 2>&1; then
    printf 'SpikeExtension process not found\n' >&2
    exit 1
  fi

  rss="$(ps -o rss= -p "$pid" | tr -d ' ')"
  printf '%s %s\n' "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" "$rss" >> "$log_file"
  sleep 2
done
