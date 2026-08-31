#!/usr/bin/env bash
# Copyright (C) 2026 Gaultier HUBERT
# SPDX-License-Identifier: GPL-3.0-or-later

set -euo pipefail

# Minimal deb packaging for hecate-lampad-desktop.
# Usage: ./build-deb.sh <version> <arch> <binary-path> <outdir>

VERSION="${1:?version}"
ARCH="${2:?arch}"
BINARY="${3:?binary}"
OUTDIR="${4:?outdir}"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

PKG="hecate-lampad-desktop_${VERSION}_${ARCH}"
DEST="$STAGE/$PKG"

mkdir -p \
  "$DEST/DEBIAN" \
  "$DEST/usr/bin" \
  "$DEST/usr/lib/systemd/user" \
  "$DEST/etc/xdg/autostart"

install -m 0755 "$BINARY" "$DEST/usr/bin/hecate-lampad-desktop"
install -m 0644 "$ROOT/packaging/linux/systemd/user/hecate-lampad-desktop.service" \
  "$DEST/usr/lib/systemd/user/hecate-lampad-desktop.service"
install -m 0644 "$ROOT/packaging/linux/autostart/hecate-lampad-desktop.desktop" \
  "$DEST/etc/xdg/autostart/hecate-lampad-desktop.desktop"

# Package names (not SONAMEs): libxfixes3 ships libXfixes.so.6; libxdo3 ships libxdo.so.3.
# Do not use SONAME-derived names like libxfix6 — they are not installable and break apt
# coexistence with other packages (e.g. qemu-guest-agent).
cat >"$DEST/DEBIAN/control" <<EOF
Package: hecate-lampad-desktop
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Maintainer: Hecate Contributors
Depends: libx11-6, libxfixes3, libxdo3
Recommends: hecate-lampad
Enhances: hecate-lampad
Description: Hecate lampad desktop helper (user-session GUI control)
 Provides screenshot, keyboard/mouse, clipboard, and session IPC for hecate-lampad.
EOF

cat >"$DEST/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if ! getent group hecate-ipc >/dev/null 2>&1; then
  groupadd --system hecate-ipc 2>/dev/null || true
fi
install -d -m 0750 -o root -g root /etc/hecate-lampad 2>/dev/null || true
if [ ! -f /etc/hecate-lampad/desktop-helper.toml ]; then
  cat >/etc/hecate-lampad/desktop-helper.toml <<'POL'
# Root-owned helper policy. Dangerous env keys and elevation wrappers are
# always blocked. Tighten allowlists to match the agent identity policy.
allowed_binaries = ["*"]
allowed_cwd = ["*"]
allowed_env = ["*"]
POL
  chmod 0644 /etc/hecate-lampad/desktop-helper.toml
fi
if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi
cat <<'MSG'
Enable in each graphical user session (after login):
  systemctl --user enable --now hecate-lampad-desktop

Add each GUI user to the hecate-ipc group so the helper can bind desktop.sock:
  usermod -a -G hecate-ipc <username>

Requires hecate-lampad agent (creates /run/hecate-lampad for desktop.sock).
MSG
EOF
chmod 0755 "$DEST/DEBIAN/postinst"

mkdir -p "$OUTDIR"
dpkg-deb --build "$DEST" "$OUTDIR/${PKG}.deb"
echo "built $OUTDIR/${PKG}.deb"
