# Every cargo command runs inside Docker. The project directory is mounted
# into the containers, which run as your user, so everything generated is a
# normal file on the host, owned by you:
#
#   dist/                 release packages and the local build
#   target/<platform>/    cargo output per platform image
#   .cache/cargo/         downloaded crates (shared by all images)
#
# Release builds, one Docker image per platform (docker/<platform>.Dockerfile).
# Compiling and packaging are separate targets; a package target compiles
# first if needed.
#
#   make all                      every package below
#
#   make linux-x86_64             compile -> target/linux-x86_64/release/
#   make linux-x86_64-deb         -> dist/fastcnsl_<ver>_amd64.deb
#   make linux-x86_64-rpm         -> dist/fastcnsl-<ver>-1.x86_64.rpm
#   make linux-x86_64-appimage    -> dist/Fastcnsl-<ver>-x86_64.AppImage
#   make windows-x86_64           compile -> target/windows-x86_64/.../fastcnsl.exe
#   make windows-x86_64-zip       -> dist/fastcnsl-<ver>-windows-x86_64.zip (portable)
#   make windows-x86_64-setup     -> dist/fastcnsl-<ver>-windows-x86_64-setup.exe (installer)
#   make windows-arm64            compile -> target/windows-arm64/.../fastcnsl.exe
#   make windows-arm64-zip        -> dist/fastcnsl-<ver>-windows-arm64.zip (portable)
#   make windows-arm64-setup      -> dist/fastcnsl-<ver>-windows-arm64-setup.exe (installer)
#   make macos-arm64              compile -> target/macos-arm64/.../fastcnsl
#   make macos-arm64-dmg          -> dist/fastcnsl-<ver>-macos-arm64.dmg (Fastcnsl.app)
#
# The macOS image downloads the macOS SDK while it is built. Read the
# licensing note at the top of docker/macos-arm64.Dockerfile first.
#
# Development (Dockerfile.dev):
#
#   make build            release build for this machine -> dist/fastcnsl
#   make run              build, then start it
#   make debug            debug build
#   make tests            all unit tests (also: make test)
#   make clippy           lints, warnings are errors
#   make fmt              format the code
#   make check-all        type-check Linux and Windows (macOS: make macos-arm64)
#   make screenshot       run on a virtual display -> dist/screenshots/
#   make shell            interactive shell in the dev container
#
# Benchmark against Konsole, xterm, Ptyxis and GNOME Terminal on Ubuntu
# 26.04 (docker/bench-ubuntu-26.04.Dockerfile, scripts/bench/):
#
#   make bench            on Xvfb, software rendering -> dist/bench/
#   make bench-host       on this machine's display (WSLg) and GPU -> dist/bench-host/
#   make bench BENCH_ARGS="--reps 5 --terminals fastcnsl,xterm --tests ascii"
#
# From WSL, natively on Windows (no Docker except to compile), against
# Windows Terminal and the classic console (scripts/bench/windows/):
#
#   make bench-windows    -> dist/bench-windows/
#   make bench-windows WIN_BENCH_ARGS="-Reps 5 -Terminals fastcnsl,wt"
#
#   make clean            remove build output (target/, dist/), no Docker needed
#   make distclean        also remove the crate cache and the Docker images

CARGO_CACHE  := $(CURDIR)/.cache/cargo
# 0.1.<commits>; see scripts/get_version.sh. Override with make VERSION=...
VERSION      ?= $(shell scripts/get_version.sh)
PLATFORMS    := linux-x86_64 windows-x86_64 windows-arm64 macos-arm64
PACKAGES     := linux-x86_64-deb linux-x86_64-rpm linux-x86_64-appimage windows-x86_64-zip windows-x86_64-setup windows-arm64-zip windows-arm64-setup macos-arm64-dmg
# Optional overrides for the macOS SDK download (see docker/macos-arm64.Dockerfile).
MACOS_SDK_ARGS := $(foreach v,MACOS_SDK_VERSION MACOS_SDK_URL MACOS_SDK_SHA256,$(if $($(v)),--build-arg $(v)=$($(v))))

TTY := $(shell [ -t 0 ] && [ -t 1 ] && echo -it)
# $(call docker_run,<image>,<cargo target dir>)
docker_run = mkdir -p $(CARGO_CACHE) && docker run --rm $(TTY) \
	-u $(shell id -u):$(shell id -g) \
	-e HOME=/tmp \
	-e USER=$(shell id -un) \
	-e SHELL=/bin/bash \
	-e XDG_RUNTIME_DIR=/tmp \
	-e FASTCNSL_VERSION=$(VERSION) \
	-e CARGO_TARGET_DIR=/work/$(2) \
	-v $(CURDIR):/work \
	-v $(CARGO_CACHE):/cargo \
	-w /work \
	$(1)

DEV_IMAGE := fastcnsl-dev
DEV_STAMP := target/.image-dev
DEV_RUN    = $(call docker_run,$(DEV_IMAGE),target)

.PHONY: all $(PLATFORMS) $(PACKAGES) \
	build run debug tests test clippy fmt check-all screenshot shell bench bench-host bench-windows clean distclean

# ---------------------------------------------------------------- releases

all: $(PACKAGES)
	@echo
	@echo "Packages in dist/:"
	@ls -1 dist | grep -v -e '^fastcnsl$$' -e '^screenshots$$' | sed 's/^/  /'

# Image per platform, rebuilt when its Dockerfile changes.
target/.image-%: docker/%.Dockerfile
	docker build -f $< -t fastcnsl-$* docker
	@mkdir -p $(dir $@) && touch $@

# Windows images also download Microsoft's ConPTY with this script.
target/.image-windows-x86_64 target/.image-windows-arm64: docker/conpty.sh

# macOS: same as the pattern rule, plus the optional SDK overrides.
target/.image-macos-arm64: docker/macos-arm64.Dockerfile
	docker build $(MACOS_SDK_ARGS) -f $< -t fastcnsl-macos-arm64 docker
	@mkdir -p $(dir $@) && touch $@

LINUX   = $(call docker_run,fastcnsl-linux-x86_64,target/linux-x86_64) scripts/package/linux-x86_64.sh
WIN_X64 = $(call docker_run,fastcnsl-windows-x86_64,target/windows-x86_64) scripts/package/windows.sh x86_64
WIN_ARM = $(call docker_run,fastcnsl-windows-arm64,target/windows-arm64) scripts/package/windows.sh arm64
MACOS   = $(call docker_run,fastcnsl-macos-arm64,target/macos-arm64) scripts/package/macos-arm64.sh

# Compile only.
linux-x86_64: target/.image-linux-x86_64
	$(LINUX) build
windows-x86_64: target/.image-windows-x86_64
	$(WIN_X64) build
windows-arm64: target/.image-windows-arm64
	$(WIN_ARM) build
macos-arm64: target/.image-macos-arm64
	$(MACOS) build

# Package (compiles first).
linux-x86_64-deb: linux-x86_64
	$(LINUX) deb
linux-x86_64-rpm: linux-x86_64
	$(LINUX) rpm
linux-x86_64-appimage: linux-x86_64
	$(LINUX) appimage
windows-x86_64-zip: windows-x86_64
	$(WIN_X64) zip
windows-arm64-zip: windows-arm64
	$(WIN_ARM) zip
# Installers (Inno Setup under Wine, docker/windows-setup.Dockerfile) from
# the files the zip step staged.
windows-x86_64-setup: windows-x86_64-zip target/.image-windows-setup
	$(call docker_run,fastcnsl-windows-setup,target/windows-x86_64) scripts/package/windows-setup.sh x86_64
windows-arm64-setup: windows-arm64-zip target/.image-windows-setup
	$(call docker_run,fastcnsl-windows-setup,target/windows-arm64) scripts/package/windows-setup.sh arm64
macos-arm64-dmg: macos-arm64
	$(MACOS) dmg

# ------------------------------------------------------------- development

$(DEV_STAMP): Dockerfile.dev
	docker build -f Dockerfile.dev -t $(DEV_IMAGE) docker
	@mkdir -p $(dir $@) && touch $@

build: $(DEV_STAMP)
	$(DEV_RUN) cargo build --release
	@mkdir -p dist && cp target/release/fastcnsl dist/
	@echo "-> dist/fastcnsl"

# Built on Debian bookworm (glibc 2.36): runs on any newer Linux, incl. WSLg.
run: build
	./dist/fastcnsl

debug: $(DEV_STAMP)
	$(DEV_RUN) cargo build

# Unit tests, including GPU drawing (headless Mesa Vulkan in the image) and
# real shells on a PTY. Not run by GitHub Actions.
tests: $(DEV_STAMP)
	$(DEV_RUN) cargo test

test: tests

clippy: $(DEV_STAMP)
	$(DEV_RUN) cargo clippy --all-targets -- -D warnings

fmt: $(DEV_STAMP)
	$(DEV_RUN) cargo fmt

check-all: $(DEV_STAMP)
	$(DEV_RUN) cargo check
	$(DEV_RUN) cargo check --target x86_64-pc-windows-gnu
	$(DEV_RUN) cargo check --target aarch64-pc-windows-gnullvm

screenshot: build
	$(DEV_RUN) scripts/screenshot.sh dist/screenshots

shell: $(DEV_STAMP)
	$(DEV_RUN) bash

# ---------------------------------------------------------------- benchmark

BENCH_IMAGE := fastcnsl-bench-ubuntu-26.04
BENCH_RUN    = $(call docker_run,$(1) $(BENCH_IMAGE),target/bench-ubuntu-26.04) scripts/bench/run.sh $(BENCH_ARGS)
# The host's X display; on WSL also its GPU, through Mesa's d3d12 driver.
# That driver is OpenGL only (Ubuntu's Mesa has no dzn Vulkan), so
# fastcnsl is started with --gpu opengl there.
BENCH_HOST  := -e BENCH_DISPLAY=host -e BENCH_OUT=dist/bench-host -e DISPLAY=$(DISPLAY) \
	-v /tmp/.X11-unix:/tmp/.X11-unix \
	$(if $(wildcard /dev/dxg),--device /dev/dxg -v /usr/lib/wsl:/usr/lib/wsl:ro \
		-e LD_LIBRARY_PATH=/usr/lib/wsl/lib -e GALLIUM_DRIVER=d3d12 -e FASTCNSL_GPU=opengl)

bench: target/.image-bench-ubuntu-26.04
	$(call BENCH_RUN,)

bench-host: target/.image-bench-ubuntu-26.04
	$(call BENCH_RUN,$(BENCH_HOST))

bench-windows: windows-x86_64
	scripts/bench/windows.sh $(WIN_BENCH_ARGS)

# ------------------------------------------------------------------ cleanup

clean:
	rm -rf target dist

distclean: clean
	rm -rf .cache
	-docker image rm $(DEV_IMAGE) $(addprefix fastcnsl-,$(PLATFORMS)) fastcnsl-windows-setup
