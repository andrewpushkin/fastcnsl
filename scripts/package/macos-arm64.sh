#!/usr/bin/env bash
# macOS Apple Silicon steps. Runs in docker/macos-arm64.Dockerfile, or
# natively on a Mac.
#   macos-arm64.sh build   compile the binary
#   macos-arm64.sh zip     bundle it as Ragamuffin.app, sign, zip
source "$(dirname "$0")/common.sh"
STEP="${1:?usage: macos-arm64.sh build|zip}"
TRIPLE=aarch64-apple-darwin
BIN="$TARGET_DIR/$TRIPLE/release/ragamuffin"

if [[ "$STEP" == build ]]; then
    say "compiling ragamuffin $VERSION for $TRIPLE"
    cargo build --release --locked --target "$TRIPLE"
    echo "-> ${BIN#$ROOT/}"
    exit 0
fi
[[ "$STEP" == zip ]] || { echo "unknown step: $STEP" >&2; exit 1; }
need_binary "$BIN" macos-arm64

say "bundling Ragamuffin.app"
STAGE="$TARGET_DIR/package-macos"
APP="$STAGE/Ragamuffin.app"
rm -rf "$STAGE" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/ragamuffin"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"

ICONSET="$STAGE/icons"
mkdir -p "$ICONSET"
if command -v convert >/dev/null; then
    for s in 128 256 512; do
        convert -background none packaging/icons/ragamuffin.svg -resize ${s}x${s} "$ICONSET/icon_$s.png"
    done
else  # native macOS
    for s in 128 256 512; do
        sips -s format png -z $s $s packaging/icons/ragamuffin-256.png --out "$ICONSET/icon_$s.png" >/dev/null
    done
fi
python3 scripts/package/make_icns.py "$APP/Contents/Resources/ragamuffin.icns" "$ICONSET"/icon_{128,256,512}.png

# Apple Silicon only runs signed code; ad-hoc signing needs no certificate.
if command -v rcodesign >/dev/null; then
    rcodesign sign "$APP"
else
    codesign --force --deep --sign - "$APP"
fi

OUT="$DIST/ragamuffin-${VERSION}-macos-arm64.zip"
rm -f "$OUT"
(cd "$STAGE" && zip -qry "$OUT" Ragamuffin.app)
echo "-> ${OUT#$ROOT/}"
