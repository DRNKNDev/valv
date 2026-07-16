#!/usr/bin/env bash
set -euo pipefail


repo="${VALV_GITHUB_REPO:-DRNKNDev/valv}"
scheme="Valv"
bundle_id="dev.drnkn.valv"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
oss_root="$(cd "${script_dir}/../.." && pwd)"
project="${oss_root}/macos/Valv/Valv.xcodeproj"
crates_dir="${oss_root}/crates"

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

resolve_latest_version() {
  local prefix="$1"
  local tags
  tags="$(gh api "repos/${repo}/releases" --paginate --jq '.[].tag_name')" ||
    fail "failed to list releases for ${repo}"

  local best="" best_major=-1 best_minor=-1 best_patch=-1
  local candidate version major minor patch
  while IFS= read -r candidate; do
    [[ -n "${candidate}" ]] || continue
    case "${candidate}" in
      "${prefix}"*)
        version="${candidate#"${prefix}"}"
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

  [[ -n "${best}" ]] || fail "no ${prefix}* release found for ${repo}"
  echo "${best}"
}

usage() {
  cat >&2 <<'USAGE'
Usage:
  APPLE_TEAM_ID="TEAMID" \
  NOTARY_PROFILE="valv-notary" \
  VALV_RELEASE_NOTES_FILE="/path/to/notes.md" \
    macos/scripts/release-app.sh [--dry-run] [--cli-version 0.3.1] \
      [--valvd-version 0.3.0] macos-v0.1.0

Produces Valv-<version>.dmg (signed + notarized + stapled) and uploads it, its
SHA-256, and any Sparkle deltas to the <tag> GitHub Release on
${VALV_GITHUB_REPO:-DRNKNDev/valv}.

Resolves the latest signed released valv/valvd (highest semver among cli-v*
and valvd-v* releases) once per run, downloads each release's tarball,
verifies it carries a hardened-runtime signature, and embeds it in the app
bundle. Pass --cli-version / --valvd-version to pin the embedded binaries to
an explicit version instead of resolving latest, for reproducible builds.
The resolved versions are printed and recorded in the release notes and the
appcast item description.

--dry-run runs the full archive/export/notarize/staple/DMG/appcast-generation
flow (so signing, notarization, and Sparkle key problems still surface), but
skips gh release upload and the post-upload enclosure assertion, defaults
VALV_DMG_ARCHIVE_DIR to a fresh temp dir instead of ~/.valv-release-dmgs, and
prints a diff of the generated appcast instead of overwriting the tracked
oss/macos/appcast.xml. The produced .dmg path is printed at the end so the
operator can mount and install it by hand.

VALV_RELEASE_NOTES_FILE is the markdown shown in the in-app update dialog. It
may be a trimmed version of the GitHub Release body (see
tooling/release/release-notes.md). The resolved valv/valvd versions are
appended to it (or used as the sole content if unset).
USAGE
}

dry_run=0
cli_version_override=""
valvd_version_override=""
positional=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      dry_run=1
      shift
      ;;
    --cli-version)
      [[ $# -ge 2 ]] || fail "--cli-version requires a value"
      cli_version_override="${2#v}"
      shift 2
      ;;
    --valvd-version)
      [[ $# -ge 2 ]] || fail "--valvd-version requires a value"
      valvd_version_override="${2#v}"
      shift 2
      ;;
    *)
      positional+=("$1")
      shift
      ;;
  esac
done

tag="${positional[0]:-}"
team_id="${APPLE_TEAM_ID:-}"
notary_profile="${NOTARY_PROFILE:-}"

[[ -n "${tag}" ]] || { usage; fail "missing tag"; }
[[ "${tag}" == macos-v* ]] || fail "tag must start with macos-v (e.g. macos-v0.1.0)"
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

version="${tag#macos-v}"

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

appcast_path="${oss_root}/macos/appcast.xml"
previous_build_number=0
if [[ -f "${appcast_path}" ]]; then
  latest_from_appcast="$(grep -oE 'sparkle:version(="|>)[0-9]+' "${appcast_path}" | grep -oE '[0-9]+' | sort -n | tail -1 || true)"
  [[ -n "${latest_from_appcast}" ]] && previous_build_number="${latest_from_appcast}"
fi
(( current_project_version > previous_build_number )) ||
  fail "${scheme} CURRENT_PROJECT_VERSION (${current_project_version}) must be strictly greater than the previously published app release's build number (${previous_build_number}): Sparkle orders updates by this field"

work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT
archive_path="${work_dir}/Valv.xcarchive"
export_dir="${work_dir}/export"
app_path="${export_dir}/Valv.app"
notarize_zip="${work_dir}/Valv-notarize.zip"
dmg_staging="${work_dir}/dmg"
dmg_path="${work_dir}/Valv-${version}.dmg"
export_plist="${work_dir}/ExportOptions.plist"

echo "==> Resolving latest signed released valv/valvd"
cli_resolved_version="${cli_version_override}"
[[ -n "${cli_resolved_version}" ]] || cli_resolved_version="$(resolve_latest_version "cli-v")"
valvd_resolved_version="${valvd_version_override}"
[[ -n "${valvd_resolved_version}" ]] || valvd_resolved_version="$(resolve_latest_version "valvd-v")"
echo "==> Embedding valv ${cli_resolved_version} and valvd ${valvd_resolved_version}"

release_dir="${crates_dir}/target/release"
mkdir -p "${release_dir}"
embed_target="aarch64-apple-darwin"
embedded_dir="${work_dir}/embedded"
mkdir -p "${embedded_dir}"

fetch_signed_binary() {
  local binary="$1" prefix="$2" resolved_version="$3"
  local component_tag="${prefix}${resolved_version}"
  local asset="${binary}-${resolved_version}-${embed_target}.tar.gz"
  gh release download "${component_tag}" \
    --repo "${repo}" \
    --pattern "${asset}" \
    --dir "${embedded_dir}" \
    --clobber ||
    fail "failed to download ${asset} from ${repo} ${component_tag}"
  tar -xzf "${embedded_dir}/${asset}" -C "${embedded_dir}"
  [[ -x "${embedded_dir}/${binary}" ]] \
    || fail "${asset} did not contain an executable ${binary}"
  local sig
  sig="$(codesign -dv --verbose=2 "${embedded_dir}/${binary}" 2>&1 || true)"
  [[ "${sig}" == *"(runtime)"* ]] \
    || fail "${binary} from ${repo} ${component_tag} lacks hardened runtime - run sign-cli-binaries.sh for ${component_tag} first"
  cp "${embedded_dir}/${binary}" "${release_dir}/${binary}"
}

fetch_signed_binary valv "cli-v" "${cli_resolved_version}"
fetch_signed_binary valvd "valvd-v" "${valvd_resolved_version}"

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

generate_appcast_bin="$(command -v generate_appcast 2>/dev/null || find "${HOME}/Library/Developer/Xcode/DerivedData" -name generate_appcast -path '*sparkle*' 2>/dev/null | head -1)"
[[ -x "${generate_appcast_bin}" ]] || fail "generate_appcast not found on PATH or in DerivedData Sparkle artifacts (build the app once so SPM resolves Sparkle's tools)"

if [[ "${dry_run}" -eq 1 ]]; then
  dmg_archive_dir="$(mktemp -d)"
else
  dmg_archive_dir="${VALV_DMG_ARCHIVE_DIR:-${HOME}/.valv-release-dmgs}"
fi
mkdir -p "${dmg_archive_dir}"
echo "==> Archiving Valv-${version}.dmg into the local release archive (${dmg_archive_dir})"
cp "${dmg_path}" "${dmg_archive_dir}/Valv-${version}.dmg"

appcast_output="${dmg_archive_dir}/appcast.xml"
if [[ ! -f "${appcast_output}" && -f "${appcast_path}" ]]; then
  cp "${appcast_path}" "${appcast_output}"
fi

notes_file="${VALV_RELEASE_NOTES_FILE:-}"
release_notes_path="${dmg_archive_dir}/Valv-${version}.md"
embedded_versions_line="Embedded: valv ${cli_resolved_version}, valvd ${valvd_resolved_version}"
if [[ -n "${notes_file}" ]]; then
  [[ -f "${notes_file}" ]] || fail "VALV_RELEASE_NOTES_FILE not found: ${notes_file}"
  cp "${notes_file}" "${release_notes_path}"
  printf '\n%s\n' "${embedded_versions_line}" >> "${release_notes_path}"
  echo "==> Release notes staged as Valv-${version}.md (embedded versions recorded)"
else
  printf '%s\n' "${embedded_versions_line}" > "${release_notes_path}"
  echo "==> VALV_RELEASE_NOTES_FILE unset - ${tag}'s appcast description will only record the embedded valv/valvd versions" >&2
fi

echo "==> Generating signed appcast entry"
generate_appcast_args=("${dmg_archive_dir}" --embed-release-notes)
if [[ -n "${SPARKLE_ED_KEY_FILE:-}" ]]; then
  generate_appcast_args+=(--ed-key-file "${SPARKLE_ED_KEY_FILE}")
fi
generate_appcast_args+=(--download-url-prefix "${SPARKLE_DOWNLOAD_URL_PREFIX:-https://github.com/${repo}/releases/download/${tag}/}")
"${generate_appcast_bin}" "${generate_appcast_args[@]}"

[[ -f "${appcast_output}" ]] || fail "generate_appcast did not produce ${appcast_output}"

# --download-url-prefix applies this tag's prefix to every archive in the dir, so
# restore each past item's enclosure/delta URL from the previously published
# appcast instead of guessing its tag from the DMG version (past items may
# predate the macos-v* scheme and carry a bare v* tag).
if [[ -f "${appcast_path}" ]]; then
  while IFS=$'\t' read -r prev_tag prev_file; do
    [[ -n "${prev_tag}" && -n "${prev_file}" ]] || continue
    escaped_file="${prev_file//./\\.}"
    sed -i '' -E \
      "s#releases/download/[^/\"]+/${escaped_file}#releases/download/${prev_tag}/${prev_file}#g" \
      "${appcast_output}"
  done < <(grep -oE 'releases/download/[^/"]+/[^"]+\.(dmg|delta)' "${appcast_path}" | awk -F/ '{print $(NF-1)"\t"$NF}')
fi

if [[ "${dry_run}" -eq 1 ]]; then
  echo "==> --dry-run: not overwriting ${appcast_path}; diff of what a real run would write:"
  diff -u "${appcast_path}" "${appcast_output}" || true
else
  cp "${appcast_output}" "${appcast_path}"
  echo "==> Wrote ${appcast_path}"
fi

if [[ "${dry_run}" -eq 1 ]]; then
  echo "==> --dry-run: skipping gh release upload"
else
  if ! gh release view "${tag}" --repo "${repo}" >/dev/null 2>&1; then
    echo "==> Creating ${repo} release ${tag}"
    gh release create "${tag}" \
      --repo "${repo}" \
      --title "${tag}" \
      --notes-file "${release_notes_path}"
  fi

  echo "==> Uploading to ${repo} ${tag}"
  upload_files=("${dmg_path}" "${checksum_file}")
  while IFS= read -r delta; do
    upload_files+=("${delta}")
  done < <(find "${dmg_archive_dir}" -maxdepth 1 -name "Valv${current_project_version}-*.delta")
  gh release upload "${tag}" \
    --repo "${repo}" \
    --clobber \
    "${upload_files[@]}"

  echo "Published Valv-${version}.dmg (sha256 ${digest}) to ${repo} ${tag}"

  echo "==> Verifying every ${tag} enclosure resolves to an uploaded asset"
  assets="$(gh release view "${tag}" --repo "${repo}" --json assets --jq '.assets[].name')"
  while IFS= read -r asset; do
    [[ -z "${asset}" ]] && continue
    grep -qx "${asset}" <<<"${assets}" ||
      fail "appcast references ${asset} under ${tag}, but no such asset exists on the release"
  done < <(grep -oE "releases/download/${tag}/[^\"]+" "${appcast_path}" | sed -E 's#.*/##')
fi

if [[ "${dry_run}" -eq 1 ]]; then
  cat <<STEPS

==> --dry-run complete: nothing was uploaded, and ${appcast_path} was not
    modified (diff shown above). Mount and install the DMG to verify it:
      ${dmg_archive_dir}/Valv-${version}.dmg
    Re-run without --dry-run to publish for real.
STEPS
else
  cat <<STEPS

==> Appcast regenerated locally but NOT yet published. Complete the publish
    sequence by hand (design D2/D4, tooling/release/release-notes.md):
      1. review the diff:  git -C "${oss_root}" diff -- macos/appcast.xml
      2. git -C "${oss_root}" add macos/appcast.xml
      3. git -C "${oss_root}" commit -m "chore(macos): publish appcast entry for ${tag}"
      4. git -C "${oss_root}" push
      5. wait for private/apps/web's Pages deploy to finish
      6. curl -fsSL https://valvsync.com/appcast.xml | grep -- "${version}"
         (confirm the new version's entry is actually live before calling this
         release done - an unpublished appcast is safe, but not shipped)
STEPS
fi
