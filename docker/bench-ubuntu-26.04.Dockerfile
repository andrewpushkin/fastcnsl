# Terminal benchmark image: fastcnsl against Konsole, xterm and the
# Ubuntu terminals (Ptyxis, the default since 25.10, and GNOME Terminal)
# on Ubuntu 26.04. Used by `make bench`; see scripts/bench/run.sh.
#
# Same build dependencies as linux-x86_64.Dockerfile, with Ubuntu's own
# Rust 1.91 instead of the rust: image. fastcnsl is compiled inside the
# container from the mounted source tree (target/bench-ubuntu-26.04), so
# it is linked against this image's glibc and Mesa.
#
# Two display modes (BENCH_DISPLAY):
#   xvfb  virtual X server, software rendering (llvmpipe / lavapipe)
#   host  the host's X server (WSLg :0) with /dev/dxg passed in, so
#         Mesa's d3d12 driver renders on the Windows GPU

FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        # build: Rust 1.91 (rust-version in Cargo.toml) and winit's headers
        cargo-1.91 rustc-1.91 gcc libc6-dev pkg-config \
        libxkbcommon-dev libwayland-dev libx11-dev \
        # fastcnsl at run time: X11 keyboard, Vulkan / GL (Mesa)
        libxkbcommon-x11-0 libx11-xcb1 libxcursor1 libxrandr2 libxi6 \
        libvulkan1 mesa-vulkan-drivers libegl1 libgl1 libgl1-mesa-dri \
        # the terminals compared
        konsole xterm gnome-terminal ptyxis \
        # display, session bus, fonts
        xvfb x11-utils xdotool dbus dbus-x11 at-spi2-core \
        fontconfig fonts-dejavu-core \
        # the harness
        python3 python3-xlib procps coreutils \
    && rm -rf /var/lib/apt/lists/*

ENV PATH=/usr/lib/rust-1.91/bin:$PATH

# CARGO_HOME lives in the shared crate cache (see Makefile); it must be
# writable by the non-root user the container runs as.
ENV CARGO_HOME=/cargo
RUN mkdir -p /cargo && chmod 1777 /cargo

WORKDIR /work
