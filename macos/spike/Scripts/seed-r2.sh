#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: SPIKE_BUCKET=<bucket> SPIKE_ENDPOINT=<endpoint> ./oss/macos/spike/Scripts/seed-r2.sh [test-data-dir]

Required environment variables:
  SPIKE_BUCKET    R2 bucket name
  SPIKE_ENDPOINT  R2 S3 endpoint, for example:
                  https://<account-id>.r2.cloudflarestorage.com

Optional argument:
  test-data-dir   Directory to upload (defaults to ../test-data)
EOF
}

if ! command -v aws >/dev/null 2>&1; then
  printf 'aws CLI not found in PATH\n' >&2
  exit 1
fi

if [ -z "${SPIKE_BUCKET:-}" ] || [ -z "${SPIKE_ENDPOINT:-}" ]; then
  printf 'SPIKE_BUCKET and SPIKE_ENDPOINT are required.\n\n' >&2
  usage
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA_DIR="${1:-$SCRIPT_DIR/../test-data}"

if [ ! -d "$DATA_DIR" ]; then
  printf 'Test data directory not found: %s\n' "$DATA_DIR" >&2
  exit 1
fi

aws s3 sync "$DATA_DIR" "s3://$SPIKE_BUCKET/" --endpoint-url "$SPIKE_ENDPOINT"
