#!/bin/sh
# Copyright (C) 2026 Gaultier HUBERT
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Launch the desktop helper with the hecate-ipc supplementary group when the
# calling user is a member in /etc/group. `sg` consults the group database, so
# this works immediately after `usermod -a -G hecate-ipc` without a re-login
# (unlike a plain systemd --user start, which keeps the old login token).

set -eu

HELPER="/usr/bin/hecate-lampad-desktop"

user="$(id -un 2>/dev/null || true)"
if [ -n "${user}" ] \
  && command -v sg >/dev/null 2>&1 \
  && getent group hecate-ipc >/dev/null 2>&1 \
  && getent group hecate-ipc | grep -E "(^hecate-ipc:|:|,)${user}(,|$)" >/dev/null 2>&1; then
  exec sg hecate-ipc -c "exec \"${HELPER}\" run"
fi

exec "${HELPER}" run
