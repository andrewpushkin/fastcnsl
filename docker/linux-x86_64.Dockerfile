# Linux x86-64 release image: builds the .deb, the .rpm and the AppImage.
#
# Debian bullseye (glibc 2.31) so the binary runs on older distributions
# too (Ubuntu 20.04+, Debian 11+, Fedora 32+). winit, wgpu and the
# clipboard load X11/Wayland/Vulkan libraries at runtime with dlopen, so
# nothing beyond glibc is linked.

FROM rust:1.91-bullseye

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libxkbcommon-dev libwayland-dev libx11-dev \
        file squashfs-tools desktop-file-utils \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-deb --version 3.8.0 --locked \
    && cargo install cargo-generate-rpm --version 0.21.1 --locked

# appimagetool is itself an AppImage; FUSE isn't available in containers,
# so it is extracted once here. The runtime is pinned and passed with
# --runtime-file, so packaging never touches the network.
ARG APPIMAGETOOL=1.9.1
ARG APPIMAGE_RUNTIME=20251108
RUN cd /opt \
    && curl -fsSL -o appimagetool.AppImage \
        https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL}/appimagetool-x86_64.AppImage \
    && chmod +x appimagetool.AppImage \
    && ./appimagetool.AppImage --appimage-extract >/dev/null \
    && mv squashfs-root appimagetool && rm appimagetool.AppImage \
    && ln -s /opt/appimagetool/AppRun /usr/local/bin/appimagetool \
    && curl -fsSL -o /opt/appimage-runtime-x86_64 \
        https://github.com/AppImage/type2-runtime/releases/download/${APPIMAGE_RUNTIME}/runtime-x86_64

ENV CARGO_HOME=/cargo
RUN mkdir -p /cargo && chmod 1777 /cargo
WORKDIR /work
