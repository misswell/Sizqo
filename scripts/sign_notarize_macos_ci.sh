#!/bin/bash
# Sign, notarize, staple, and package the three Direct macOS distributions.
# Intended for the ephemeral keychain created by .github/workflows/release.yml.

set -euo pipefail

ARM_APP="${1:?arm64 app path required}"
INTEL_APP="${2:?x86_64 app path required}"
UNIVERSAL_APP="${3:?universal app path required}"
OUTPUT_DIR="${4:?output directory required}"

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ENTITLEMENTS="${ENTITLEMENTS:-${PROJECT_DIR}/src-tauri/entitlements.plist}"
SIGNING_IDENTITY="${SIGNING_IDENTITY:-Developer ID Application: Guofeng Liu (U8U443D7ZL)}"
SIGNING_KEYCHAIN="${SIGNING_KEYCHAIN:?SIGNING_KEYCHAIN is required}"
RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG is required}"

# Notarization credentials, resolved the same way as scripts/notarize.sh:
# an App Store Connect API key first (works non-interactively and needs no
# app-specific password), falling back to the Apple ID + app-specific password
# pair. Never default to an inherited upstream credential.
NOTARY_ARGS=()
NOTARY_KEY_FILE=""
if [ -n "${APPLE_API_KEY:-}" ] && [ -n "${APPLE_API_KEY_ID:-}" ] && [ -n "${APPLE_API_ISSUER:-}" ]; then
  NOTARY_KEY_FILE="$(mktemp "${TMPDIR:-/tmp}/sizqo-notary-key.XXXXXX")"
  printf '%s' "${APPLE_API_KEY}" | base64 -D > "${NOTARY_KEY_FILE}"
  chmod 600 "${NOTARY_KEY_FILE}"
  NOTARY_ARGS=(--key "${NOTARY_KEY_FILE}" --key-id "${APPLE_API_KEY_ID}" --issuer "${APPLE_API_ISSUER}")
  echo "==> Notarization credentials: App Store Connect API key (${APPLE_API_KEY_ID})"
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" ]; then
  APPLE_TEAM_ID="${APPLE_TEAM_ID:?APPLE_TEAM_ID is required when notarizing with an Apple ID}"
  NOTARY_ARGS=(--apple-id "${APPLE_ID}" --team-id "${APPLE_TEAM_ID}" --password "${APPLE_APP_SPECIFIC_PASSWORD}")
  echo "==> Notarization credentials: Apple ID + app-specific password"
else
  echo "Missing notarization credentials: provide APPLE_API_KEY + APPLE_API_KEY_ID + APPLE_API_ISSUER," >&2
  echo "or APPLE_ID + APPLE_APP_SPECIFIC_PASSWORD + APPLE_TEAM_ID." >&2
  exit 2
fi

for app in "${ARM_APP}" "${INTEL_APP}" "${UNIVERSAL_APP}"; do
  [ -d "${app}" ] || { echo "Missing app: ${app}" >&2; exit 1; }
done
[ -f "${ENTITLEMENTS}" ] || { echo "Missing entitlements: ${ENTITLEMENTS}" >&2; exit 1; }
security find-identity -v -p codesigning "${SIGNING_KEYCHAIN}" | grep -F "${SIGNING_IDENTITY}"
mkdir -p "${OUTPUT_DIR}"

log() { echo "==> $*"; }

# Every artifact a user can actually receive must satisfy all four of these.
# The shipped v0.2.0 updater archive failed every one of these and the app inside
# the DMG failed the ticket check, while the pipeline stayed green — the old checks only
# ever ran against the build-directory app and the dmg envelope. spctl cannot
# stand in for the ticket check: online it reports "Notarized Developer ID" even
# for a bundle with no ticket attached.
assert_distributable() {
  local app="$1"
  local label="$2"
  local csinfo
  if [ ! -d "${app}" ]; then
    echo "Missing app inside ${label}: ${app}" >&2
    exit 1
  fi
  # Capture then match with case: this script runs under pipefail, so
  # `codesign | grep -q` would let grep exit first, hand codesign a SIGPIPE and
  # fail the whole pipeline on a perfectly good bundle.
  csinfo="$(codesign -dv --verbose=2 "${app}" 2>&1 || true)"
  case "${csinfo}" in
    *'Signature=adhoc'*)
      echo "${label}: app is ad-hoc signed, the Developer ID step never applied to it" >&2
      exit 1 ;;
  esac
  case "${csinfo}" in
    *'Authority=Developer ID Application'*) ;;
    *)
      echo "${label}: no Developer ID signature chain" >&2
      exit 1 ;;
  esac
  # engine.rs resolves tools from either <Resources>/resources/bin or a flat
  # <Resources>/bin, and each route uses one of them: the CI build ships the
  # glob from bundle.resources (which keeps the resources/ prefix), while
  # scripts/notarize.sh copies the tools into the flat layout itself. Requiring
  # only the flat one failed a bundle that the app opens happily.
  local engines=""
  local candidate
  for candidate in "${app}/Contents/Resources/resources/bin" "${app}/Contents/Resources/bin"; do
    if [ -n "$(ls -A "${candidate}" 2>/dev/null)" ]; then
      engines="${candidate}"
      break
    fi
  done
  if [ -z "${engines}" ]; then
    echo "${label}: no compression engines under Resources/bin or Resources/resources/bin" >&2
    exit 1
  fi
  if [ -z "$(ls -A "${app}/Contents/Resources/video/bin" 2>/dev/null)" ]; then
    echo "${label}: Resources/video/bin is empty — the video engine did not make it in" >&2
    exit 1
  fi
  if ! xcrun stapler validate "${app}" >/dev/null 2>&1; then
    echo "${label}: no notarization ticket stapled to the app" >&2
    exit 1
  fi
  echo "    ${label}: Developer ID + engines + ticket"
}

verify_architectures() {
  local app="$1"
  shift
  local file_path
  local count=0

  while IFS= read -r -d '' file_path; do
    if file "${file_path}" | grep -q 'Mach-O'; then
      lipo "${file_path}" -verify_arch "$@"
      count=$((count + 1))
    fi
  done < <(find "${app}/Contents" -type f -print0)

  [ "${count}" -gt 0 ] || { echo "No Mach-O files found in ${app}" >&2; exit 1; }
  echo "    verified ${count} Mach-O files in $(dirname "${app}")"
}

verify_universal_architectures() {
  local app="$1"
  local file_path
  local archs
  local count=0

  while IFS= read -r -d '' file_path; do
    if file "${file_path}" | grep -q 'Mach-O'; then
      archs="$(lipo -archs "${file_path}")"
      case " ${archs} " in
        *" arm64 "*|*" x86_64 "*) ;;
        *)
          echo "Unexpected architecture(s) in ${file_path}: ${archs}" >&2
          exit 1
          ;;
      esac
      count=$((count + 1))
    fi
  done < <(find "${app}/Contents" -type f -print0)

  [ "${count}" -gt 0 ] || { echo "No Mach-O files found in ${app}" >&2; exit 1; }
  echo "    verified ${count} Mach-O files in $(dirname "${app}")"
}

sign_app() {
  local app="$1"
  local file_path
  local count=0
  local sign_options=(
    --force
    --options runtime
    --timestamp
    --entitlements "${ENTITLEMENTS}"
    --sign "${SIGNING_IDENTITY}"
  )

  while IFS= read -r -d '' file_path; do
    if file "${file_path}" | grep -q 'Mach-O'; then
      codesign "${sign_options[@]}" "${file_path}"
      count=$((count + 1))
    fi
  done < <(find "${app}/Contents" -type f -print0)

  codesign "${sign_options[@]}" "${app}"
  codesign --verify --deep --strict --verbose=2 "${app}"
  codesign -dv --verbose=2 "${app}" 2>&1 | grep -E 'Authority=|Timestamp='
  echo "    signed ${count} Mach-O files in $(dirname "${app}")"
}

create_dmg() {
  local app="$1"
  local arch="$2"
  local dmg="${OUTPUT_DIR}/Sizqo_${RELEASE_TAG}_macos_${arch}.dmg"
  local staging
  staging="$(mktemp -d -t "sizqo-${arch}")"

  ditto "${app}" "${staging}/Sizqo.app"
  ln -s /Applications "${staging}/Applications"
  hdiutil create -volname Sizqo -srcfolder "${staging}" -ov -format UDZO "${dmg}" >/dev/null
  codesign --force --timestamp --sign "${SIGNING_IDENTITY}" "${dmg}"
  rm -r "${staging}"
  echo "${dmg}"
}

notarize_dmg() {
  local dmg="$1"
  xcrun notarytool submit "${dmg}" "${NOTARY_ARGS[@]}" --wait
}

# The extracted .p8 exists only for this job's lifetime.
cleanup_notary_key() {
  if [ -n "${NOTARY_KEY_FILE:-}" ] && [ -e "${NOTARY_KEY_FILE}" ]; then
    rm -f "${NOTARY_KEY_FILE}"
  fi
}
trap cleanup_notary_key EXIT

log "Verify app architectures"
verify_architectures "${ARM_APP}" arm64
verify_architectures "${INTEL_APP}" x86_64
verify_universal_architectures "${UNIVERSAL_APP}"

log "Developer ID sign apps"
sign_app "${ARM_APP}"
sign_app "${INTEL_APP}"
sign_app "${UNIVERSAL_APP}"

# The apps are notarized and stapled FIRST. An image or an archive built from an
# app that has no ticket can never gain one afterwards, and "the dmg validates"
# says nothing about the copy sealed inside it — that is how v0.2.0 shipped.
notarize_app() {
  local app="$1"
  local zip
  zip="$(mktemp "${TMPDIR:-/tmp}/sizqo-app-notary.XXXXXX").zip"
  rm -f "${zip%.zip}"
  ditto -c -k --keepParent "${app}" "${zip}"
  xcrun notarytool submit "${zip}" "${NOTARY_ARGS[@]}" --wait
  rm -f "${zip}"
}

log "Notarize the three apps"
notarize_app "${ARM_APP}" &
arm_app_pid=$!
notarize_app "${INTEL_APP}" &
intel_app_pid=$!
notarize_app "${UNIVERSAL_APP}" &
universal_app_pid=$!

set +e
wait "${arm_app_pid}"
arm_app_status=$?
wait "${intel_app_pid}"
intel_app_status=$?
wait "${universal_app_pid}"
universal_app_status=$?
set -e

if [ "${arm_app_status}" -ne 0 ] || [ "${intel_app_status}" -ne 0 ] || [ "${universal_app_status}" -ne 0 ]; then
  echo "App notarization failed: arm64=${arm_app_status}, x86_64=${intel_app_status}, universal=${universal_app_status}" >&2
  exit 1
fi

log "Staple the notarization tickets into the apps"
for app in "${ARM_APP}" "${INTEL_APP}" "${UNIVERSAL_APP}"; do
  xcrun stapler staple "${app}"
  xcrun stapler validate "${app}"
done

log "Create signed DMGs from the ticketed apps"
ARM_DMG="$(create_dmg "${ARM_APP}" arm64)"
INTEL_DMG="$(create_dmg "${INTEL_APP}" x86_64)"
UNIVERSAL_DMG="$(create_dmg "${UNIVERSAL_APP}" universal)"

log "Submit three DMGs to Apple notarization"
notarize_dmg "${ARM_DMG}" &
arm_pid=$!
notarize_dmg "${INTEL_DMG}" &
intel_pid=$!
notarize_dmg "${UNIVERSAL_DMG}" &
universal_pid=$!

set +e
wait "${arm_pid}"
arm_status=$?
wait "${intel_pid}"
intel_status=$?
wait "${universal_pid}"
universal_status=$?
set -e

if [ "${arm_status}" -ne 0 ] || [ "${intel_status}" -ne 0 ] || [ "${universal_status}" -ne 0 ]; then
  echo "Notarization failed: arm64=${arm_status}, x86_64=${intel_status}, universal=${universal_status}" >&2
  exit 1
fi

log "Staple and validate the DMG tickets"
for dmg in "${ARM_DMG}" "${INTEL_DMG}" "${UNIVERSAL_DMG}"; do
  xcrun stapler staple "${dmg}"
  xcrun stapler validate "${dmg}"
done

log "Mount each DMG and verify the app inside it"
for pair in "arm64:${ARM_DMG}" "x86_64:${INTEL_DMG}" "universal:${UNIVERSAL_DMG}"; do
  arch="${pair%%:*}"
  dmg="${pair#*:}"
  mnt="$(mktemp -d "${TMPDIR:-/tmp}/sizqo-verify-${arch}.XXXXXX")"
  hdiutil attach -readonly -nobrowse -mountpoint "${mnt}" "${dmg}" >/dev/null
  assert_distributable "${mnt}/Sizqo.app" "DMG ${arch}"
  spctl -a -vvv -t exec "${mnt}/Sizqo.app" || true
  hdiutil detach "${mnt}" -quiet >/dev/null 2>&1 || true
  rm -rf "${mnt}"
done

log "Create the updater archive from the ticketed Universal app"
UPDATER_ARCHIVE="${OUTPUT_DIR}/Sizqo_${RELEASE_TAG}_macos_universal.app.tar.gz"
tar -czf "${UPDATER_ARCHIVE}" \
  -C "$(dirname "${UNIVERSAL_APP}")" "$(basename "${UNIVERSAL_APP}")"

# The signature step downstream signs these bytes, so the archive is checked here
# rather than after it has been published.
upcheck="$(mktemp -d "${TMPDIR:-/tmp}/sizqo-verify-updater.XXXXXX")"
tar -xzf "${UPDATER_ARCHIVE}" -C "${upcheck}"
assert_distributable "${upcheck}/Sizqo.app" "updater archive"
rm -rf "${upcheck}"

log "macOS release assets are signed, notarized, and stapled"
ls -lh "${OUTPUT_DIR}"
