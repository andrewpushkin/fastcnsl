#!/bin/sh
# Builds the website, https://andrewpushkin.github.io/fastcnsl/, from
# website/index.html. CI runs it on every release and publishes the result
# on GitHub Pages next to the apt and rpm repositories (job repos in
# .github/workflows/build.yml). `make website` builds it for a look.
#
#   scripts/website.sh <out-dir>
#
# Writes <out-dir>/index.html and <out-dir>/img/, and leaves anything else
# in <out-dir> alone. The page shows $VERSION (default: get_version.sh) and
# the date of the current commit. Its screenshots are the README's, from docs/.
set -eu

OUT="${1:?usage: website.sh <out-dir>}"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION="${VERSION:-$("$ROOT/scripts/get_version.sh")}"
DATE=$(git -C "$ROOT" log -1 --format=%cd --date=format:'%-d %B %Y')

mkdir -p "$OUT/img"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@DATE@/$DATE/g" \
    "$ROOT/website/index.html" > "$OUT/index.html"
for f in screenshot light search-colors bookmarks; do
    cp "$ROOT/docs/$f.png" "$OUT/img/"
done
cp "$ROOT/packaging/icons/fastcnsl.svg" "$ROOT/packaging/icons/fastcnsl-256.png" "$OUT/img/"

if grep -n '@[A-Z]*@' "$OUT/index.html"; then
    echo "error: placeholder left in $OUT/index.html" >&2
    exit 1
fi
echo "website $VERSION -> $OUT"
