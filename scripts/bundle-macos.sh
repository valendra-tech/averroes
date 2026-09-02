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

normalize_architecture() {
    case "$1" in
        x86_64|amd64) printf '%s\n' "x86_64" ;;
        arm64|aarch64) printf '%s\n' "arm64" ;;
        *) return 1 ;;
    esac
}

host_architecture="$(uname -m)"
if ! ARCH="$(normalize_architecture "${host_architecture}")"; then
    echo "Unsupported host architecture from uname -m: ${host_architecture}" >&2
    exit 1
fi

if [[ -n "${EXPECTED_ARCH+x}" ]]; then
    if [[ -z "${EXPECTED_ARCH}" ]]; then
        echo "EXPECTED_ARCH must not be empty when set." >&2
        exit 1
    fi

    expected_architecture="${EXPECTED_ARCH}"
    if ! EXPECTED_ARCH="$(normalize_architecture "${expected_architecture}")"; then
        echo "Unsupported expected architecture: ${expected_architecture}" >&2
        exit 1
    fi
elif [[ -n "${CI:-}" || -n "${GITHUB_ACTIONS:-}" ]]; then
    echo "EXPECTED_ARCH is required in CI." >&2
    exit 1
else
    EXPECTED_ARCH="${ARCH}"
fi

if [[ "${ARCH}" != "${EXPECTED_ARCH}" ]]; then
    echo "Architecture mismatch: expected ${EXPECTED_ARCH}, got ${ARCH}." >&2
    exit 1
fi

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

for tool in cargo hdiutil plutil ditto vtool lipo; do
    require_tool "${tool}"
done

ensure_safe_artifact_path "${BUNDLE}"
ensure_safe_artifact_path "${DMG}"

mkdir -p "${TARGET_DIR}/release" "${DIST_DIR}"

echo "Building ${APP_NAME} ${VERSION} for ${ARCH}..."
export MACOSX_DEPLOYMENT_TARGET="${MINIMUM_MACOS_VERSION}"
(
    cd "${PROJECT_ROOT}"
    AVERROES_VERSION="${VERSION}" cargo build --release --locked -p averroes-gpui
)

if [[ ! -x "${BINARY}" ]]; then
    echo "Release binary was not created: ${BINARY}" >&2
    exit 1
fi

if ! vtool_build_info="$(vtool -show-build "${BINARY}")"; then
    echo "Could not inspect the release binary with vtool: ${BINARY}" >&2
    exit 1
fi

if ! awk -v minimum_macos_version="${MINIMUM_MACOS_VERSION}" '
    /^[[:space:]]*Load command[[:space:]]/ { in_build_version = 0 }
    $1 == "cmd" && $2 == "LC_BUILD_VERSION" {
        in_build_version = 1
        macos_platform = 0
        next
    }
    in_build_version && $1 == "platform" && $2 == "MACOS" { macos_platform = 1 }
    in_build_version && macos_platform && $1 == "minos" && $2 == minimum_macos_version { valid = 1 }
    END { exit valid ? 0 : 1 }
' <<< "${vtool_build_info}"; then
    echo "Release binary must target macOS ${MINIMUM_MACOS_VERSION}: ${BINARY}" >&2
    exit 1
fi

if ! binary_architectures="$(lipo -archs "${BINARY}")"; then
    echo "Could not inspect the release binary with lipo: ${BINARY}" >&2
    exit 1
fi

if [[ ! "${binary_architectures}" =~ ^[[:space:]]*${EXPECTED_ARCH}[[:space:]]*$ ]]; then
    echo "Release binary architecture mismatch: expected only ${EXPECTED_ARCH}, got ${binary_architectures}." >&2
    exit 1
fi

VECTOR_LIBRARY="$(find "${TARGET_DIR}/release/deps" -maxdepth 1 -type f -name 'libsqlite_vector_rs-*.dylib' -print -quit)"
if [[ -z "${VECTOR_LIBRARY}" ]]; then
    echo "sqlite-vector-rs library was not created under ${TARGET_DIR}/release/deps" >&2
    exit 1
fi

echo "Creating ${BUNDLE}..."
rm -rf "${BUNDLE}"
mkdir -p "${BUNDLE}/Contents/MacOS" "${BUNDLE}/Contents/Resources"
ditto "${BINARY}" "${BUNDLE}/Contents/MacOS/${APP_NAME}"
# The vector extension is a loadable module, so it must travel with the app.
# Keep a stable filename beside the executable; the runtime supplies the
# explicit sqlite3 entrypoint and therefore does not depend on Cargo's hash.
ditto "${VECTOR_LIBRARY}" "${BUNDLE}/Contents/MacOS/libsqlite_vector_rs.dylib"

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
    codesign_keychain_args=()
    if [[ -n "${CODESIGN_KEYCHAIN:-}" ]]; then
        codesign_keychain_args=(--keychain "${CODESIGN_KEYCHAIN}")
    fi

    # Loadable libraries are independent Mach-O code objects. Signing only the
    # outer app leaves Cargo's ad-hoc dylib signature in place, which passes a
    # deep local verification but is rejected by Apple's notarization service.
    echo "Signing embedded libraries with ${CODESIGN_IDENTITY}..."
    codesign --force --options runtime --timestamp --sign "${CODESIGN_IDENTITY}" "${codesign_keychain_args[@]+"${codesign_keychain_args[@]}"}" "${BUNDLE}/Contents/MacOS/libsqlite_vector_rs.dylib"
    codesign --verify --strict --verbose=2 "${BUNDLE}/Contents/MacOS/libsqlite_vector_rs.dylib"

    echo "Signing app bundle with ${CODESIGN_IDENTITY}..."
    codesign --force --options runtime --timestamp --sign "${CODESIGN_IDENTITY}" "${codesign_keychain_args[@]+"${codesign_keychain_args[@]}"}" "${BUNDLE}"
    codesign --verify --deep --strict "${codesign_keychain_args[@]+"${codesign_keychain_args[@]}"}" "${BUNDLE}"
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
    codesign --force --timestamp --sign "${CODESIGN_IDENTITY}" "${codesign_keychain_args[@]+"${codesign_keychain_args[@]}"}" "${DMG}"
fi

hdiutil verify "${DMG}"

echo "App bundle: ${BUNDLE}"
echo "DMG: ${DMG}"

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    {
        echo "app_bundle=${BUNDLE}"
        echo "dmg=${DMG}"
    } >> "${GITHUB_OUTPUT}"
fi
