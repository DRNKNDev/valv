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
  local prefix="$1" pin_env="$2"
  local pin_value="${!pin_env:-}"
  if [[ -n "${pin_value}" ]]; then
    echo "${pin_value#v}"
    return
  fi

  local page=1 releases_json best=""
  local best_major=-1 best_minor=-1 best_patch=-1
  while :; do
    releases_json="${tmp_dir}/releases-${prefix}-${page}.json"
    curl -fsSL "https://api.github.com/repos/${repo}/releases?per_page=100&page=${page}" -o "${releases_json}" ||
      fail "failed to list releases for ${repo}"

    local tags
    tags="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "${releases_json}")"
    local tag_count=0
    local tag version major minor patch
    while IFS= read -r tag; do
      [[ -n "${tag}" ]] || continue
      tag_count=$((tag_count + 1))
      case "${tag}" in
        "${prefix}"*)
          version="${tag#"${prefix}"}"
          if [[ "${version}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
            major="${BASH_REMATCH[1]}"
            minor="${BASH_REMATCH[2]}"
            patch="${BASH_REMATCH[3]}"
            if (( major > best_major || (major == best_major && minor > best_minor) || (major == best_major && minor == best_minor && patch > best_patch) )); then
              best_major="${major}"
              best_minor="${minor}"
              best_patch="${patch}"
              best="${version}"
            fi
          fi
          ;;
      esac
    done <<< "${tags}"

    [[ "${tag_count}" -eq 100 ]] || break
    page=$((page + 1))
  done

  [[ -n "${best}" ]] || fail "no ${prefix}* release found for ${repo}"
  echo "${best}"
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
  local asset="$1" checksums_file="$2"
  awk -v asset="${asset}" '
    $2 == asset || $2 == "*" asset { print $1; found = 1; exit }
    END { if (!found) exit 1 }
  ' "${checksums_file}"
}

install_component() {
  local binary="$1" prefix="$2" pin_env="$3"
  local version tag asset release_base archive checksums_file expected actual extract_dir

  version="$(resolve_version "${prefix}" "${pin_env}")"
  tag="${prefix}${version}"
  asset="${binary}-${version}-${target}.tar.gz"
  release_base="https://github.com/${repo}/releases/download/${tag}"
  archive="${tmp_dir}/${asset}"
  checksums_file="${tmp_dir}/${binary}-SHA256SUMS"
  extract_dir="${tmp_dir}/extract-${binary}"

  curl -fsSL "${release_base}/${asset}" -o "${archive}" ||
    fail "failed to download ${asset} from ${repo} ${tag}"
  curl -fsSL "${release_base}/SHA256SUMS" -o "${checksums_file}" ||
    fail "failed to download SHA256SUMS from ${repo} ${tag}"

  expected="$(checksum_for_asset "${asset}" "${checksums_file}")" ||
    fail "SHA256SUMS does not contain ${asset}"
  actual="$(sha256_file "${archive}")"
  [[ "${actual}" == "${expected}" ]] ||
    fail "checksum mismatch for ${asset}: expected ${expected}, got ${actual}"

  mkdir -p "${extract_dir}"
  tar -xzf "${archive}" -C "${extract_dir}"
  [[ -f "${extract_dir}/${binary}" ]] ||
    fail "${asset} did not contain ${binary}"

  mkdir -p "${install_dir}"
  install -m 0755 "${extract_dir}/${binary}" "${install_dir}/.${binary}.tmp.$$"
  mv "${install_dir}/.${binary}.tmp.$$" "${install_dir}/${binary}"

  echo "${version}"
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

valv_version="$(install_component valv cli-v VALV_CLI_VERSION)"
valvd_version="$(install_component valvd valvd-v VALVD_VERSION)"

echo "Installed valv ${valv_version} and valvd ${valvd_version} to ${install_dir}"
case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *)
    echo "Add ${install_dir} to PATH to run valv from any shell."
    ;;
esac
