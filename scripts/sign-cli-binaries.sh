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
    NOTARY_PROFILE="valv-notary" \
    scripts/sign-cli-binaries.sh [--local] v0.1.0

Optionally pass the signing identity as a second positional argument.

--local builds valv/valvd from the working tree with
cargo build --release --target aarch64-apple-darwin instead of downloading a
published release asset. It needs no tag to already exist, no gh auth, and
no GitHub network (notarization still needs network). It skips gh release
upload and instead prints the paths of the produced tarball, SHA256SUMS,
SHA256SUMS.minisig, and the signed-cli handoff dir. A --local SHA256SUMS
covers macOS only: macOS cannot cross-compile the Linux target, so the
x86_64-unknown-linux-gnu row is absent by design, not a bug.

NOTARY_PROFILE must name a keychain profile created with
xcrun notarytool store-credentials. It notarizes valv/valvd after
codesigning, in every mode; a bare Mach-O cannot be stapled, so Gatekeeper
resolves the ticket via an online lookup on first run.

MINISIGN_SECRET_KEY_FILE (task 1.3) must point at the same minisign keypair
release.yml's "Sign checksum manifest" step signs SHA256SUMS with (task 1.2)
- this script's re-signing step is MANDATORY, not optional: it mutates
SHA256SUMS after codesigning, so the signature release.yml already produced
no longer verifies against the file's new content until this step
regenerates it (see design.md D2). If the key is password-protected,
`minisign -S` prompts for it interactively.
USAGE
}

local_mode=0
positional=()
for arg in "$@"; do
  case "${arg}" in
    --local)
      local_mode=1
      ;;
    *)
      positional+=("${arg}")
      ;;
  esac
done

tag="${positional[0]:-}"
identity="${DEVELOPER_ID_APPLICATION:-${positional[1]:-}}"
minisign_key_file="${MINISIGN_SECRET_KEY_FILE:-}"
notary_profile="${NOTARY_PROFILE:-}"

[[ -n "${tag}" ]] || { usage; fail "missing tag"; }
[[ "${tag}" == v* ]] || fail "tag must start with v"
[[ -n "${identity}" ]] || { usage; fail "missing Developer ID Application identity"; }
[[ -n "${minisign_key_file}" ]] || { usage; fail "missing MINISIGN_SECRET_KEY_FILE"; }
[[ -f "${minisign_key_file}" ]] || fail "MINISIGN_SECRET_KEY_FILE does not exist: ${minisign_key_file}"
[[ -n "${notary_profile}" ]] || { usage; fail "missing NOTARY_PROFILE"; }

need gh
need codesign
need ditto
need xcrun
need minisign
need tar
need awk
need mktemp
[[ "${local_mode}" -eq 0 ]] || need cargo

version="${tag#v}"
target="aarch64-apple-darwin"
asset="valv-${version}-${target}.tar.gz"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

mkdir -p "${tmp_dir}/payload"

if [[ "${local_mode}" -eq 1 ]]; then
  echo "==> Building valv/valvd from the working tree (--local)"
  ( cd "${oss_root}/crates" && cargo build --release -p valv-cli -p valvd --target "${target}" )
  build_dir="${oss_root}/crates/target/${target}/release"
  [[ -f "${build_dir}/valv" && -f "${build_dir}/valvd" ]] ||
    fail "cargo build did not produce valv and valvd in ${build_dir}"
  cp "${build_dir}/valv" "${build_dir}/valvd" "${tmp_dir}/payload/"
else
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

  tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}/payload"
  [[ -f "${tmp_dir}/payload/valv" && -f "${tmp_dir}/payload/valvd" ]] ||
    fail "${asset} did not contain valv and valvd"
fi

codesign --force --sign "${identity}" --options runtime --timestamp "${tmp_dir}/payload/valv"
codesign --force --sign "${identity}" --options runtime --timestamp "${tmp_dir}/payload/valvd"
codesign -dv --verbose=4 "${tmp_dir}/payload/valv" >/dev/null
codesign -dv --verbose=4 "${tmp_dir}/payload/valvd" >/dev/null

echo "==> Notarizing valv/valvd"
notarize_zip="${tmp_dir}/valv-cli-notarize.zip"
ditto -c -k --keepParent "${tmp_dir}/payload" "${notarize_zip}"
xcrun notarytool submit "${notarize_zip}" \
  --keychain-profile "${notary_profile}" \
  --wait

# Persist the signed binaries to a cargo-safe handoff dir so release-app.sh can embed
# the exact same artifacts without a flaky GitHub CDN round-trip.
signed_dir="${oss_root}/crates/target/signed-cli"
mkdir -p "${signed_dir}"
cp "${tmp_dir}/payload/valv" "${tmp_dir}/payload/valvd" "${signed_dir}/"

tar -C "${tmp_dir}/payload" -czf "${tmp_dir}/${asset}" valv valvd
digest="$(sha256_file "${tmp_dir}/${asset}")"

if [[ "${local_mode}" -eq 1 ]]; then
  echo "${digest}  ${asset}" > "${tmp_dir}/SHA256SUMS"
  echo "==> --local SHA256SUMS covers macOS (${target}) only; macOS cannot cross-compile the Linux target, so the x86_64-unknown-linux-gnu row is absent by design"
else
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
fi

minisign -S -s "${minisign_key_file}" \
  -m "${tmp_dir}/SHA256SUMS" \
  -x "${tmp_dir}/SHA256SUMS.minisig" \
  -t "valv release ${tag} (macOS re-sign)"

if [[ "${local_mode}" -eq 1 ]]; then
  local_out_dir="${oss_root}/crates/target/local-release"
  mkdir -p "${local_out_dir}"
  cp "${tmp_dir}/${asset}" "${tmp_dir}/SHA256SUMS" "${tmp_dir}/SHA256SUMS.minisig" "${local_out_dir}/"
  echo "==> --local build complete; nothing uploaded"
  echo "    tarball:        ${local_out_dir}/${asset}"
  echo "    SHA256SUMS:     ${local_out_dir}/SHA256SUMS"
  echo "    minisig:        ${local_out_dir}/SHA256SUMS.minisig"
  echo "    signed-cli dir: ${signed_dir}"
else
  gh release upload "${tag}" \
    --repo "${repo}" \
    --clobber \
    "${tmp_dir}/${asset}" \
    "${tmp_dir}/SHA256SUMS" \
    "${tmp_dir}/SHA256SUMS.minisig"

  echo "Uploaded signed ${asset}, updated SHA256SUMS, and re-signed SHA256SUMS.minisig to ${repo} ${tag}"
fi
