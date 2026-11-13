#!/usr/bin/env bash
# Windows installer. Runs in docker/windows-setup.Dockerfile (Inno Setup
# under Wine), after `windows.sh <arch> zip` has staged the files.
#   windows-setup.sh x86_64|arm64   -> dist/fastcnsl-<ver>-windows-<arch>-setup.exe
source "$(dirname "$0")/common.sh"
ARCH="${1:?usage: windows-setup.sh x86_64|arm64}"
case "$ARCH" in
    x86_64) INNO_ARCH=x64 ;;
    arm64)  INNO_ARCH=arm64 ;;
    *) echo "unknown arch $ARCH" >&2; exit 1 ;;
esac

STAGE="$TARGET_DIR/package-windows-$ARCH/fastcnsl"
need_binary "$STAGE/fastcnsl.exe" "windows-$ARCH-zip"
say "building the installer"

# A fresh Wine prefix per run (see the Dockerfile). Wine also wants the
# prefix's parent folder to be ours, hence the private temp folder.
WINETMP="$(mktemp -d)"
trap 'wineserver -k 2>/dev/null || true; rm -rf "$WINETMP"' EXIT
export WINEPREFIX="$WINETMP/prefix"
export WINEDEBUG=-all
NAME="fastcnsl-${VERSION}-windows-$ARCH-setup"
wine /opt/inno/ISCC.exe \
    "/DAppVersion=$VERSION" \
    "/DArch=$INNO_ARCH" \
    "/DSourceDir=$(winepath -w "$STAGE")" \
    "/DIconFile=$(winepath -w "$ROOT/packaging/windows/fastcnsl.ico")" \
    "/O$(winepath -w "$DIST")" \
    "/F$NAME" \
    "$(winepath -w "$ROOT/packaging/windows/fastcnsl.iss")"
wineserver -w
test -f "$DIST/$NAME.exe"
echo "-> ${DIST#$ROOT/}/$NAME.exe"
