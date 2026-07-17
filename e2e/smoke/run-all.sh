#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/harness.sh"

scripts=()
while IFS= read -r script; do
  scripts+=("$script")
done < <(find "$SMOKE_DIR" -maxdepth 1 -name '[0-9][0-9]-*.sh' | sort)

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
    diag="${SMOKE_DIAG_DIR}/${script_name}"
    mkdir -p "$diag"
    cp "$log_file" "$diag/console.out" 2>/dev/null || true
    find "$TMPDIR" -maxdepth 3 -name '*.log' -exec cp {} "$diag/" \; 2>/dev/null || true
    # local SQLite DBs (per-device sync.db + WAL/SHM, and the backend db), named
    # by their path under TMPDIR so home-a vs home-b stay distinguishable.
    find "$TMPDIR" \( -name 'sync.db' -o -name 'sync.db-wal' -o -name 'sync.db-shm' -o -name 'backend.db' \) 2>/dev/null | while IFS= read -r db; do
      cp "$db" "$diag/$(printf '%s' "${db#"${TMPDIR}"/}" | tr '/' '_')" 2>/dev/null || true
    done
    { echo "# mount dirs at failure of ${script_name}"; find "$TMPDIR" -maxdepth 2 -type d -name 'mount-*' -exec ls -la {} \; ; \
      echo "# small file contents under mount dirs"; find "$TMPDIR" -maxdepth 3 -path '*mount-*' -type f -size -20k -exec sh -c 'echo "=== $1 ==="; cat "$1"' _ {} \; ; } \
      > "$diag/mounts.txt" 2>/dev/null || true
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
