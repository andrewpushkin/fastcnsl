# macOS Apple Silicon release image: downloads the macOS SDK, builds
# osxcross (Apple's clang/ld64 port for Linux), adds the Rust target and an
# ad-hoc code signer. The first build takes ~10 minutes; after that the
# layers are cached and only rebuild when this file changes.
#
# Licensing: the macOS SDK is Apple software under the Xcode and Apple SDKs
# Agreement, which limits use to Apple-branded hardware and forbids
# redistribution. The tarball below comes from a third-party mirror
# (github.com/joseluisq/macosx-sdks), not from Apple. Whoever builds this
# image is responsible for having the right to use the SDK. Don't push the
# resulting image to a public registry. To use an SDK you packaged
# yourself on a Mac (osxcross tools/gen_sdk_package.sh), serve it over
# HTTP and run:
#   make macos-arm64 MACOS_SDK_VERSION=.. MACOS_SDK_URL=.. MACOS_SDK_SHA256=..

# trixie: clang 19 understands the libc++ headers of recent SDKs
# (bookworm's clang 14 fails osxcross's self-test with SDK 15.x).
FROM rust:1.91-trixie

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        clang lld llvm cmake make patch python3 xz-utils bzip2 cpio \
        libxml2-dev libssl-dev zlib1g-dev uuid-dev \
        xorriso imagemagick \
    && rm -rf /var/lib/apt/lists/*

ARG OSXCROSS_COMMIT=27d21e4977c9751d01199c7a226a6faf494c3dd9
RUN curl -fsSL https://github.com/tpoechtrager/osxcross/archive/${OSXCROSS_COMMIT}.tar.gz | tar -xz -C /opt \
    && mv /opt/osxcross-${OSXCROSS_COMMIT} /opt/osxcross

ARG MACOS_SDK_VERSION=15.5
ARG MACOS_SDK_URL=https://github.com/joseluisq/macosx-sdks/releases/download/${MACOS_SDK_VERSION}/MacOSX${MACOS_SDK_VERSION}.sdk.tar.xz
ARG MACOS_SDK_SHA256=c15cf0f3f17d714d1aa5a642da8e118db53d79429eb015771ba816aa7c6c1cbd
RUN curl -fsSL -o /opt/osxcross/tarballs/MacOSX${MACOS_SDK_VERSION}.sdk.tar.xz "${MACOS_SDK_URL}" \
    && echo "${MACOS_SDK_SHA256}  /opt/osxcross/tarballs/MacOSX${MACOS_SDK_VERSION}.sdk.tar.xz" | sha256sum -c -

RUN cd /opt/osxcross && UNATTENDED=1 JOBS=$(nproc) ./build.sh \
    && rm -rf build tarballs/*

# The wrappers find ld64 and friends through PATH.
ENV PATH=/opt/osxcross/target/bin:$PATH

# osxcross names its tools after the SDK's Darwin version
# (aarch64-apple-darwin24.5-clang ...) and reads that version from the name
# it is invoked as, so a plain symlink doesn't work. Small wrappers give the
# tools stable names.
RUN cd /opt/osxcross/target/bin \
    && for t in clang clang++ ar ranlib; do \
         src=$(ls aarch64-apple-darwin*-$t 2>/dev/null | head -1); \
         [ -n "$src" ] || src=$(ls arm64-apple-darwin*-$t | head -1); \
         printf '#!/bin/sh\nexec %s "$@"\n' "$PWD/$src" > /usr/local/bin/aarch64-apple-darwin-$t; \
         chmod +x /usr/local/bin/aarch64-apple-darwin-$t; \
       done \
    && echo 'int main(void){return 0;}' > /tmp/t.c \
    && aarch64-apple-darwin-clang -o /tmp/t /tmp/t.c && rm /tmp/t /tmp/t.c
ENV CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=aarch64-apple-darwin-clang \
    CC_aarch64_apple_darwin=aarch64-apple-darwin-clang \
    CXX_aarch64_apple_darwin=aarch64-apple-darwin-clang++ \
    AR_aarch64_apple_darwin=aarch64-apple-darwin-ar \
    MACOSX_DEPLOYMENT_TARGET=11.0

RUN rustup target add aarch64-apple-darwin

# rcodesign: ad-hoc signing (Apple Silicon refuses to run unsigned code).
ARG RCODESIGN=0.29.0
RUN curl -fsSL https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign%2F${RCODESIGN}/apple-codesign-${RCODESIGN}-x86_64-unknown-linux-musl.tar.gz \
        | tar -xz -C /tmp \
    && mv /tmp/apple-codesign-${RCODESIGN}-x86_64-unknown-linux-musl/rcodesign /usr/local/bin/ \
    && rm -rf /tmp/apple-codesign-*

ENV CARGO_HOME=/cargo
RUN mkdir -p /cargo && chmod 1777 /cargo
WORKDIR /work
