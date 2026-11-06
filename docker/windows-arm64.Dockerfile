# Windows ARM64 release image (Snapdragon / Surface Pro X / Windows on
# Arm VMs). Cross-compiles with llvm-mingw for aarch64-pc-windows-gnullvm.

FROM rust:1.91-bookworm

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        zip xz-utils \
    && rm -rf /var/lib/apt/lists/*

ARG LLVM_MINGW=20261020
RUN curl -fsSL https://github.com/mstorsjo/llvm-mingw/releases/download/${LLVM_MINGW}/llvm-mingw-${LLVM_MINGW}-ucrt-ubuntu-22.04-x86_64.tar.xz \
        | tar -xJ -C /opt \
    && mv /opt/llvm-mingw-${LLVM_MINGW}-ucrt-ubuntu-22.04-x86_64 /opt/llvm-mingw
ENV PATH=/opt/llvm-mingw/bin:$PATH

RUN rustup target add aarch64-pc-windows-gnullvm

ENV CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_LINKER=aarch64-w64-mingw32-clang \
    CC_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang \
    AR_aarch64_pc_windows_gnullvm=llvm-ar

ENV CARGO_HOME=/cargo
RUN mkdir -p /cargo && chmod 1777 /cargo
WORKDIR /work
