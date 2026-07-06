#!/usr/bin/env bash
set -euo pipefail

repo="${VALV_GITHUB_REPO:-DRNKNDev/valv}"
install_dir="${VALV_INSTALL_DIR:-${HOME}/.local/bin}"

fail() {
  echo "valv install: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "${os}:${arch}" in
    Darwin:arm64|Darwin:aarch64)
      echo "aarch64-apple-darwin"
      ;;
    Linux:x86_64|Linux:amd64)
      echo "x86_64-unknown-linux-gnu"
      ;;
    *)
      fail "unsupported platform ${os}/${arch}; supported targets are macOS arm64 and Linux x86_64"
      ;;
  esac
}

resolve_version() {
  if [[ -n "${VALV_VERSION:-}" ]]; then
    echo "${VALV_VERSION#v}"
    return
  fi

  local latest_json tag
  latest_json="${tmp_dir}/latest.json"
  curl -fsSL "https://api.github.com/repos/${repo}/releases/latest" -o "${latest_json}" ||
    fail "failed to resolve latest release for ${repo}"
  tag="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "${latest_json}" | head -n 1)"
  [[ -n "${tag}" ]] || fail "latest release response did not include tag_name"
  echo "${tag#v}"
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    fail "missing required command: sha256sum or shasum"
  fi
}

checksum_for_asset() {
  local asset="$1"
  awk -v asset="${asset}" '
    $2 == asset || $2 == "*" asset { print $1; found = 1; exit }
    END { if (!found) exit 1 }
  ' "${checksums_file}"
}

need curl
need tar
need awk
need sed
need mktemp
need install

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

target="$(detect_target)"
version="$(resolve_version)"
tag="v${version}"
asset="valv-${version}-${target}.tar.gz"
release_base="https://github.com/${repo}/releases/download/${tag}"
archive="${tmp_dir}/${asset}"
checksums_file="${tmp_dir}/SHA256SUMS"
extract_dir="${tmp_dir}/extract"

curl -fsSL "${release_base}/${asset}" -o "${archive}" ||
  fail "failed to download ${asset} from ${repo} ${tag}"
curl -fsSL "${release_base}/SHA256SUMS" -o "${checksums_file}" ||
  fail "failed to download SHA256SUMS from ${repo} ${tag}"

expected="$(checksum_for_asset "${asset}")" ||
  fail "SHA256SUMS does not contain ${asset}"
actual="$(sha256_file "${archive}")"
[[ "${actual}" == "${expected}" ]] ||
  fail "checksum mismatch for ${asset}: expected ${expected}, got ${actual}"

mkdir -p "${extract_dir}"
tar -xzf "${archive}" -C "${extract_dir}"
[[ -f "${extract_dir}/valv" && -f "${extract_dir}/valvd" ]] ||
  fail "${asset} did not contain valv and valvd"

mkdir -p "${install_dir}"
install -m 0755 "${extract_dir}/valv" "${install_dir}/.valv.tmp.$$"
install -m 0755 "${extract_dir}/valvd" "${install_dir}/.valvd.tmp.$$"
mv "${install_dir}/.valv.tmp.$$" "${install_dir}/valv"
mv "${install_dir}/.valvd.tmp.$$" "${install_dir}/valvd"

echo "Installed valv and valvd ${version} to ${install_dir}"
case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *)
    echo "Add ${install_dir} to PATH to run valv from any shell."
    ;;
esac
