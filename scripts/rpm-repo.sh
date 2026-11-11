#!/usr/bin/env bash
# Builds a signed rpm repository (dnf, zypper) from .rpm files. CI runs it
# on every release next to scripts/apt-repo.sh and publishes the result on
# GitHub Pages; see README "Install".
#
#   scripts/rpm-repo.sh <out-dir> <file.rpm>...
#
# <out-dir> is replaced. Needs createrepo_c, rpmsign (rpm) and gpg. Signs
# the packages and repodata/repomd.xml with the secret key in the gpg
# keyring, or with $GPG_KEY_ID when the keyring holds more than one. The
# key must be RSA (rpm before 4.16 has no EdDSA) and have no passphrase.
#
# Layout:
#   packages/fastcnsl-<ver>-1.x86_64.rpm   signed
#   repodata/                                repomd.xml{,.asc,.key} and the rest
#   fastcnsl.repo                          repo file for /etc/yum.repos.d
#   fastcnsl.asc                           public key, armored
#   index.html                               install instructions
set -euo pipefail

OUT="${1:?usage: rpm-repo.sh <out-dir> <file.rpm>...}"
shift
[[ $# -gt 0 ]] || { echo "error: no .rpm files given" >&2; exit 1; }

URL="${RPM_REPO_URL:-https://andrewpushkin.github.io/fastcnsl/rpm}"
FPR=$(gpg --batch --with-colons --list-secret-keys ${GPG_KEY_ID:-} 2>/dev/null | awk -F: '$1 == "fpr" { print $10; exit }')
[[ -n "$FPR" ]] || { echo "error: no gpg secret key to sign with" >&2; exit 1; }

rm -rf "$OUT"
mkdir -p "$OUT/packages"
cp "$@" "$OUT/packages/"
cd "$OUT"

gpg --batch --armor --export "$FPR" > fastcnsl.asc
# __gpg: Ubuntu's rpm looks for a gpg2 binary that Ubuntu doesn't ship.
rpmsign --addsign \
    --define "__gpg $(command -v gpg)" \
    --define "_gpg_name $FPR" \
    --define "_gpg_sign_cmd_extra_args --batch --pinentry-mode loopback" \
    packages/*.rpm
# Fail here, not on users' machines, if a signature doesn't verify.
DB=$(mktemp -d)
rpm --dbpath "$DB" --import fastcnsl.asc
rpm --dbpath "$DB" --checksig packages/*.rpm
rm -rf "$DB"

createrepo_c --quiet --general-compress-type=gz .
gpg --batch --yes --local-user "$FPR" --armor --detach-sign -o repodata/repomd.xml.asc repodata/repomd.xml
cp fastcnsl.asc repodata/repomd.xml.key

cat > fastcnsl.repo <<EOF
[fastcnsl]
name=fastcnsl
baseurl=$URL
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=$URL/fastcnsl.asc
EOF

VERSION=$(for f in packages/*.rpm; do rpm -qp --qf '%{VERSION}\n' "$f"; done | sort -V | tail -n 1)
cat > index.html <<EOF
<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>fastcnsl rpm repository</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:46rem;margin:2rem auto;padding:0 1rem}pre{background:#8882;padding:1rem;overflow-x:auto}</style>
<h1>fastcnsl rpm repository</h1>
<p>Fedora, RHEL and compatible, openSUSE (x86-64). Latest version: <b>$VERSION</b>.</p>
<p>Fedora, RHEL:</p>
<pre>sudo curl -fsSL -o /etc/yum.repos.d/fastcnsl.repo $URL/fastcnsl.repo
sudo dnf install fastcnsl</pre>
<p>openSUSE:</p>
<pre>sudo zypper addrepo $URL/fastcnsl.repo
sudo zypper refresh
sudo zypper install fastcnsl</pre>
<p>Signing key fingerprint: <code>$FPR</code></p>
<p>Source: <a href="https://github.com/andrewpushkin/fastcnsl">github.com/andrewpushkin/fastcnsl</a></p>
EOF

echo "rpm repository for fastcnsl $VERSION -> $OUT (key $FPR)"
