# Windows installer image: Inno Setup's compiler (ISCC.exe) under Wine.
# It only packages files that are already built (scripts/package/windows.sh
# stage), so one image serves x86_64 and arm64. Run by
# scripts/package/windows-setup.sh.
#
# Inno Setup 7's x86 (32-bit) edition, hence wine32: the x64 edition's
# installer checks the processor with an API Wine 8 lacks and refuses to run
# ("does not support the version of Windows"). Both editions build the same
# installers. It is installed into a throwaway Wine prefix while the image
# is built and only its folder is kept: containers run as the host user,
# and Wine refuses a prefix owned by someone else, so each run makes a fresh
# one (scripts/package/windows-setup.sh).

FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive
RUN dpkg --add-architecture i386 \
    && apt-get update && apt-get install -y --no-install-recommends \
        wine wine32 wine64 xvfb xauth ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# No Mono/Gecko install prompts; no Wine debug chatter.
ENV WINEDEBUG=-all \
    WINEDLLOVERRIDES="mscoree,mshtml="

# Inno Setup (free; jrsoftware.org), from its GitHub release. The file is
# Authenticode-signed by Pyrsys B.V. (Inno Setup's maintainers); a new
# version needs a new checksum.
ARG INNO_VERSION=7.1.1
ARG INNO_SHA256=03446e46f5da932e408881168c01afc59cc667cb5bc927d839c5526ace5cc0d3
RUN set -eu; \
    tag="is-$(echo "$INNO_VERSION" | tr . _)"; \
    curl -fsSL --retry 3 -o /tmp/innosetup.exe \
        "https://github.com/jrsoftware/issrc/releases/download/$tag/innosetup-$INNO_VERSION-x86.exe"; \
    echo "$INNO_SHA256  /tmp/innosetup.exe" | sha256sum -c -; \
    export WINEPREFIX=/tmp/wineprefix; \
    xvfb-run -a wine /tmp/innosetup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- \
        /CURRENTUSER '/DIR=C:\inno'; \
    wineserver -w; \
    test -f /tmp/wineprefix/drive_c/inno/ISCC.exe; \
    cp -r /tmp/wineprefix/drive_c/inno /opt/inno; \
    chmod -R a+rX /opt/inno; \
    rm -rf /tmp/wineprefix /tmp/innosetup.exe

WORKDIR /work
