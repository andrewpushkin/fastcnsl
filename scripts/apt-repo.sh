#!/usr/bin/env bash
# Builds a signed apt repository from .deb files. CI runs it on every
# release and publishes the result on GitHub Pages; see README "Install".
#
#   scripts/apt-repo.sh <out-dir> <file.deb>...
#
# <out-dir> is replaced. Needs apt-ftparchive (apt-utils) and gpg. Signs
# with the secret key in the gpg keyring, or with $GPG_KEY_ID when the
# keyring holds more than one. The key must have no passphrase.
#
# Layout (apt line: deb <url> stable main):
#   pool/main/ragamuffin_<ver>_amd64.deb
#   dists/stable/{Release,InRelease,Release.gpg}
#   dists/stable/main/binary-amd64/Packages{,.gz}
#   ragamuffin.gpg     public key, binary (for signed-by=)
#   ragamuffin.asc     public key, armored
#   ragamuffin.sources apt source file (deb822)
#   index.html         install instructions
set -euo pipefail

OUT="${1:?usage: apt-repo.sh <out-dir> <file.deb>...}"
shift
[[ $# -gt 0 ]] || { echo "error: no .deb files given" >&2; exit 1; }

URL="${APT_REPO_URL:-https://andrewpushkin.github.io/ragamuffin/apt}"
SUITE=stable
COMPONENT=main
ARCH=amd64
KEY=(${GPG_KEY_ID:+--local-user "$GPG_KEY_ID"})

if ! gpg --batch --list-secret-keys ${GPG_KEY_ID:-} >/dev/null 2>&1; then
    echo "error: no gpg secret key to sign with" >&2
    exit 1
fi

rm -rf "$OUT"
mkdir -p "$OUT/pool/$COMPONENT" "$OUT/dists/$SUITE/$COMPONENT/binary-$ARCH"

# Pool files are named from the package's own control fields, so any input
# name works (the release assets have fixed names without a version).
for deb in "$@"; do
    name=$(dpkg-deb -f "$deb" Package)_$(dpkg-deb -f "$deb" Version)_$(dpkg-deb -f "$deb" Architecture).deb
    cp "$deb" "$OUT/pool/$COMPONENT/$name"
done

cd "$OUT"
BIN="dists/$SUITE/$COMPONENT/binary-$ARCH"
apt-ftparchive packages "pool/$COMPONENT" > "$BIN/Packages"
gzip -9kn "$BIN/Packages"

# Written outside dists/ first: apt-ftparchive would otherwise hash the
# half-written Release file into itself.
apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=ragamuffin \
    -o APT::FTPArchive::Release::Label=ragamuffin \
    -o APT::FTPArchive::Release::Suite=$SUITE \
    -o APT::FTPArchive::Release::Codename=$SUITE \
    -o APT::FTPArchive::Release::Architectures=$ARCH \
    -o APT::FTPArchive::Release::Components=$COMPONENT \
    -o APT::FTPArchive::Release::Description="ragamuffin terminal emulator" \
    release "dists/$SUITE" > Release.tmp
mv Release.tmp "dists/$SUITE/Release"

gpg --batch --yes "${KEY[@]}" --clearsign -o "dists/$SUITE/InRelease" "dists/$SUITE/Release"
gpg --batch --yes "${KEY[@]}" --armor --detach-sign -o "dists/$SUITE/Release.gpg" "dists/$SUITE/Release"

FPR=$(gpg --batch --with-colons --list-secret-keys ${GPG_KEY_ID:-} | awk -F: '$1 == "fpr" { print $10; exit }')
gpg --batch --export "$FPR" > ragamuffin.gpg
gpg --batch --armor --export "$FPR" > ragamuffin.asc

cat > ragamuffin.sources <<EOF
Types: deb
URIs: $URL
Suites: $SUITE
Components: $COMPONENT
Architectures: $ARCH
Signed-By: /usr/share/keyrings/ragamuffin.gpg
EOF

VERSION=$(sed -n 's/^Version: //p' "$BIN/Packages" | sort -V | tail -n 1)
cat > index.html <<EOF
<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ragamuffin apt repository</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:46rem;margin:2rem auto;padding:0 1rem}pre{background:#8882;padding:1rem;overflow-x:auto}</style>
<h1>ragamuffin apt repository</h1>
<p>Debian and Ubuntu (x86-64). Latest version: <b>$VERSION</b>.</p>
<pre>curl -fsSL $URL/ragamuffin.gpg | sudo tee /usr/share/keyrings/ragamuffin.gpg &gt;/dev/null
curl -fsSL $URL/ragamuffin.sources | sudo tee /etc/apt/sources.list.d/ragamuffin.sources &gt;/dev/null
sudo apt update
sudo apt install ragamuffin</pre>
<p>Signing key fingerprint: <code>$FPR</code></p>
<p>Source: <a href="https://github.com/andrewpushkin/ragamuffin">github.com/andrewpushkin/ragamuffin</a></p>
EOF

echo "apt repository for ragamuffin $VERSION -> $OUT (key $FPR)"
