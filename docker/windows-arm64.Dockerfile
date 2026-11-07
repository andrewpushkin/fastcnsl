# Windows ARM64 release image (Snapdragon / Surface Pro X / Windows on
# Arm VMs). Cross-compiles with llvm-mingw for aarch64-pc-windows-gnullvm.

FROM rust:1.91-bookworm

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        zip unzip xz-utils \
    && rm -rf /var/lib/apt/lists/*

ARG LLVM_MINGW=20261020
RUN curl -fsSL https://github.com/mstorsjo/llvm-mingw/releases/download/${LLVM_MINGW}/llvm-mingw-${LLVM_MINGW}-ucrt-ubuntu-22.04-x86_64.tar.xz \
        | tar -xJ -C /opt \
    && mv /opt/llvm-mingw-${LLVM_MINGW}-ucrt-ubuntu-22.04-x86_64 /opt/llvm-mingw
ENV PATH=/opt/llvm-mingw/bin:$PATH

# aarch64-unknown-linux-musl: ragamuffin-wsl (wsl-relay/), the static Linux
# helper for WSL tabs, linked by llvm-mingw's ld.lld with musl's own crt.
RUN rustup target add aarch64-pc-windows-gnullvm aarch64-unknown-linux-musl

ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=ld.lld \
    CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_LINKER=aarch64-w64-mingw32-clang \
    CC_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang \
    AR_aarch64_pc_windows_gnullvm=llvm-ar

# Microsoft's ConPTY (conpty.dll + OpenConsole.exe), shipped next to the exe.
COPY conpty.sh /usr/local/bin/conpty.sh
RUN sh /usr/local/bin/conpty.sh arm64

ENV CARGO_HOME=/cargo
RUN mkdir -p /cargo && chmod 1777 /cargo
WORKDIR /work
