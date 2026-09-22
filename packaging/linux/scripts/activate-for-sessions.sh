#!/bin/sh
# Copyright (C) 2026 Gaultier HUBERT
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Called from the .deb postinst (and optionally by the agent after helper.install).
# Mirrors macOS/Windows packaging: grant IPC group membership and start the
# helper in any live GUI session. Never fails the package install.

set -u

UNIT="hecate-lampad-desktop.service"
RUNTIME_PARENT="/run/hecate-lampad"

log() {
  echo "hecate-lampad-desktop: $*" >&2
}

ensure_group() {
  if ! getent group hecate-ipc >/dev/null 2>&1; then
    groupadd --system hecate-ipc 2>/dev/null || true
  fi
}

# Interactive local accounts (UID >= 1000, real login shell).
add_interactive_users_to_ipc() {
  if ! getent group hecate-ipc >/dev/null 2>&1; then
    return 0
  fi
  # passwd: name:passwd:uid:gid:gecos:home:shell
  while IFS=: read -r name _ uid _ _ _ shell; do
    case "${uid}" in
      ''|*[!0-9]*) continue ;;
    esac
    [ "${uid}" -ge 1000 ] || continue
    case "${shell}" in
      */nologin|*/false|*/sync|*/halt|*/shutdown) continue ;;
    esac
    if id -nG "${name}" 2>/dev/null | grep -qw hecate-ipc; then
      continue
    fi
    if usermod -a -G hecate-ipc "${name}" 2>/dev/null; then
      log "added ${name} to hecate-ipc"
    fi
  done </etc/passwd
}

session_is_graphical() {
  sid="$1"
  type=""
  display=""
  desktop=""
  if command -v loginctl >/dev/null 2>&1; then
    type="$(loginctl show-session "${sid}" -p Type --value 2>/dev/null || true)"
    display="$(loginctl show-session "${sid}" -p Display --value 2>/dev/null || true)"
    desktop="$(loginctl show-session "${sid}" -p Desktop --value 2>/dev/null || true)"
  fi
  case "${type}" in
    x11|wayland|mir|web|xrdp|vnc) return 0 ;;
  esac
  [ -n "${display}" ] && return 0
  [ -n "${desktop}" ] && return 0
  return 1
}

enable_and_start_for_user() {
  user="$1"
  uid="$2"
  sid="${3:-}"
  runtime="/run/user/${uid}"
  bus="${runtime}/bus"

  if [ ! -d "${runtime}" ] || [ ! -S "${bus}" ]; then
    log "no user manager for ${user} (uid ${uid}); enable deferred until login"
    return 0
  fi

  # Prefer runuser; fall back to sudo -u for older images.
  run_as_user() {
    if command -v runuser >/dev/null 2>&1; then
      runuser -u "${user}" -- env "$@"
    else
      sudo -u "${user}" env "$@"
    fi
  }

  display=""
  xauthority=""
  if [ -n "${sid}" ] && command -v loginctl >/dev/null 2>&1; then
    display="$(loginctl show-session "${sid}" -p Display --value 2>/dev/null || true)"
  fi
  if [ -z "${display}" ] && [ -S /tmp/.X11-unix/X0 ]; then
    display=":0"
  fi
  for candidate in \
    "${runtime}/gdm/Xauthority" \
    "${runtime}/.mutter-Xauthority" \
    "/home/${user}/.Xauthority"; do
    if [ -f "${candidate}" ]; then
      xauthority="${candidate}"
      break
    fi
  done

  if ! run_as_user \
    "XDG_RUNTIME_DIR=${runtime}" \
    "DBUS_SESSION_BUS_ADDRESS=unix:path=${bus}" \
    systemctl --user enable "${UNIT}" >/dev/null 2>&1; then
    log "warning: systemctl --user enable failed for ${user}"
  fi

  if [ -n "${display}" ]; then
    run_as_user \
      "XDG_RUNTIME_DIR=${runtime}" \
      "DBUS_SESSION_BUS_ADDRESS=unix:path=${bus}" \
      systemctl --user set-environment "DISPLAY=${display}" >/dev/null 2>&1 || true
  fi
  if [ -n "${xauthority}" ]; then
    run_as_user \
      "XDG_RUNTIME_DIR=${runtime}" \
      "DBUS_SESSION_BUS_ADDRESS=unix:path=${bus}" \
      systemctl --user set-environment "XAUTHORITY=${xauthority}" >/dev/null 2>&1 || true
  fi

  if [ -d "${RUNTIME_PARENT}" ]; then
    # Best-effort ACL so bind works even if the session token lacks hecate-ipc.
    if command -v setfacl >/dev/null 2>&1; then
      setfacl -m "u:${uid}:rwx" "${RUNTIME_PARENT}" 2>/dev/null || true
    fi
  else
    log "warning: ${RUNTIME_PARENT} missing (start hecate-lampad agent first)"
  fi

  if run_as_user \
    "XDG_RUNTIME_DIR=${runtime}" \
    "DBUS_SESSION_BUS_ADDRESS=unix:path=${bus}" \
    systemctl --user restart "${UNIT}" >/dev/null 2>&1; then
    log "started ${UNIT} for ${user}"
  elif run_as_user \
    "XDG_RUNTIME_DIR=${runtime}" \
    "DBUS_SESSION_BUS_ADDRESS=unix:path=${bus}" \
    systemctl --user start "${UNIT}" >/dev/null 2>&1; then
    log "started ${UNIT} for ${user}"
  else
    log "warning: could not start ${UNIT} for ${user} (will rely on autostart at next login)"
  fi

  # Helper may lack hecate-ipc in its credential token (no sg / no re-login).
  # Root can still fix sock/token group so the agent service can connect.
  i=0
  while [ "${i}" -lt 20 ]; do
    if [ -S "${RUNTIME_PARENT}/desktop.sock" ] && [ -f "${RUNTIME_PARENT}/ipc.token" ]; then
      chgrp hecate-ipc "${RUNTIME_PARENT}/desktop.sock" "${RUNTIME_PARENT}/ipc.token" 2>/dev/null || true
      chmod 0660 "${RUNTIME_PARENT}/desktop.sock" 2>/dev/null || true
      chmod 0640 "${RUNTIME_PARENT}/ipc.token" 2>/dev/null || true
      break
    fi
    i=$((i + 1))
    sleep 0.25 2>/dev/null || sleep 1
  done

  # Re-arm / run the system path oneshot. Older packages used RemainAfterExit on
  # PathExists(desktop.sock), which skipped chgrp after fast sock recreate.
  if command -v systemctl >/dev/null 2>&1; then
    systemctl stop hecate-lampad-desktop-ipc-fix.service >/dev/null 2>&1 || true
  fi
  if [ -x /usr/lib/hecate-lampad-desktop/fix-ipc-perms.sh ]; then
    /usr/lib/hecate-lampad-desktop/fix-ipc-perms.sh || true
  fi
  # Nudge PathChanged watcher for this and future helper recreates.
  : > "${RUNTIME_PARENT}/desktop.ipc-fix" 2>/dev/null || true
  if command -v systemctl >/dev/null 2>&1; then
    systemctl start hecate-lampad-desktop-ipc-fix.service >/dev/null 2>&1 || true
  fi
}

activate_loginctl_sessions() {
  command -v loginctl >/dev/null 2>&1 || return 0
  # Columns: SESSION UID USER SEAT TTY
  loginctl list-sessions --no-legend 2>/dev/null | while read -r sid uid user _rest; do
    case "${sid}" in
      ''|SESSION) continue ;;
    esac
    case "${uid}" in
      ''|*[!0-9]*) continue ;;
    esac
    [ "${uid}" -ge 1000 ] || continue
    [ -n "${user}" ] || continue
    session_is_graphical "${sid}" || continue
    enable_and_start_for_user "${user}" "${uid}" "${sid}"
  done
}

activate_seat0_fallback() {
  # When loginctl session typing is sparse, still try the console owner of :0.
  if [ -S /tmp/.X11-unix/X0 ] || [ -n "${DISPLAY:-}" ]; then
    console_user="$(stat -c '%U' /dev/tty0 2>/dev/null || true)"
    if [ -z "${console_user}" ] || [ "${console_user}" = "root" ]; then
      console_user="$(who 2>/dev/null | awk '/:0/{print $1; exit}')"
    fi
    if [ -n "${console_user}" ] && [ "${console_user}" != "root" ]; then
      uid="$(id -u "${console_user}" 2>/dev/null || true)"
      case "${uid}" in
        ''|*[!0-9]*) return 0 ;;
      esac
      [ "${uid}" -ge 1000 ] || return 0
      enable_and_start_for_user "${console_user}" "${uid}" ""
    fi
  fi
}

ensure_group
add_interactive_users_to_ipc

if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi

activate_loginctl_sessions
activate_seat0_fallback

exit 0
