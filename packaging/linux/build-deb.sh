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
  "$DEST/usr/lib/hecate-lampad-desktop" \
  "$DEST/usr/lib/systemd/user" \
  "$DEST/lib/systemd/system" \
  "$DEST/etc/xdg/autostart"

install -m 0755 "$BINARY" "$DEST/usr/bin/hecate-lampad-desktop"
install -m 0755 "$ROOT/packaging/linux/scripts/run-helper.sh" \
  "$DEST/usr/lib/hecate-lampad-desktop/run-helper.sh"
install -m 0755 "$ROOT/packaging/linux/scripts/activate-for-sessions.sh" \
  "$DEST/usr/lib/hecate-lampad-desktop/activate-for-sessions.sh"
install -m 0755 "$ROOT/packaging/linux/scripts/fix-ipc-perms.sh" \
  "$DEST/usr/lib/hecate-lampad-desktop/fix-ipc-perms.sh"
install -m 0644 "$ROOT/packaging/linux/systemd/user/hecate-lampad-desktop.service" \
  "$DEST/usr/lib/systemd/user/hecate-lampad-desktop.service"
install -m 0644 "$ROOT/packaging/linux/systemd/hecate-lampad-desktop-ipc-fix.path" \
  "$DEST/lib/systemd/system/hecate-lampad-desktop-ipc-fix.path"
install -m 0644 "$ROOT/packaging/linux/systemd/hecate-lampad-desktop-ipc-fix.service" \
  "$DEST/lib/systemd/system/hecate-lampad-desktop-ipc-fix.service"
install -m 0644 "$ROOT/packaging/linux/autostart/hecate-lampad-desktop.desktop" \
  "$DEST/etc/xdg/autostart/hecate-lampad-desktop.desktop"

# Package names (not SONAMEs): libxfixes3 ships libXfixes.so.6; libxdo3 ships libxdo.so.3.
# Do not use SONAME-derived names like libxfixes6 — they are not installable and break apt
# coexistence with other packages (e.g. qemu-guest-agent).
# Note: modern Ubuntu `login` no longer ships `sg`; IPC group is repaired by the
# system path unit + agent-side chgrp instead.
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
# Shared agent/helper config dir. Never force root:root — that makes mode 0750
# unreadable to User=hecate-lampad (systemd) and the agent reports "config not found".
if [ ! -d /etc/hecate-lampad ]; then
  if id hecate-lampad >/dev/null 2>&1 && getent group hecate-ipc >/dev/null 2>&1; then
    install -d -m 0750 -o hecate-lampad -g hecate-ipc /etc/hecate-lampad
  else
    install -d -m 0755 /etc/hecate-lampad
  fi
elif id hecate-lampad >/dev/null 2>&1 && getent group hecate-ipc >/dev/null 2>&1; then
  chown hecate-lampad:hecate-ipc /etc/hecate-lampad 2>/dev/null || true
  chmod 0750 /etc/hecate-lampad 2>/dev/null || true
fi
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
  # Root path watcher: chgrp sock/token whenever the helper recreates them.
  systemctl reset-failed hecate-lampad-desktop-ipc-fix.path \
    hecate-lampad-desktop-ipc-fix.service >/dev/null 2>&1 || true
  systemctl enable --now hecate-lampad-desktop-ipc-fix.path >/dev/null 2>&1 || true
  if [ -x /usr/lib/hecate-lampad-desktop/fix-ipc-perms.sh ]; then
    /usr/lib/hecate-lampad-desktop/fix-ipc-perms.sh || true
  fi
fi
# Activate like macOS/Windows packaging: group membership + start in live GUI
# sessions. Script always exits 0 so a missing session does not fail dpkg.
if [ -x /usr/lib/hecate-lampad-desktop/activate-for-sessions.sh ]; then
  /usr/lib/hecate-lampad-desktop/activate-for-sessions.sh || true
fi
EOF
chmod 0755 "$DEST/DEBIAN/postinst"

cat >"$DEST/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = remove ] || [ "$1" = upgrade ] || [ "$1" = deconfigure ]; then
  if command -v systemctl >/dev/null 2>&1; then
    systemctl disable --now hecate-lampad-desktop-ipc-fix.path >/dev/null 2>&1 || true
  fi
fi
EOF
chmod 0755 "$DEST/DEBIAN/prerm"

mkdir -p "$OUTDIR"
dpkg-deb --build "$DEST" "$OUTDIR/${PKG}.deb"
echo "built $OUTDIR/${PKG}.deb"
