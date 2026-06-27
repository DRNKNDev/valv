#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="${1:-$SCRIPT_DIR/../test-data}"

rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

timestamp="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

for index in $(seq 1 5000); do
  filename="$(printf 'test-file-%04d.txt' "$index")"
  cat > "$OUTPUT_DIR/$filename" <<EOF
Valv File Provider spike test data
file: $filename
generated_at: $timestamp
sequence: $index
EOF
done

printf 'Generated 5000 files in %s\n' "$OUTPUT_DIR"
