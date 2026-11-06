#!/usr/bin/env bash
# Linux x86-64 steps. Runs in docker/linux-x86_64.Dockerfile.
#   linux-x86_64.sh build      compile target/linux-x86_64/release/ragamuffin
#   linux-x86_64.sh deb        package the compiled binary as a .deb
#   linux-x86_64.sh appimage   package the compiled binary as an AppImage
source "$(dirname "$0")/common.sh"
STEP="${1:?usage: linux-x86_64.sh build|deb|appimage}"
BIN="$TARGET_DIR/release/ragamuffin"

case "$STEP" in
build)
    say "compiling ragamuffin $VERSION for x86_64-unknown-linux-gnu"
    cargo build --release --locked
    echo "-> ${BIN#$ROOT/}"
    ;;
deb)
    need_binary "$BIN" linux-x86_64
    say "packaging .deb"
    OUT="$DIST/ragamuffin_${VERSION}_amd64.deb"
    cargo deb --no-build --output "$OUT"
    echo "-> ${OUT#$ROOT/}"
    ;;
appimage)
    need_binary "$BIN" linux-x86_64
    say "packaging AppImage"
    APPDIR="$TARGET_DIR/Ragamuffin.AppDir"
    rm -rf "$APPDIR"
    install -Dm755 "$BIN" "$APPDIR/usr/bin/ragamuffin"
    install -Dm644 packaging/linux/ragamuffin.desktop "$APPDIR/usr/share/applications/ragamuffin.desktop"
    install -Dm644 packaging/icons/ragamuffin-256.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/ragamuffin.png"
    cp packaging/linux/ragamuffin.desktop "$APPDIR/ragamuffin.desktop"
    cp packaging/icons/ragamuffin-256.png "$APPDIR/ragamuffin.png"
    ln -s usr/bin/ragamuffin "$APPDIR/AppRun"
    desktop-file-validate "$APPDIR/ragamuffin.desktop"

    OUT="$DIST/Ragamuffin-${VERSION}-x86_64.AppImage"
    ARCH=x86_64 appimagetool --no-appstream --runtime-file /opt/appimage-runtime-x86_64 "$APPDIR" "$OUT"
    echo "-> ${OUT#$ROOT/}"
    ;;
*)
    echo "unknown step: $STEP" >&2
    exit 1
    ;;
esac
