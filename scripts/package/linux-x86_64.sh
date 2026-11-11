#!/usr/bin/env bash
# Linux x86-64 steps. Runs in docker/linux-x86_64.Dockerfile.
#   linux-x86_64.sh build      compile target/linux-x86_64/release/fastcnsl
#   linux-x86_64.sh deb        package the compiled binary as a .deb
#   linux-x86_64.sh rpm        package the compiled binary as an .rpm
#   linux-x86_64.sh appimage   package the compiled binary as an AppImage
source "$(dirname "$0")/common.sh"
STEP="${1:?usage: linux-x86_64.sh build|deb|rpm|appimage}"
BIN="$TARGET_DIR/release/fastcnsl"

case "$STEP" in
build)
    say "compiling fastcnsl $VERSION for x86_64-unknown-linux-gnu"
    cargo build --release --locked
    echo "-> ${BIN#$ROOT/}"
    ;;
deb)
    need_binary "$BIN" linux-x86_64
    say "packaging .deb"
    OUT="$DIST/fastcnsl_${VERSION}_amd64.deb"
    cargo deb --no-build --deb-version "$VERSION" --output "$OUT"
    echo "-> ${OUT#$ROOT/}"
    ;;
rpm)
    need_binary "$BIN" linux-x86_64
    say "packaging .rpm"
    OUT="$DIST/fastcnsl-${VERSION}-1.x86_64.rpm"
    # gzip payload: older rpm versions (openSUSE Leap) may lack zstd.
    cargo generate-rpm --payload-compress gzip --set-metadata "version = \"$VERSION\"" --output "$OUT"
    echo "-> ${OUT#$ROOT/}"
    ;;
appimage)
    need_binary "$BIN" linux-x86_64
    say "packaging AppImage"
    APPDIR="$TARGET_DIR/Fastcnsl.AppDir"
    rm -rf "$APPDIR"
    install -Dm755 "$BIN" "$APPDIR/usr/bin/fastcnsl"
    install -Dm644 packaging/linux/fastcnsl.desktop "$APPDIR/usr/share/applications/fastcnsl.desktop"
    install -Dm644 packaging/icons/fastcnsl-256.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/fastcnsl.png"
    cp packaging/linux/fastcnsl.desktop "$APPDIR/fastcnsl.desktop"
    cp packaging/icons/fastcnsl-256.png "$APPDIR/fastcnsl.png"
    ln -s usr/bin/fastcnsl "$APPDIR/AppRun"
    desktop-file-validate "$APPDIR/fastcnsl.desktop"

    OUT="$DIST/Fastcnsl-${VERSION}-x86_64.AppImage"
    # Update information for AppImageUpdate, Gear Lever and AppImageLauncher:
    # the latest GitHub release's fastcnsl-linux-x86_64.AppImage.zsync,
    # which CI makes for the fixed-name copy it uploads.
    UPDATE_INFO="${APPIMAGE_UPDATE_INFO-gh-releases-zsync|andrewpushkin|fastcnsl|latest|fastcnsl-linux-x86_64.AppImage.zsync}"
    ARCH=x86_64 appimagetool --no-appstream --runtime-file /opt/appimage-runtime-x86_64 \
        ${UPDATE_INFO:+--updateinformation "$UPDATE_INFO"} "$APPDIR" "$OUT"
    # appimagetool also writes a .zsync into the current directory. It names
    # the versioned file, so it's useless; CI makes one for the release name.
    rm -f "$(basename "$OUT").zsync" "$OUT.zsync"
    echo "-> ${OUT#$ROOT/}"
    ;;
*)
    echo "unknown step: $STEP" >&2
    exit 1
    ;;
esac
