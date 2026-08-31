#!/usr/bin/env bash
# Shared helpers for macOS packaging scripts (sourced, not executed).

# Normalize Apple Silicon uname (arm64) to the Hecate/release_sync arch (aarch64).
macos_package_arch() {
  local raw="${1:-$(uname -m)}"
  case "${raw}" in
    arm64|aarch64) echo aarch64 ;;
    x86_64|amd64) echo x86_64 ;;
    *) echo "${raw}" ;;
  esac
}

# Portable SHA-256 checksum line (sha256sum-compatible when possible).
sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}"
  else
    echo "Error: neither sha256sum nor shasum found" >&2
    return 1
  fi
}

# Build an unsigned component .pkg from a payload root (requires pkgbuild).
# Optional scripts_dir may contain preinstall/postinstall executables.
create_pkg_from_root() {
  local root="$1"
  local scripts_dir="$2"
  local identifier="$3"
  local version="$4"
  local output="$5"
  local -a args

  if ! command -v pkgbuild >/dev/null 2>&1; then
    echo "Error: pkgbuild not found (required to build macOS PKG packages)" >&2
    return 1
  fi

  rm -f "${output}"
  args=(
    --root "${root}"
    --identifier "${identifier}"
    --version "${version}"
    --install-location /
  )
  if [ -n "${scripts_dir}" ] && [ -d "${scripts_dir}" ]; then
    # pkgbuild copies script modes into the package; installer refuses non-executable
    # postinstall/preinstall (common after git checkouts that drop +x).
    local script
    for script in "${scripts_dir}"/*; do
      [ -f "${script}" ] || continue
      case "$(basename "${script}")" in
        preinstall|postinstall|preupgrade|postupgrade)
          chmod a+x "${script}"
          ;;
      esac
    done
    args+=(--scripts "${scripts_dir}")
  fi
  pkgbuild "${args[@]}" "${output}"
}
