#!/usr/bin/env bash

set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "${SMOKE_DIR}/helpers.sh"

DAEMON_PID_A=""
DAEMON_PID_B=""
trap 'stop_daemon DAEMON_PID_A; stop_daemon DAEMON_PID_B' EXIT

start_daemon HOME_A DAEMON_PID_A
mount_a="${TMPDIR}/mount-25-a"
folder_id=$(mount_folder "$HOME_A" "$mount_a")

dd if=/dev/urandom bs=1M count=12 of="${mount_a}/large.bin"
sync_mount "$HOME_A" "$folder_id"
node_id=$(get_node_id_at_path "$folder_id" "/large.bin")
manifest=$(api GET "/api/folders/${folder_id}/versions/${node_id}" | json_eval 'process.stdout.write(JSON.stringify(data[0].manifest || []))')
chunk_count=$(printf '%s' "$manifest" | json_eval 'process.stdout.write(String(data.length))')
[ "$chunk_count" -ge 2 ] || fail "expected multiple chunks, got ${chunk_count}"
printf '%s' "$manifest" | json_eval 'for (const c of data) console.log(c.chunk_hash)' | while read -r hash; do
  assert_bucket_key "$(expected_chunk_key "$hash")"
done

start_daemon HOME_B DAEMON_PID_B
mount_b="${TMPDIR}/mount-25-b"
mount_folder "$HOME_B" "$mount_b" --folder "$folder_id" >/dev/null
sync_mount "$HOME_B" "$folder_id"
assert_path_present "${mount_b}/large.bin"
size_b=$(wc -c < "${mount_b}/large.bin" | tr -d ' ')
[ "$size_b" = "12582912" ] || fail "expected 12582912 bytes on Device B, got ${size_b}"
sha256_a=$(shasum -a 256 "${mount_a}/large.bin" | cut -d ' ' -f 1)
sha256_b=$(shasum -a 256 "${mount_b}/large.bin" | cut -d ' ' -f 1)
[ "$sha256_a" = "$sha256_b" ] || fail "large file checksum mismatch"

printf '\001' | dd of="${mount_a}/large.bin" bs=1 count=1 conv=notrunc
sync_mount "$HOME_A" "$folder_id"
manifest_v2=$(api GET "/api/folders/${folder_id}/versions/${node_id}" | json_eval 'process.stdout.write(JSON.stringify(data[0].manifest || []))')
hashes_v1="${TMPDIR}/large-v1-hashes.txt"
hashes_v2="${TMPDIR}/large-v2-hashes.txt"
printf '%s' "$manifest" | json_eval 'for (const c of data) console.log(c.chunk_hash)' | sort > "$hashes_v1"
printf '%s' "$manifest_v2" | json_eval 'for (const c of data) console.log(c.chunk_hash)' | sort > "$hashes_v2"
shared_count=$(comm -12 "$hashes_v1" "$hashes_v2" | wc -l | tr -d ' ')
printf 'shared chunks after one-byte edit: %s\n' "$shared_count"
[ "$shared_count" -gt 0 ] || fail "expected at least one shared chunk after one-byte edit"
