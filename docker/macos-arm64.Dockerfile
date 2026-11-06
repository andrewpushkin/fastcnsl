# macOS Apple Silicon release image, using osxcross.
#
# Apple's license doesn't allow redistributing the macOS SDK, so it is not
# downloaded here. Package it on a Mac once (see docker/macos-sdk/README.md)
# and drop MacOSX<version>.sdk.tar.xz into docker/macos-sdk/; that folder
# is the build context of this image.

FROM rust:1.91-bookworm

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        clang lld llvm cmake make patch python3 xz-utils bzip2 cpio \
        libxml2-dev libssl-dev zlib1g-dev uuid-dev \
        zip imagemagick icnsutils \
    && rm -rf /var/lib/apt/lists/*

ARG OSXCROSS_COMMIT=27d21e4977c9751d01199c7a226a6faf494c3dd9
RUN curl -fsSL https://github.com/tpoechtrager/osxcross/archive/${OSXCROSS_COMMIT}.tar.gz | tar -xz -C /opt \
    && mv /opt/osxcross-${OSXCROSS_COMMIT} /opt/osxcross

COPY MacOSX*.sdk.tar.* /opt/osxcross/tarballs/
RUN cd /opt/osxcross && UNATTENDED=1 ./build.sh && rm -rf build tarballs/*

# osxcross names its tools after the SDK's Darwin version
# (aarch64-apple-darwin24.4-clang ...); give them stable names.
RUN cd /opt/osxcross/target/bin \
    && for t in clang clang++ ar ranlib; do \
         src=$(ls aarch64-apple-darwin*-$t 2>/dev/null | head -1); \
         [ -n "$src" ] || src=$(ls arm64-apple-darwin*-$t | head -1); \
         ln -sf "$PWD/$src" /usr/local/bin/aarch64-apple-darwin-$t; \
       done
ENV PATH=/opt/osxcross/target/bin:$PATH \
    CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=aarch64-apple-darwin-clang \
    CC_aarch64_apple_darwin=aarch64-apple-darwin-clang \
    CXX_aarch64_apple_darwin=aarch64-apple-darwin-clang++ \
    AR_aarch64_apple_darwin=aarch64-apple-darwin-ar \
    MACOSX_DEPLOYMENT_TARGET=11.0

RUN rustup target add aarch64-apple-darwin

# rcodesign: ad-hoc signing (Apple Silicon refuses unsigned binaries).
ARG RCODESIGN=0.29.0
RUN curl -fsSL https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign%2F${RCODESIGN}/apple-codesign-${RCODESIGN}-x86_64-unknown-linux-musl.tar.gz \
        | tar -xz -C /tmp \
    && mv /tmp/apple-codesign-${RCODESIGN}-x86_64-unknown-linux-musl/rcodesign /usr/local/bin/ \
    && rm -rf /tmp/apple-codesign-*

ENV CARGO_HOME=/cargo
RUN mkdir -p /cargo && chmod 1777 /cargo
WORKDIR /work
