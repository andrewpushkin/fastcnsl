#!/usr/bin/env bash
# macOS Apple Silicon steps. Runs in docker/macos-arm64.Dockerfile, or
# natively on a Mac.
#   macos-arm64.sh build   compile the binary
#   macos-arm64.sh dmg     bundle it as Fastcnsl.app, sign, put it in a .dmg
source "$(dirname "$0")/common.sh"
STEP="${1:?usage: macos-arm64.sh build|dmg}"
TRIPLE=aarch64-apple-darwin
BIN="$TARGET_DIR/$TRIPLE/release/fastcnsl"

if [[ "$STEP" == build ]]; then
    say "compiling fastcnsl $VERSION for $TRIPLE"
    cargo build --release --locked --target "$TRIPLE"
    echo "-> ${BIN#$ROOT/}"
    exit 0
fi
[[ "$STEP" == dmg ]] || { echo "unknown step: $STEP" >&2; exit 1; }
need_binary "$BIN" macos-arm64

say "bundling Fastcnsl.app"
STAGE="$TARGET_DIR/package-macos"
APP="$STAGE/Fastcnsl.app"
rm -rf "$STAGE" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/fastcnsl"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"

ICONSET="$STAGE/icons"
mkdir -p "$ICONSET"
if command -v convert >/dev/null; then
    for s in 128 256 512; do
        convert -background none packaging/icons/fastcnsl.svg -resize ${s}x${s} "$ICONSET/icon_$s.png"
    done
else  # native macOS
    for s in 128 256 512; do
        sips -s format png -z $s $s packaging/icons/fastcnsl-256.png --out "$ICONSET/icon_$s.png" >/dev/null
    done
fi
python3 scripts/package/make_icns.py "$APP/Contents/Resources/fastcnsl.icns" "$ICONSET"/icon_{128,256,512}.png

# Apple Silicon only runs signed code; ad-hoc signing needs no certificate.
if command -v rcodesign >/dev/null; then
    rcodesign sign "$APP"
else
    codesign --force --deep --sign - "$APP"
fi

say "making the disk image"
# What the mounted image shows: the app and a link to drag it onto.
VOL="$STAGE/volume"
mkdir -p "$VOL"
mv "$APP" "$VOL/"
ln -s /Applications "$VOL/Applications"

OUT="$DIST/fastcnsl-${VERSION}-macos-arm64.dmg"
rm -f "$OUT"
if command -v hdiutil >/dev/null; then
    # Native macOS: compressed UDIF. hdiutil sometimes fails with "Resource
    # busy" on CI runners; a retry gets past it.
    for try in 1 2 3; do
        hdiutil create -quiet -volname Fastcnsl -srcfolder "$VOL" -fs HFS+ \
            -format UDZO -ov "$OUT" && break
        [[ $try == 3 ]] && exit 1
        sleep 5
    done
else
    # Linux: an uncompressed ISO 9660 / HFS+ hybrid. macOS mounts its HFS+
    # side (modes and the symlink kept) like any .dmg.
    xorriso -as mkisofs -quiet -V Fastcnsl -hfsplus -apm-block-size 2048 \
        -R -no-pad -o "$OUT" "$VOL"
fi
echo "-> ${OUT#$ROOT/}"
