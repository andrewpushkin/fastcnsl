#!/bin/sh
# Downloads Microsoft's ConPTY for one architecture into /opt/conpty:
# conpty.dll and OpenConsole.exe from the Microsoft.Windows.Console.ConPTY
# NuGet package (github.com/microsoft/terminal, MIT). Shipped next to
# fastcnsl.exe, alacritty_terminal loads it instead of the console host
# built into Windows: the same one Windows Terminal uses, about 7x faster
# for large output (36 MB cat: 3.4 s -> 0.48 s).
#   conpty.sh x64|arm64
set -eu
ARCH="${1:?usage: conpty.sh x64|arm64}"
VERSION=1.25.260930003
SHA256=02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee
URL="https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$VERSION/microsoft.windows.console.conpty.$VERSION.nupkg"

tmp=$(mktemp -d)
curl -fsSL -o "$tmp/conpty.nupkg" "$URL"
echo "$SHA256  $tmp/conpty.nupkg" | sha256sum -c -
mkdir -p /opt/conpty
unzip -j -o "$tmp/conpty.nupkg" \
    "runtimes/win-$ARCH/native/conpty.dll" \
    "build/native/runtimes/$ARCH/OpenConsole.exe" \
    -d /opt/conpty
echo "$VERSION" > /opt/conpty/VERSION
rm -rf "$tmp"
