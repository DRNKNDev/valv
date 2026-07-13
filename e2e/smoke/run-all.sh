#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/harness.sh"

mapfile -t scripts < <(find "$SMOKE_DIR" -maxdepth 1 -name '[0-9][0-9]-*.sh' | sort)

declare -a results=()
failures=0
skips=0

for script in "${scripts[@]}"; do
  script_name=$(basename "$script")
  printf 'Running %s\n' "$script_name"
  log_file=$(mktemp)
  set +e
  bash "$script" 2>&1 | tee "$log_file"
  status=${PIPESTATUS[0]}
  set -e
  if [ "$status" -eq 0 ]; then
    printf '[PASS] %s\n' "$script_name"
    results+=("PASS ${script_name}")
  elif [ "$status" -eq "$SMOKE_SKIP_STATUS" ]; then
    reason=$(grep -m1 '^SKIP: ' "$log_file" | sed 's/^SKIP: //')
    printf '[SKIP] %s (%s)\n' "$script_name" "$reason"
    results+=("SKIP ${script_name} - ${reason}")
    skips=$((skips + 1))
  else
    printf '[FAIL] %s\n' "$script_name" >&2
    results+=("FAIL ${script_name}")
    failures=$((failures + 1))
  fi
  rm -f "$log_file"
done

printf '\nSmoke summary\n'
printf '=============\n'
for result in "${results[@]}"; do
  printf '%s\n' "$result"
done
printf '\n%d passed, %d failed, %d skipped\n' \
  "$(( ${#results[@]} - failures - skips ))" "$failures" "$skips"

exit "$failures"
