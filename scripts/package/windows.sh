#!/usr/bin/env bash
# Windows steps. Runs in docker/windows-<arch>.Dockerfile.
#   windows.sh x86_64|arm64 build   compile fastcnsl.exe
#   windows.sh x86_64|arm64 stage   collect the files to ship in
#                                   $TARGET_DIR/package-windows-<arch>/fastcnsl
#                                   (the installer's payload, see windows-setup.sh)
#   windows.sh x86_64|arm64 zip     stage, then zip it with the portable marker
source "$(dirname "$0")/common.sh"
ARCH="${1:?usage: windows.sh x86_64|arm64 build|stage|zip}"
STEP="${2:?usage: windows.sh x86_64|arm64 build|stage|zip}"
case "$ARCH" in
    x86_64) TRIPLE=x86_64-pc-windows-gnu;     OBJDUMP=x86_64-w64-mingw32-objdump; SYSROOT=/usr/x86_64-w64-mingw32
            LINUX=x86_64-unknown-linux-musl ;;
    arm64)  TRIPLE=aarch64-pc-windows-gnullvm; OBJDUMP=llvm-objdump;               SYSROOT=/opt/llvm-mingw/aarch64-w64-mingw32
            LINUX=aarch64-unknown-linux-musl ;;
    *) echo "unknown arch $ARCH" >&2; exit 1 ;;
esac

EXE="$TARGET_DIR/$TRIPLE/release/fastcnsl.exe"
# The static Linux helper for WSL tabs (wsl-relay/, see src/wsl.rs).
RELAY="$TARGET_DIR/$LINUX/release/fastcnsl-wsl"
# Microsoft's ConPTY, downloaded into the image by docker/conpty.sh.
CONPTY=/opt/conpty

# Puts conpty.dll and OpenConsole.exe next to fastcnsl.exe in $1: the
# terminal then uses them instead of Windows' own, much slower, console host.
add_conpty() {
    if [[ -f "$CONPTY/conpty.dll" && -f "$CONPTY/OpenConsole.exe" ]]; then
        cp "$CONPTY/conpty.dll" "$CONPTY/OpenConsole.exe" "$1/"
    else
        echo "warning: $CONPTY missing; using the console host built into Windows" >&2
    fi
}

if [[ "$STEP" == build ]]; then
    say "compiling fastcnsl $VERSION for $TRIPLE"
    cargo build --release --locked --target "$TRIPLE"
    say "compiling fastcnsl-wsl for $LINUX"
    cargo build --release --locked --target "$LINUX" --manifest-path wsl-relay/Cargo.toml
    cp "$RELAY" "$(dirname "$EXE")/"
    add_conpty "$(dirname "$EXE")"
    echo "-> ${EXE#$ROOT/}"
    exit 0
fi
[[ "$STEP" == stage || "$STEP" == zip ]] || { echo "unknown step: $STEP" >&2; exit 1; }
need_binary "$EXE" "windows-$ARCH"
say "staging the files to ship"

PKG="$TARGET_DIR/package-windows-$ARCH"
STAGE="$PKG/fastcnsl"
rm -rf "$STAGE" && mkdir -p "$STAGE"
cp "$EXE" "$RELAY" "$STAGE/"
cp README.md "$STAGE/" 2>/dev/null || true
cp LICENSE "$STAGE/" 2>/dev/null || true
add_conpty "$STAGE"
[[ -f "$STAGE/conpty.dll" ]] && cp packaging/windows/ConPTY-LICENSE.txt "$STAGE/"

# Ship any toolchain runtime DLL the exe imports (e.g. libunwind.dll from
# llvm-mingw); everything else must be a Windows system DLL.
for dll in $("$OBJDUMP" -p "$EXE" | sed -n 's/.*DLL Name: //p'); do
    found=$(find "$SYSROOT" -iname "$dll" 2>/dev/null | head -1 || true)
    if [[ -n "$found" ]]; then
        echo "bundling $dll"
        cp "$found" "$STAGE/"
    fi
done

echo "-> ${STAGE#$ROOT/}"
[[ "$STEP" == zip ]] || exit 0

# The zip is the portable copy: the marker keeps its settings in its folder
# (src/config.rs, PORTABLE_MARKER). The installer ships the stage without it.
say "packaging .zip"
ZIPDIR="$PKG/zip"
rm -rf "$ZIPDIR" && mkdir -p "$ZIPDIR"
cp -r "$STAGE" "$ZIPDIR/"
cp packaging/windows/portable "$ZIPDIR/fastcnsl/"
OUT="$DIST/fastcnsl-${VERSION}-windows-$ARCH.zip"
rm -f "$OUT"
(cd "$ZIPDIR" && zip -qr "$OUT" fastcnsl)
echo "-> ${OUT#$ROOT/}"
