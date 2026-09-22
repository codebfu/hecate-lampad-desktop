# Changelog

## 1.0.7 — 2026-09-22

- Re-validate `app.launch` against the local helper shell allowlists before launching (same policy file as `shell.run`; defense in depth for the §11 sandbox escape).

## 1.0.6 — 2026-09-22

- Fix IPC path unit oneshot: set `RemainAfterExit=yes` so `PathExists` does not restart the fix service in a tight loop.

## 1.0.5 — 2026-09-22

- Fix IPC path unit: drop `PathChanged` (chgrp/chmod retriggered it into `unit-start-limit-hit`).

## 1.0.4 — 2026-09-22

- Add systemd path unit (`hecate-lampad-desktop-ipc-fix`) so root re-applies `chgrp hecate-ipc` whenever `desktop.sock` is created or replaced (survives helper restart / agent update).
- Drop useless `Depends: login` — modern Ubuntu no longer ships `sg` in that package.

## 1.0.3 — 2026-09-22

- Depend on `login` (`sg`) so the helper can activate `hecate-ipc` without a re-login.
- After starting the user unit, force `chgrp hecate-ipc` on `desktop.sock` / `ipc.token` so the agent can connect even when `sg` is missing.

## 1.0.2 — 2026-09-22

- Fix Linux postinst: stop forcing `/etc/hecate-lampad` to `root:root` `0750`, which made the agent unable to read `config.toml` after `helper.install`.

## 1.0.1 — 2026-09-21

- Linux install (`helper.install` / `.deb` postinst) now grants `hecate-ipc` and starts the user-session helper in live GUI sessions (aligned with macOS/Windows).
- Add `run-helper.sh` so `hecate-ipc` applies via `sg` without requiring a re-login after `usermod`.

## 1.0.0 — 2026-08-31

Initial public release.
