#!/usr/bin/env bash
# Build hecate-lampad-desktop Windows MSI (requires mingw-w64 + wixl + msitools).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

VERSION="${VERSION:-$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')}"
WIX_VERSION="${VERSION}.0"
TARGET="x86_64-pc-windows-gnu"
DIST="${ROOT}/dist"
WORK="$(mktemp -d)"
STAGING="${WORK}/staging"

trap 'rm -rf "${WORK}"' EXIT

rustup target add "${TARGET}"
cargo build --release --target "${TARGET}"

mkdir -p "${STAGING}" "${DIST}"
install -m 755 "target/${TARGET}/release/hecate-lampad-desktop.exe" \
  "${STAGING}/hecate-lampad-desktop.exe"
x86_64-w64-mingw32-gcc -O2 -s -o "${STAGING}/register-logon-task.exe" \
  packaging/windows/register-logon-task.c
python3 - \
  packaging/windows/hecate-lampad-desktop-logon.xml \
  "${STAGING}/hecate-lampad-desktop-logon.xml" <<'PY'
import sys
from pathlib import Path
src = Path(sys.argv[1]).read_text(encoding="utf-8")
Path(sys.argv[2]).write_bytes(src.encode("utf-16"))
PY
install -m 644 README.md "${STAGING}/README.md"

WXS="${WORK}/hecate-lampad-desktop.wxs"
cat > "${WXS}" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Product
    Id="*"
    Name="Hecate Lampad Desktop Helper"
    Language="1033"
    Version="${WIX_VERSION}"
    Manufacturer="Hecate Contributors"
    UpgradeCode="a7c3e1f4-2b8d-4e6a-9c12-5f0d8e3b7a91">
    <Package
      InstallerVersion="500"
      Compressed="yes"
      InstallScope="perMachine"
      Description="User-session GUI helper for Hecate computer-use commands"
      Comments="Hecate Lampad Desktop" />
    <Media Id="1" Cabinet="hecate-lampad-desktop.cab" EmbedCab="yes" />
    <UIRef Id="WixUI_Minimal" />
    <MajorUpgrade
      AllowSameVersionUpgrades="yes"
      DowngradeErrorMessage="A newer version of Hecate Lampad Desktop is already installed." />
    <Directory Id="TARGETDIR" Name="SourceDir">
      <Directory Id="ProgramFiles64Folder">
        <Directory Id="INSTALLDIR" Name="hecate-lampad-desktop">
          <Component Id="MainExecutable" Guid="3c8f1a62-7e4b-4d90-b1f6-9a2e5c7d8b43">
            <File Id="HecateLampadDesktopExe" Source="staging/hecate-lampad-desktop.exe" KeyPath="yes" />
          </Component>
          <Component Id="RegisterLogonTaskExe" Guid="c4e8a1b2-9f3d-4c70-a6e1-2b5d8f0c7a14">
            <File Id="RegisterLogonTaskExeFile" Source="staging/register-logon-task.exe" KeyPath="yes" />
          </Component>
          <Component Id="LogonTaskXml" Guid="6b2d9f15-8a3c-4e71-c5d9-1f7e4a8b6c32">
            <File Id="HecateLampadDesktopLogonXml" Source="staging/hecate-lampad-desktop-logon.xml" />
          </Component>
          <Component Id="Readme" Guid="9e4a7c21-5b8f-4d63-a2e7-3c1f6b9d4e82">
            <File Id="HecateLampadDesktopReadme" Source="staging/README.md" />
          </Component>
        </Directory>
      </Directory>
    </Directory>
    <Feature Id="DefaultFeature" Title="Hecate Lampad Desktop" Level="1">
      <ComponentRef Id="MainExecutable" />
      <ComponentRef Id="RegisterLogonTaskExe" />
      <ComponentRef Id="LogonTaskXml" />
      <ComponentRef Id="Readme" />
    </Feature>
    <CustomAction
      Id="RegisterDesktopLogonTask"
      FileKey="RegisterLogonTaskExeFile"
      ExeCommand="install"
      Execute="deferred"
      Impersonate="no"
      Return="ignore" />
    <CustomAction
      Id="StartDesktopHelper"
      FileKey="RegisterLogonTaskExeFile"
      ExeCommand="start"
      Execute="deferred"
      Impersonate="no"
      Return="ignore" />
    <CustomAction
      Id="UnregisterDesktopLogonTask"
      Directory="SystemFolder"
      ExeCommand="schtasks.exe /Delete /TN &quot;Hecate Lampad Desktop&quot; /F"
      Execute="immediate"
      Impersonate="no"
      Return="ignore" />
    <InstallExecuteSequence>
      <RemoveExistingProducts After="InstallValidate" />
      <Custom Action="RegisterDesktopLogonTask" After="InstallFiles">NOT REMOVE</Custom>
      <Custom Action="StartDesktopHelper" After="RegisterDesktopLogonTask">NOT REMOVE</Custom>
      <Custom Action="UnregisterDesktopLogonTask" After="UnpublishFeatures">REMOVE="ALL"</Custom>
    </InstallExecuteSequence>
  </Product>
</Wix>
EOF

install -m 644 packaging/windows/License.rtf "${WORK}/License.rtf"
OUTPUT="${DIST}/hecate-lampad-desktop_${VERSION}_windows-amd64.msi"
(
  cd "${WORK}"
  wixl -a x64 --ext ui -o "${OUTPUT}" "hecate-lampad-desktop.wxs"
)
command -v msibuild >/dev/null 2>&1 || {
  echo "Error: msibuild not found (install msitools)" >&2
  exit 1
}
msibuild "${OUTPUT}" \
  -q "UPDATE \`InstallExecuteSequence\` SET \`Sequence\`=1401 WHERE \`Action\`='RemoveExistingProducts'" \
  -q "UPDATE \`InstallExecuteSequence\` SET \`Sequence\`=1801 WHERE \`Action\`='UnregisterDesktopLogonTask'"
sha256sum "${OUTPUT}" > "${OUTPUT}.sha256"
echo "Built ${OUTPUT}"
