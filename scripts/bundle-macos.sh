#!/bin/bash
set -euo pipefail

# Builds the Rust application, wraps it in a macOS app bundle, and produces a
# distributable DMG. The script deliberately owns the whole release pipeline
# so local builds and CI produce identical artifacts.

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="Averroes"
BUNDLE_ID="com.valendra.averroes"
MINIMUM_MACOS_VERSION="13.0"
TARGET_DIR="${CARGO_TARGET_DIR:-${PROJECT_ROOT}/target}"
DIST_DIR="${DIST_DIR:-${PROJECT_ROOT}/dist}"
ARCH="$(uname -m)"

if [[ "${TARGET_DIR}" != /* ]]; then
    TARGET_DIR="${PROJECT_ROOT}/${TARGET_DIR}"
fi
if [[ "${DIST_DIR}" != /* ]]; then
    DIST_DIR="${PROJECT_ROOT}/${DIST_DIR}"
fi

workspace_version() {
    sed -nE '/^\[workspace\.package\]$/,/^\[/{s/^version = "([^"]+)"/\1/p;}' \
        "${PROJECT_ROOT}/Cargo.toml" \
        | head -n 1
}

VERSION="${VERSION:-$(workspace_version)}"
VERSION="${VERSION#v}"

if [[ -z "${VERSION}" ]]; then
    echo "Could not determine the workspace version. Set VERSION explicitly." >&2
    exit 1
fi

SEMVER_REGEX='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-((0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)(\.(0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*))?(\+([0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*))?$'
if [[ ! "${VERSION}" =~ ${SEMVER_REGEX} ]]; then
    echo "Invalid release version '${VERSION}'. Expected SemVer like 1.2.3." >&2
    exit 1
fi

BINARY="${TARGET_DIR}/release/averroes-gpui"
BUNDLE="${TARGET_DIR}/release/${APP_NAME}.app"
DMG="${DIST_DIR}/${APP_NAME}-${VERSION}-macos-${ARCH}.dmg"
ICON="${PROJECT_ROOT}/assets/AppIcon.icns"

ensure_safe_artifact_path() {
    local artifact="$1"
    case "${artifact}" in
        "${TARGET_DIR}"/*|"${DIST_DIR}"/*) ;;
        *)
            echo "Refusing to overwrite an artifact outside target or dist: ${artifact}" >&2
            exit 1
            ;;
    esac
}

require_tool() {
    command -v "$1" >/dev/null || {
        echo "Required tool is unavailable: $1" >&2
        exit 1
    }
}

for tool in cargo hdiutil plutil ditto; do
    require_tool "${tool}"
done

ensure_safe_artifact_path "${BUNDLE}"
ensure_safe_artifact_path "${DMG}"

mkdir -p "${TARGET_DIR}/release" "${DIST_DIR}"

echo "Building ${APP_NAME} ${VERSION} for ${ARCH}..."
(
    cd "${PROJECT_ROOT}"
    AVERROES_VERSION="${VERSION}" cargo build --release --locked -p averroes-gpui
)

if [[ ! -x "${BINARY}" ]]; then
    echo "Release binary was not created: ${BINARY}" >&2
    exit 1
fi

echo "Creating ${BUNDLE}..."
rm -rf "${BUNDLE}"
mkdir -p "${BUNDLE}/Contents/MacOS" "${BUNDLE}/Contents/Resources"
ditto "${BINARY}" "${BUNDLE}/Contents/MacOS/${APP_NAME}"

if [[ -f "${ICON}" ]]; then
    ditto "${ICON}" "${BUNDLE}/Contents/Resources/AppIcon.icns"
else
    echo "Warning: app icon not found at ${ICON}" >&2
fi

cat > "${BUNDLE}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleExecutable</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon.icns</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>${MINIMUM_MACOS_VERSION}</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

plutil -lint "${BUNDLE}/Contents/Info.plist"

if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
    require_tool codesign
    echo "Signing app bundle with ${CODESIGN_IDENTITY}..."
    codesign --force --deep --options runtime --sign "${CODESIGN_IDENTITY}" "${BUNDLE}"
    codesign --verify --deep --strict "${BUNDLE}"
else
    echo "Skipping code signing (set CODESIGN_IDENTITY to sign the app)."
fi

echo "Creating ${DMG}..."
rm -f "${DMG}"
DMG_STAGE="$(mktemp -d "${TARGET_DIR}/averroes-dmg.XXXXXX")"
cleanup_dmg_stage() {
    rm -rf "${DMG_STAGE}"
}
trap cleanup_dmg_stage EXIT

# Keep the installer convention users expect: drag the app onto the bundled
# Applications alias rather than locating /Applications manually.
ditto "${BUNDLE}" "${DMG_STAGE}/${APP_NAME}.app"
ln -s /Applications "${DMG_STAGE}/Applications"
hdiutil create \
    -volname "${APP_NAME}" \
    -srcfolder "${DMG_STAGE}" \
    -ov \
    -format UDZO \
    "${DMG}"

if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
    codesign --force --sign "${CODESIGN_IDENTITY}" "${DMG}"
fi

echo "App bundle: ${BUNDLE}"
echo "DMG: ${DMG}"

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    {
        echo "app_bundle=${BUNDLE}"
        echo "dmg=${DMG}"
    } >> "${GITHUB_OUTPUT}"
fi
