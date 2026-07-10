#!/usr/bin/env bash
set -euo pipefail

repo="${VALV_GITHUB_REPO:-DRNKNDev/valv}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
oss_root="$(cd "${script_dir}/.." && pwd)"

fail() {
  echo "valv signing: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    fail "missing required command: shasum or sha256sum"
  fi
}

checksum_for_asset() {
  local asset="$1"
  awk -v asset="${asset}" '
    $2 == asset || $2 == "*" asset { print $1; found = 1; exit }
    END { if (!found) exit 1 }
  ' "${tmp_dir}/SHA256SUMS"
}

usage() {
  cat >&2 <<'USAGE'
Usage:
  DEVELOPER_ID_APPLICATION="Developer ID Application: Name (TEAMID)" \
    MINISIGN_SECRET_KEY_FILE="/path/to/founder/minisign.key" \
    scripts/sign-cli-binaries.sh v0.1.0

Optionally pass the signing identity as a second argument.

MINISIGN_SECRET_KEY_FILE (task 1.3) must point at the same minisign keypair
release.yml's "Sign checksum manifest" step signs SHA256SUMS with (task 1.2)
- this script's re-signing step is MANDATORY, not optional: it mutates
SHA256SUMS after codesigning, so the signature release.yml already produced
no longer verifies against the file's new content until this step
regenerates it (see design.md D2). If the key is password-protected,
`minisign -S` prompts for it interactively.
USAGE
}

tag="${1:-}"
identity="${DEVELOPER_ID_APPLICATION:-${2:-}}"
minisign_key_file="${MINISIGN_SECRET_KEY_FILE:-}"

[[ -n "${tag}" ]] || { usage; fail "missing tag"; }
[[ "${tag}" == v* ]] || fail "tag must start with v"
[[ -n "${identity}" ]] || { usage; fail "missing Developer ID Application identity"; }
[[ -n "${minisign_key_file}" ]] || { usage; fail "missing MINISIGN_SECRET_KEY_FILE"; }
[[ -f "${minisign_key_file}" ]] || fail "MINISIGN_SECRET_KEY_FILE does not exist: ${minisign_key_file}"

need gh
need codesign
need minisign
need tar
need awk
need mktemp

version="${tag#v}"
target="aarch64-apple-darwin"
asset="valv-${version}-${target}.tar.gz"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

gh release download "${tag}" \
  --repo "${repo}" \
  --pattern "${asset}" \
  --pattern "SHA256SUMS" \
  --dir "${tmp_dir}" \
  --clobber

expected="$(checksum_for_asset "${asset}")" ||
  fail "SHA256SUMS did not contain ${asset}"
actual="$(sha256_file "${tmp_dir}/${asset}")"
[[ "${actual}" == "${expected}" ]] ||
  fail "checksum mismatch for ${asset}: expected ${expected}, got ${actual}"

mkdir -p "${tmp_dir}/payload"
tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}/payload"
[[ -f "${tmp_dir}/payload/valv" && -f "${tmp_dir}/payload/valvd" ]] ||
  fail "${asset} did not contain valv and valvd"

codesign --force --sign "${identity}" --options runtime --timestamp "${tmp_dir}/payload/valv"
codesign --force --sign "${identity}" --options runtime --timestamp "${tmp_dir}/payload/valvd"
codesign -dv --verbose=4 "${tmp_dir}/payload/valv" >/dev/null
codesign -dv --verbose=4 "${tmp_dir}/payload/valvd" >/dev/null

# Persist the signed binaries to a cargo-safe handoff dir so release-app.sh can embed
# the exact same artifacts without a flaky GitHub CDN round-trip.
signed_dir="${oss_root}/crates/target/signed-cli"
mkdir -p "${signed_dir}"
cp "${tmp_dir}/payload/valv" "${tmp_dir}/payload/valvd" "${signed_dir}/"

tar -C "${tmp_dir}/payload" -czf "${tmp_dir}/${asset}" valv valvd
digest="$(sha256_file "${tmp_dir}/${asset}")"

awk -v asset="${asset}" -v digest="${digest}" '
  $2 == asset || $2 == "*" asset {
    print digest "  " asset
    updated = 1
    next
  }
  { print }
  END { if (!updated) exit 1 }
' "${tmp_dir}/SHA256SUMS" > "${tmp_dir}/SHA256SUMS.updated" ||
  fail "SHA256SUMS did not contain ${asset}"
mv "${tmp_dir}/SHA256SUMS.updated" "${tmp_dir}/SHA256SUMS"

minisign -S -s "${minisign_key_file}" \
  -m "${tmp_dir}/SHA256SUMS" \
  -x "${tmp_dir}/SHA256SUMS.minisig" \
  -t "valv release ${tag} (macOS re-sign)"

gh release upload "${tag}" \
  --repo "${repo}" \
  --clobber \
  "${tmp_dir}/${asset}" \
  "${tmp_dir}/SHA256SUMS" \
  "${tmp_dir}/SHA256SUMS.minisig"

echo "Uploaded signed ${asset}, updated SHA256SUMS, and re-signed SHA256SUMS.minisig to ${repo} ${tag}"
