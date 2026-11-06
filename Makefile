# Every cargo command runs inside Docker. The project directory is mounted
# into the containers, which run as your user, so all output lands on the
# host with your ownership:
#
#   dist/                 release packages and the local build
#   target/<platform>/    cargo output per platform image
#
# Release packages, one Docker image per platform (docker/<platform>.Dockerfile):
#
#   make all              every platform below (macOS only if its SDK is present)
#   make linux-x86_64     .deb + AppImage
#   make deb              only the .deb
#   make appimage         only the AppImage
#   make windows-x86_64   ragamuffin-<ver>-windows-x86_64.zip
#   make windows-arm64    ragamuffin-<ver>-windows-arm64.zip
#   make macos-arm64      ragamuffin-<ver>-macos-arm64.zip (needs docker/macos-sdk/)
#
# Development (Dockerfile.dev):
#
#   make build            release build for this machine -> dist/ragamuffin
#   make run              build, then start it
#   make debug            debug build
#   make test             unit tests
#   make clippy           lints, warnings are errors
#   make fmt              format the code
#   make check-all        type-check every release target
#   make screenshot       run on a virtual display -> dist/screenshot.png
#   make shell            interactive shell in the dev container
#
#   make clean            remove build output
#   make distclean        also remove the Docker images and the cargo cache

CARGO_VOLUME := ragamuffin-cargo
PLATFORMS    := linux-x86_64 windows-x86_64 windows-arm64 macos-arm64
MACOS_SDK    := $(firstword $(wildcard docker/macos-sdk/MacOSX*.sdk.tar.*))

TTY := $(shell [ -t 0 ] && [ -t 1 ] && echo -it)
# $(call docker_run,<image>,<cargo target dir>)
docker_run = docker run --rm $(TTY) \
	-u $(shell id -u):$(shell id -g) \
	-e HOME=/tmp \
	-e CARGO_TARGET_DIR=/work/$(2) \
	-v $(CURDIR):/work \
	-v $(CARGO_VOLUME):/cargo \
	-w /work \
	$(1)

DEV_IMAGE := ragamuffin-dev
DEV_STAMP := target/.image-dev
DEV_RUN    = $(call docker_run,$(DEV_IMAGE),target)

.PHONY: all $(PLATFORMS) deb appimage macos-sdk-missing \
	build run debug test clippy fmt check-all screenshot shell clean distclean

# ---------------------------------------------------------------- releases

all: linux-x86_64 windows-x86_64 windows-arm64 $(if $(MACOS_SDK),macos-arm64,macos-sdk-missing)
	@echo
	@echo "Packages in dist/:"
	@ls -1 dist | grep -v -e '^ragamuffin$$' -e '^screenshot.png$$' | sed 's/^/  /'

# Image per platform, rebuilt when its Dockerfile changes.
target/.image-%: docker/%.Dockerfile
	docker build -f $< -t ragamuffin-$* docker
	@mkdir -p $(dir $@) && touch $@

# macOS: the SDK folder is the build context (see docker/macos-sdk/README.md).
target/.image-macos-arm64: docker/macos-arm64.Dockerfile $(MACOS_SDK)
	@if [ -z "$(MACOS_SDK)" ]; then \
		echo "macOS SDK missing: put MacOSX<ver>.sdk.tar.xz in docker/macos-sdk/ (see README there)"; exit 1; fi
	docker build -f $< -t ragamuffin-macos-arm64 docker/macos-sdk
	@mkdir -p $(dir $@) && touch $@

linux-x86_64: target/.image-linux-x86_64
	$(call docker_run,ragamuffin-linux-x86_64,target/linux-x86_64) scripts/package/linux-x86_64.sh all

deb: target/.image-linux-x86_64
	$(call docker_run,ragamuffin-linux-x86_64,target/linux-x86_64) scripts/package/linux-x86_64.sh deb

appimage: target/.image-linux-x86_64
	$(call docker_run,ragamuffin-linux-x86_64,target/linux-x86_64) scripts/package/linux-x86_64.sh appimage

windows-x86_64: target/.image-windows-x86_64
	$(call docker_run,ragamuffin-windows-x86_64,target/windows-x86_64) scripts/package/windows.sh x86_64

windows-arm64: target/.image-windows-arm64
	$(call docker_run,ragamuffin-windows-arm64,target/windows-arm64) scripts/package/windows.sh arm64

macos-arm64: target/.image-macos-arm64
	$(call docker_run,ragamuffin-macos-arm64,target/macos-arm64) scripts/package/macos-arm64.sh

macos-sdk-missing:
	@echo "SKIPPED macos-arm64: no macOS SDK in docker/macos-sdk/ (see docker/macos-sdk/README.md)"

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

test: $(DEV_STAMP)
	$(DEV_RUN) cargo test

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
	$(DEV_RUN) scripts/screenshot.sh dist/screenshot.png

shell: $(DEV_STAMP)
	$(DEV_RUN) bash

# ------------------------------------------------------------------ cleanup

clean:
	-docker run --rm -u $(shell id -u):$(shell id -g) -v $(CURDIR):/work -w /work $(DEV_IMAGE) rm -rf target dist
	rm -rf target dist

distclean: clean
	-docker image rm $(DEV_IMAGE) $(addprefix ragamuffin-,$(PLATFORMS))
	-docker volume rm $(CARGO_VOLUME)
