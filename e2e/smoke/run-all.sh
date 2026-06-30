#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/harness.sh"

mapfile -t scripts < <(find "$SMOKE_DIR" -maxdepth 1 -name '[0-9][0-9]-*.sh' | sort)

declare -a results=()
failures=0

for script in "${scripts[@]}"; do
  script_name=$(basename "$script")
  printf 'Running %s\n' "$script_name"
  if bash "$script"; then
    printf '[PASS] %s\n' "$script_name"
    results+=("PASS ${script_name}")
  else
    printf '[FAIL] %s\n' "$script_name" >&2
    results+=("FAIL ${script_name}")
    failures=$((failures + 1))
  fi
done

printf '\nSmoke summary\n'
printf '=============\n'
for result in "${results[@]}"; do
  printf '%s\n' "$result"
done

exit "$failures"
