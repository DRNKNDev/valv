#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/harness.sh"

scripts=(
  01-mount-sync.sh
  02-create-file.sh
  03-create-folder.sh
  04-edit-file.sh
  05-delete-file.sh
  06-rename-file.sh
  07-two-device-sync.sh
  08-conflict-copy.sh
  09-grant-scope.sh
  10-pause-resume.sh
)

declare -a results=()
failures=0

for script in "${scripts[@]}"; do
  printf 'Running %s\n' "$script"
  if bash "${SMOKE_DIR}/${script}"; then
    printf '[PASS] %s\n' "$script"
    results+=("PASS ${script}")
  else
    printf '[FAIL] %s\n' "$script" >&2
    results+=("FAIL ${script}")
    failures=$((failures + 1))
  fi
done

printf '\nSmoke summary\n'
printf '=============\n'
for result in "${results[@]}"; do
  printf '%s\n' "$result"
done

exit "$failures"
