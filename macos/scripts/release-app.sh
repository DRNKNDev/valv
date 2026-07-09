#!/usr/bin/env bash
set -euo pipefail


repo="${VALV_GITHUB_REPO:-DRNKNDev/valv}"
scheme="Valv"
bundle_id="dev.drnkn.valv"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../../.." && pwd)"
project="${repo_root}/oss/macos/Valv/Valv.xcodeproj"
crates_dir="${repo_root}/oss/crates"

fail() {
  echo "valv release-app: $*" >&2
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

crate_version() {
  awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' "$1"
}

usage() {
  cat >&2 <<'USAGE'
Usage:
  DEVELOPER_ID_APPLICATION="Developer ID Application: Name (TEAMID)" \
  APPLE_TEAM_ID="TEAMID" \
  NOTARY_PROFILE="valv-notary" \
    oss/macos/scripts/release-app.sh v0.1.0

Produces Valv-<version>.dmg (signed + notarized + stapled) and uploads it plus
its SHA-256 to the <tag> GitHub Release on ${VALV_GITHUB_REPO:-DRNKNDev/valv}.
USAGE
}

tag="${1:-}"
identity="${DEVELOPER_ID_APPLICATION:-}"
team_id="${APPLE_TEAM_ID:-}"
notary_profile="${NOTARY_PROFILE:-}"

[[ -n "${tag}" ]] || { usage; fail "missing tag"; }
[[ "${tag}" == v* ]] || fail "tag must start with v (e.g. v0.1.0)"
[[ -n "${identity}" ]] || { usage; fail "missing DEVELOPER_ID_APPLICATION"; }
[[ -n "${team_id}" ]] || { usage; fail "missing APPLE_TEAM_ID"; }
[[ -n "${notary_profile}" ]] || { usage; fail "missing NOTARY_PROFILE"; }

need xcodebuild
need xcrun
need cargo
need gh
need hdiutil
need ditto
need plutil
need awk
need mktemp

version="${tag#v}"

cli_version="$(crate_version "${crates_dir}/valv-cli/Cargo.toml")"
daemon_version="$(crate_version "${crates_dir}/valvd/Cargo.toml")"
[[ "${cli_version}" == "${version}" ]] ||
  fail "tag ${tag} != valv-cli Cargo.toml version ${cli_version}"
[[ "${daemon_version}" == "${version}" ]] ||
  fail "tag ${tag} != valvd Cargo.toml version ${daemon_version}"

work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT
archive_path="${work_dir}/Valv.xcarchive"
export_dir="${work_dir}/export"
app_path="${export_dir}/Valv.app"
notarize_zip="${work_dir}/Valv-notarize.zip"
dmg_staging="${work_dir}/dmg"
dmg_path="${work_dir}/Valv-${version}.dmg"
export_plist="${work_dir}/ExportOptions.plist"

echo "==> Building release valv/valvd for embedding"
( cd "${crates_dir}" && cargo build --release -p valv-cli -p valvd )

echo "==> Archiving ${scheme}"
xcodebuild archive \
  -project "${project}" \
  -scheme "${scheme}" \
  -configuration Release \
  -archivePath "${archive_path}" \
  -destination "generic/platform=macOS" \
  CODE_SIGN_STYLE=Manual \
  DEVELOPMENT_TEAM="${team_id}" \
  "CODE_SIGN_IDENTITY=${identity}"

if [[ -n "${EXPORT_OPTIONS_PLIST:-}" ]]; then
  [[ -f "${EXPORT_OPTIONS_PLIST}" ]] || fail "EXPORT_OPTIONS_PLIST not found: ${EXPORT_OPTIONS_PLIST}"
  cp "${EXPORT_OPTIONS_PLIST}" "${export_plist}"
else
  cat > "${export_plist}" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>developer-id</string>
  <key>teamID</key><string>${team_id}</string>
  <key>signingStyle</key><string>manual</string>
  <key>signingCertificate</key><string>${identity}</string>
</dict>
</plist>
PLIST
fi
plutil -lint "${export_plist}" >/dev/null

echo "==> Exporting signed app"
xcodebuild -exportArchive \
  -archivePath "${archive_path}" \
  -exportPath "${export_dir}" \
  -exportOptionsPlist "${export_plist}"

[[ -d "${app_path}" ]] || fail "export did not produce ${app_path}"

echo "==> Notarizing"
ditto -c -k --keepParent "${app_path}" "${notarize_zip}"
xcrun notarytool submit "${notarize_zip}" \
  --keychain-profile "${notary_profile}" \
  --wait

echo "==> Stapling"
xcrun stapler staple "${app_path}"
xcrun stapler validate "${app_path}" >/dev/null

echo "==> Packaging ${dmg_path##*/}"
mkdir -p "${dmg_staging}"
ditto "${app_path}" "${dmg_staging}/Valv.app"
ln -s /Applications "${dmg_staging}/Applications"
hdiutil create \
  -volname "Valv ${version}" \
  -srcfolder "${dmg_staging}" \
  -fs HFS+ \
  -format UDZO \
  -ov \
  "${dmg_path}"

spctl --assess --type execute --verbose=4 "${app_path}" 2>&1 || true

digest="$(sha256_file "${dmg_path}")"
checksum_file="${work_dir}/Valv-${version}.dmg.sha256"
echo "${digest}  Valv-${version}.dmg" > "${checksum_file}"

echo "==> Uploading to ${repo} ${tag}"
gh release upload "${tag}" \
  --repo "${repo}" \
  --clobber \
  "${dmg_path}" \
  "${checksum_file}"

echo "Published Valv-${version}.dmg (sha256 ${digest}) to ${repo} ${tag}"
