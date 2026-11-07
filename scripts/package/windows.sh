#!/usr/bin/env bash
# Windows steps. Runs in docker/windows-<arch>.Dockerfile.
#   windows.sh x86_64|arm64 build   compile ragamuffin.exe
#   windows.sh x86_64|arm64 zip     package the compiled exe as a .zip
source "$(dirname "$0")/common.sh"
ARCH="${1:?usage: windows.sh x86_64|arm64 build|zip}"
STEP="${2:?usage: windows.sh x86_64|arm64 build|zip}"
case "$ARCH" in
    x86_64) TRIPLE=x86_64-pc-windows-gnu;     OBJDUMP=x86_64-w64-mingw32-objdump; SYSROOT=/usr/x86_64-w64-mingw32 ;;
    arm64)  TRIPLE=aarch64-pc-windows-gnullvm; OBJDUMP=llvm-objdump;               SYSROOT=/opt/llvm-mingw/aarch64-w64-mingw32 ;;
    *) echo "unknown arch $ARCH" >&2; exit 1 ;;
esac

EXE="$TARGET_DIR/$TRIPLE/release/ragamuffin.exe"
# Microsoft's ConPTY, downloaded into the image by docker/conpty.sh.
CONPTY=/opt/conpty

# Puts conpty.dll and OpenConsole.exe next to ragamuffin.exe in $1: the
# terminal then uses them instead of Windows' own, much slower, console host.
add_conpty() {
    if [[ -f "$CONPTY/conpty.dll" && -f "$CONPTY/OpenConsole.exe" ]]; then
        cp "$CONPTY/conpty.dll" "$CONPTY/OpenConsole.exe" "$1/"
    else
        echo "warning: $CONPTY missing; using the console host built into Windows" >&2
    fi
}

if [[ "$STEP" == build ]]; then
    say "compiling ragamuffin $VERSION for $TRIPLE"
    cargo build --release --locked --target "$TRIPLE"
    add_conpty "$(dirname "$EXE")"
    echo "-> ${EXE#$ROOT/}"
    exit 0
fi
[[ "$STEP" == zip ]] || { echo "unknown step: $STEP" >&2; exit 1; }
need_binary "$EXE" "windows-$ARCH"
say "packaging .zip"

STAGE="$TARGET_DIR/package-windows-$ARCH/ragamuffin"
rm -rf "$STAGE" && mkdir -p "$STAGE"
cp "$EXE" "$STAGE/"
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

OUT="$DIST/ragamuffin-${VERSION}-windows-$ARCH.zip"
rm -f "$OUT"
(cd "$(dirname "$STAGE")" && zip -qr "$OUT" ragamuffin)
echo "-> ${OUT#$ROOT/}"
