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
#   make linux-x86_64-deb         -> dist/ragamuffin_<ver>_amd64.deb
#   make linux-x86_64-rpm         -> dist/ragamuffin-<ver>-1.x86_64.rpm
#   make linux-x86_64-appimage    -> dist/Ragamuffin-<ver>-x86_64.AppImage
#   make windows-x86_64           compile -> target/windows-x86_64/.../ragamuffin.exe
#   make windows-x86_64-zip       -> dist/ragamuffin-<ver>-windows-x86_64.zip
#   make windows-arm64            compile -> target/windows-arm64/.../ragamuffin.exe
#   make windows-arm64-zip        -> dist/ragamuffin-<ver>-windows-arm64.zip
#   make macos-arm64              compile -> target/macos-arm64/.../ragamuffin
#   make macos-arm64-zip          -> dist/ragamuffin-<ver>-macos-arm64.zip (Ragamuffin.app)
#
# The macOS image downloads the macOS SDK while it is built. Read the
# licensing note at the top of docker/macos-arm64.Dockerfile first.
#
# Development (Dockerfile.dev):
#
#   make build            release build for this machine -> dist/ragamuffin
#   make run              build, then start it
#   make debug            debug build
#   make tests            all unit tests (also: make test)
#   make clippy           lints, warnings are errors
#   make fmt              format the code
#   make check-all        type-check every release target
#   make screenshot       run on a virtual display -> dist/screenshots/
#   make shell            interactive shell in the dev container
#
#   make clean            remove build output (target/, dist/), no Docker needed
#   make distclean        also remove the crate cache and the Docker images

CARGO_CACHE  := $(CURDIR)/.cache/cargo
# 0.1.<commits>; see scripts/version.sh. Override with make VERSION=...
VERSION      ?= $(shell scripts/version.sh)
PLATFORMS    := linux-x86_64 windows-x86_64 windows-arm64 macos-arm64
PACKAGES     := linux-x86_64-deb linux-x86_64-rpm linux-x86_64-appimage windows-x86_64-zip windows-arm64-zip macos-arm64-zip
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
	-e RAGAMUFFIN_VERSION=$(VERSION) \
	-e CARGO_TARGET_DIR=/work/$(2) \
	-v $(CURDIR):/work \
	-v $(CARGO_CACHE):/cargo \
	-w /work \
	$(1)

DEV_IMAGE := ragamuffin-dev
DEV_STAMP := target/.image-dev
DEV_RUN    = $(call docker_run,$(DEV_IMAGE),target)

.PHONY: all $(PLATFORMS) $(PACKAGES) \
	build run debug tests test clippy fmt check-all screenshot shell clean distclean

# ---------------------------------------------------------------- releases

all: $(PACKAGES)
	@echo
	@echo "Packages in dist/:"
	@ls -1 dist | grep -v -e '^ragamuffin$$' -e '^screenshots$$' | sed 's/^/  /'

# Image per platform, rebuilt when its Dockerfile changes.
target/.image-%: docker/%.Dockerfile
	docker build -f $< -t ragamuffin-$* docker
	@mkdir -p $(dir $@) && touch $@

# macOS: same as the pattern rule, plus the optional SDK overrides.
target/.image-macos-arm64: docker/macos-arm64.Dockerfile
	docker build $(MACOS_SDK_ARGS) -f $< -t ragamuffin-macos-arm64 docker
	@mkdir -p $(dir $@) && touch $@

LINUX   = $(call docker_run,ragamuffin-linux-x86_64,target/linux-x86_64) scripts/package/linux-x86_64.sh
WIN_X64 = $(call docker_run,ragamuffin-windows-x86_64,target/windows-x86_64) scripts/package/windows.sh x86_64
WIN_ARM = $(call docker_run,ragamuffin-windows-arm64,target/windows-arm64) scripts/package/windows.sh arm64
MACOS   = $(call docker_run,ragamuffin-macos-arm64,target/macos-arm64) scripts/package/macos-arm64.sh

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
macos-arm64-zip: macos-arm64
	$(MACOS) zip

# ------------------------------------------------------------- development

$(DEV_STAMP): Dockerfile.dev
	docker build -f Dockerfile.dev -t $(DEV_IMAGE) docker
	@mkdir -p $(dir $@) && touch $@

build: $(DEV_STAMP)
	$(DEV_RUN) cargo build --release
	@mkdir -p dist && cp target/release/ragamuffin dist/
	@echo "-> dist/ragamuffin"

# Built on Debian bookworm (glibc 2.36): runs on any newer Linux, incl. WSLg.
run: build
	./dist/ragamuffin

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
	$(DEV_RUN) cargo check --target aarch64-apple-darwin

screenshot: build
	$(DEV_RUN) scripts/screenshot.sh dist/screenshots

shell: $(DEV_STAMP)
	$(DEV_RUN) bash

# ------------------------------------------------------------------ cleanup

clean:
	rm -rf target dist

distclean: clean
	rm -rf .cache
	-docker image rm $(DEV_IMAGE) $(addprefix ragamuffin-,$(PLATFORMS))
