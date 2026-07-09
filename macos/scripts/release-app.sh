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
  APPLE_TEAM_ID="TEAMID" \
  NOTARY_PROFILE="valv-notary" \
    oss/macos/scripts/release-app.sh v0.1.0

Produces Valv-<version>.dmg (signed + notarized + stapled) and uploads it plus
its SHA-256 to the <tag> GitHub Release on ${VALV_GITHUB_REPO:-DRNKNDev/valv}.
USAGE
}

tag="${1:-}"
team_id="${APPLE_TEAM_ID:-}"
notary_profile="${NOTARY_PROFILE:-}"

[[ -n "${tag}" ]] || { usage; fail "missing tag"; }
[[ "${tag}" == v* ]] || fail "tag must start with v (e.g. v0.1.0)"
[[ -n "${team_id}" ]] || { usage; fail "missing APPLE_TEAM_ID"; }
[[ -n "${notary_profile}" ]] || { usage; fail "missing NOTARY_PROFILE"; }

need xcodebuild
need xcrun
need gh
need tar
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

echo "==> Verifying app bundle version scheme (MARKETING_VERSION / CURRENT_PROJECT_VERSION)"
app_build_settings="$(xcodebuild -showBuildSettings \
  -project "${project}" \
  -scheme "${scheme}" \
  -configuration Release 2>/dev/null)"
marketing_version="$(printf '%s\n' "${app_build_settings}" | awk -F'= ' '/ MARKETING_VERSION / { print $2; exit }')"
current_project_version="$(printf '%s\n' "${app_build_settings}" | awk -F'= ' '/ CURRENT_PROJECT_VERSION / { print $2; exit }')"

[[ -n "${marketing_version}" ]] ||
  fail "could not read MARKETING_VERSION from ${scheme}'s Release build settings"
[[ -n "${current_project_version}" ]] ||
  fail "could not read CURRENT_PROJECT_VERSION from ${scheme}'s Release build settings"
[[ "${marketing_version}" == "${version}" ]] ||
  fail "tag ${tag} != ${scheme} MARKETING_VERSION ${marketing_version} (task 1.4/D9: set MARKETING_VERSION to the tag's semver before releasing)"
[[ "${current_project_version}" =~ ^[0-9]+$ ]] ||
  fail "${scheme} CURRENT_PROJECT_VERSION (${current_project_version}) must be a plain integer, not a semver or other string (D9)"

appcast_path="${repo_root}/oss/macos/appcast.xml"
previous_build_number=0
if [[ -f "${appcast_path}" ]]; then
  latest_from_appcast="$(grep -o 'sparkle:version="[0-9]\+"' "${appcast_path}" | grep -o '[0-9]\+' | sort -n | tail -1 || true)"
  [[ -n "${latest_from_appcast}" ]] && previous_build_number="${latest_from_appcast}"
fi
(( current_project_version > previous_build_number )) ||
  fail "${scheme} CURRENT_PROJECT_VERSION (${current_project_version}) must be strictly greater than the previously published app release's build number (${previous_build_number}) — Sparkle orders updates by this field"

work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT
archive_path="${work_dir}/Valv.xcarchive"
export_dir="${work_dir}/export"
app_path="${export_dir}/Valv.app"
notarize_zip="${work_dir}/Valv-notarize.zip"
dmg_staging="${work_dir}/dmg"
dmg_path="${work_dir}/Valv-${version}.dmg"
export_plist="${work_dir}/ExportOptions.plist"

echo "==> Embedding signed valv/valvd left by sign-cli-binaries.sh"
release_dir="${crates_dir}/target/release"
signed_dir="${crates_dir}/target/signed-cli"
for embedded_bin in valvd valv; do
  [[ -x "${signed_dir}/${embedded_bin}" ]] \
    || fail "${signed_dir}/${embedded_bin} not found - run sign-cli-binaries.sh for ${tag} first"
  sig="$(codesign -dv --verbose=2 "${signed_dir}/${embedded_bin}" 2>&1 || true)"
  [[ "${sig}" == *"(runtime)"* ]] \
    || fail "${signed_dir}/${embedded_bin} lacks hardened runtime - re-run sign-cli-binaries.sh for ${tag}"
done
mkdir -p "${release_dir}"
cp "${signed_dir}/valv" "${signed_dir}/valvd" "${release_dir}/"

echo "==> Archiving ${scheme}"
xcodebuild archive \
  -project "${project}" \
  -scheme "${scheme}" \
  -configuration Release \
  -archivePath "${archive_path}" \
  -destination "generic/platform=macOS" \
  -allowProvisioningUpdates \
  DEVELOPMENT_TEAM="${team_id}"

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
  <key>signingStyle</key><string>automatic</string>
</dict>
</plist>
PLIST
fi
plutil -lint "${export_plist}" >/dev/null

echo "==> Exporting signed app"
xcodebuild -exportArchive \
  -archivePath "${archive_path}" \
  -exportPath "${export_dir}" \
  -exportOptionsPlist "${export_plist}" \
  -allowProvisioningUpdates

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

generate_appcast_bin="$(command -v generate_appcast 2>/dev/null || find "${HOME}/Library/Developer/Xcode/DerivedData" -name generate_appcast -path '*sparkle*' 2>/dev/null | head -1)"
[[ -x "${generate_appcast_bin}" ]] || fail "generate_appcast not found on PATH or in DerivedData Sparkle artifacts (build the app once so SPM resolves Sparkle's tools)"

dmg_archive_dir="${VALV_DMG_ARCHIVE_DIR:-${HOME}/.valv-release-dmgs}"
mkdir -p "${dmg_archive_dir}"
echo "==> Archiving Valv-${version}.dmg into the local release archive (${dmg_archive_dir})"
cp "${dmg_path}" "${dmg_archive_dir}/Valv-${version}.dmg"

echo "==> Generating signed appcast entry"
generate_appcast_args=("${dmg_archive_dir}")
if [[ -n "${SPARKLE_ED_KEY_FILE:-}" ]]; then
  generate_appcast_args+=(--ed-key-file "${SPARKLE_ED_KEY_FILE}")
fi
generate_appcast_args+=(--download-url-prefix "${SPARKLE_DOWNLOAD_URL_PREFIX:-https://github.com/${repo}/releases/download/${tag}/}")
"${generate_appcast_bin}" "${generate_appcast_args[@]}"

appcast_output="${dmg_archive_dir}/appcast.xml"
[[ -f "${appcast_output}" ]] || fail "generate_appcast did not produce ${appcast_output}"

cp "${appcast_output}" "${appcast_path}"
echo "==> Wrote ${appcast_path}"

cat <<STEPS

==> Appcast regenerated locally but NOT yet published. Complete the publish
    sequence by hand (design D2/D4, tooling/release/release-notes.md):
      1. review the diff:  git -C "${repo_root}" diff -- oss/macos/appcast.xml
      2. git -C "${repo_root}" add oss/macos/appcast.xml
      3. git -C "${repo_root}" commit -m "chore(macos): publish appcast entry for ${tag}"
      4. git -C "${repo_root}" push
      5. wait for private/apps/web's Pages deploy to finish
      6. curl -fsSL https://valvsync.com/appcast.xml | grep -- "${version}"
         (confirm the new version's entry is actually live before calling this
         release done - an unpublished appcast is safe, but not shipped)
STEPS
