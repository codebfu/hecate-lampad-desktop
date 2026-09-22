#!/bin/sh
# Copyright (C) 2026 Gaultier HUBERT
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Ensure desktop.sock / ipc.token are group hecate-ipc so the agent service
# (User=hecate-lampad Group=hecate-ipc) can connect. The helper often cannot
# chgrp itself when the GUI session token lacks the supplementary group and
# `sg` is unavailable.

set -eu

RUNTIME="/run/hecate-lampad"
SOCK="${RUNTIME}/desktop.sock"
TOKEN="${RUNTIME}/ipc.token"

getent group hecate-ipc >/dev/null 2>&1 || exit 0

if [ -S "${SOCK}" ]; then
  chgrp hecate-ipc "${SOCK}" 2>/dev/null || true
  chmod 0660 "${SOCK}" 2>/dev/null || true
fi
if [ -f "${TOKEN}" ]; then
  chgrp hecate-ipc "${TOKEN}" 2>/dev/null || true
  chmod 0640 "${TOKEN}" 2>/dev/null || true
fi

exit 0
