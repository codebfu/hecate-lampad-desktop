#!/usr/bin/env bash
# Build hecate-lampad-desktop macOS .pkg on a native macOS runner.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
# shellcheck source=packaging/macos/common.sh
source "${ROOT}/packaging/macos/common.sh"

export PATH="${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"

VERSION="${VERSION:-$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')}"
DIST="${ROOT}/dist"
ARCH="$(macos_package_arch)"
STAGING="${DIST}/macos-desktop-pkgroot-${ARCH}"
SCRIPTS="${ROOT}/packaging/macos/scripts"

cargo build --release

rm -f "${DIST}"/hecate-lampad-desktop_*.pkg "${DIST}"/hecate-lampad-desktop_*.pkg.sha256
rm -rf "${STAGING}"
mkdir -p \
  "${STAGING}/usr/local/bin" \
  "${STAGING}/Library/LaunchAgents"

install -m 755 target/release/hecate-lampad-desktop \
  "${STAGING}/usr/local/bin/hecate-lampad-desktop"
install -m 644 packaging/macos/com.hecate.lampad-desktop.plist \
  "${STAGING}/Library/LaunchAgents/"

OUTPUT="${DIST}/hecate-lampad-desktop_${VERSION}_macos-${ARCH}.pkg"
create_pkg_from_root "${STAGING}" "${SCRIPTS}" "com.hecate.lampad-desktop" "${VERSION}" "${OUTPUT}"
sha256_file "${OUTPUT}" > "${OUTPUT}.sha256"
echo "Built ${OUTPUT}"
