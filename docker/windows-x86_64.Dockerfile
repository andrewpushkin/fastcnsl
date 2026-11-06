# Windows x86-64 release image: cross-compiles ragamuffin.exe with the
# MinGW-w64 toolchain (x86_64-pc-windows-gnu). The result only imports
# system DLLs, so it runs on a stock Windows 10/11.

FROM rust:1.91-bookworm

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        mingw-w64 zip binutils-mingw-w64-x86-64 \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-pc-windows-gnu

ENV CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc

ENV CARGO_HOME=/cargo
RUN mkdir -p /cargo && chmod 1777 /cargo
WORKDIR /work
