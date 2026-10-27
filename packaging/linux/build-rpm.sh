#!/usr/bin/env bash
# Copyright (C) 2026 Gaultier HUBERT
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Minimal rpm packaging for hecate-lampad-desktop (Fedora / EL9+).
# Note: on RHEL/Rocky/Alma, libxdo may require EPEL.
# Usage: ./build-rpm.sh <version> <arch> <binary-path> <outdir>
set -euo pipefail

VERSION="${1:?version}"
ARCH="${2:?arch}"
BINARY="${3:?binary}"
OUTDIR="${4:?outdir}"
RELEASE="${RPM_RELEASE:-1}"

# Map Debian-style arch names to RPM.
case "${ARCH}" in
  amd64) ARCH="x86_64" ;;
  arm64|aarch64) ARCH="aarch64" ;;
esac

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if ! command -v rpmbuild >/dev/null 2>&1; then
  echo "rpmbuild not found; install the rpm package" >&2
  exit 1
fi

if [[ ! -x "${BINARY}" ]]; then
  echo "binary not found or not executable: ${BINARY}" >&2
  exit 1
fi

RPMBUILD="${WORK}/rpmbuild"
mkdir -p \
  "${RPMBUILD}"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS} \
  "${WORK}/payload/usr/bin" \
  "${WORK}/payload/usr/lib/hecate-lampad-desktop" \
  "${WORK}/payload/usr/lib/systemd/user" \
  "${WORK}/payload/usr/lib/systemd/system" \
  "${WORK}/payload/etc/xdg/autostart"

install -m 0755 "$BINARY" "${WORK}/payload/usr/bin/hecate-lampad-desktop"
install -m 0755 "$ROOT/packaging/linux/scripts/run-helper.sh" \
  "${WORK}/payload/usr/lib/hecate-lampad-desktop/run-helper.sh"
install -m 0755 "$ROOT/packaging/linux/scripts/activate-for-sessions.sh" \
  "${WORK}/payload/usr/lib/hecate-lampad-desktop/activate-for-sessions.sh"
install -m 0755 "$ROOT/packaging/linux/scripts/fix-ipc-perms.sh" \
  "${WORK}/payload/usr/lib/hecate-lampad-desktop/fix-ipc-perms.sh"
install -m 0644 "$ROOT/packaging/linux/systemd/user/hecate-lampad-desktop.service" \
  "${WORK}/payload/usr/lib/systemd/user/hecate-lampad-desktop.service"
install -m 0644 "$ROOT/packaging/linux/systemd/hecate-lampad-desktop-ipc-fix.path" \
  "${WORK}/payload/usr/lib/systemd/system/hecate-lampad-desktop-ipc-fix.path"
install -m 0644 "$ROOT/packaging/linux/systemd/hecate-lampad-desktop-ipc-fix.service" \
  "${WORK}/payload/usr/lib/systemd/system/hecate-lampad-desktop-ipc-fix.service"
install -m 0644 "$ROOT/packaging/linux/autostart/hecate-lampad-desktop.desktop" \
  "${WORK}/payload/etc/xdg/autostart/hecate-lampad-desktop.desktop"

tar -C "${WORK}/payload" -czf "${RPMBUILD}/SOURCES/hecate-lampad-desktop-${VERSION}.tar.gz" .

cat >"${RPMBUILD}/SPECS/hecate-lampad-desktop.spec" <<EOF
Name:           hecate-lampad-desktop
Version:        ${VERSION}
Release:        ${RELEASE}%{?dist}
Summary:        Hecate lampad desktop helper
License:        GPL-3.0-or-later
URL:            https://github.com/codebfu/hecate-lampad-desktop
Source0:        hecate-lampad-desktop-%{version}.tar.gz
BuildArch:      ${ARCH}
# On EL, libxdo typically comes from EPEL.
Requires:       libX11
Requires:       libXfixes
Requires:       libxdo

%description
Provides screenshot, keyboard/mouse, clipboard, and session IPC for hecate-lampad.

%prep
%setup -c -n hecate-lampad-desktop-%{version}

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}
cp -a . %{buildroot}/

%files
%attr(755,root,root) /usr/bin/hecate-lampad-desktop
%attr(755,root,root) /usr/lib/hecate-lampad-desktop/run-helper.sh
%attr(755,root,root) /usr/lib/hecate-lampad-desktop/activate-for-sessions.sh
%attr(755,root,root) /usr/lib/hecate-lampad-desktop/fix-ipc-perms.sh
%attr(644,root,root) /usr/lib/systemd/user/hecate-lampad-desktop.service
%attr(644,root,root) /usr/lib/systemd/system/hecate-lampad-desktop-ipc-fix.path
%attr(644,root,root) /usr/lib/systemd/system/hecate-lampad-desktop-ipc-fix.service
%attr(644,root,root) /etc/xdg/autostart/hecate-lampad-desktop.desktop

%post
if ! getent group hecate-ipc >/dev/null 2>&1; then
  groupadd --system hecate-ipc 2>/dev/null || true
fi
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
  systemctl stop hecate-lampad-desktop-ipc-fix.service >/dev/null 2>&1 || true
  systemctl reset-failed hecate-lampad-desktop-ipc-fix.path \\
    hecate-lampad-desktop-ipc-fix.service >/dev/null 2>&1 || true
  systemctl enable --now hecate-lampad-desktop-ipc-fix.path >/dev/null 2>&1 || true
  if [ -x /usr/lib/hecate-lampad-desktop/fix-ipc-perms.sh ]; then
    /usr/lib/hecate-lampad-desktop/fix-ipc-perms.sh || true
  fi
fi
if [ -x /usr/lib/hecate-lampad-desktop/activate-for-sessions.sh ]; then
  /usr/lib/hecate-lampad-desktop/activate-for-sessions.sh || true
fi

%preun
if [ "\$1" -eq 0 ]; then
  if command -v systemctl >/dev/null 2>&1; then
    systemctl disable --now hecate-lampad-desktop-ipc-fix.path >/dev/null 2>&1 || true
  fi
fi

%postun
systemctl daemon-reload >/dev/null 2>&1 || true

%changelog
* $(date '+%a %b %d %Y') Hecate Contributors <hecate@localhost> - ${VERSION}-${RELEASE}
- Package hecate-lampad-desktop ${VERSION}
EOF

rpmbuild \
  --define "_topdir ${RPMBUILD}" \
  --define "_build_id_links none" \
  -bb "${RPMBUILD}/SPECS/hecate-lampad-desktop.spec"

BUILT="$(find "${RPMBUILD}/RPMS" -name 'hecate-lampad-desktop-*.rpm' | head -n 1)"
if [[ -z "${BUILT}" ]]; then
  echo "rpmbuild produced no package" >&2
  exit 1
fi

mkdir -p "$OUTDIR"
OUTPUT="${OUTDIR}/hecate-lampad-desktop-${VERSION}-${RELEASE}.${ARCH}.rpm"
cp -f "${BUILT}" "${OUTPUT}"
echo "built ${OUTPUT}"
