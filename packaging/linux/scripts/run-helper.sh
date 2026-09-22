#!/bin/sh
# Copyright (C) 2026 Gaultier HUBERT
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Launch the desktop helper with the hecate-ipc group active. Prefer `sg` so
# membership added via usermod works without a re-login. Fall back to a plain
# exec if sg is unavailable or the user is not a hecate-ipc member.

set -eu

HELPER="/usr/bin/hecate-lampad-desktop"

if command -v sg >/dev/null 2>&1 && getent group hecate-ipc >/dev/null 2>&1; then
  # `sg -c true` is the authoritative membership check (same path sg uses).
  if sg hecate-ipc -c true >/dev/null 2>&1; then
    exec sg hecate-ipc -c "exec \"${HELPER}\" run"
  fi
fi

exec "${HELPER}" run
