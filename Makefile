# Every cargo command runs inside Docker. The project directory is mounted
# into the containers, which run as your user, so everything generated is a
# normal file on the host, owned by you:
#
#   dist/                 release packages and the local build
#   target/<platform>/    cargo output per platform image
#   .cache/cargo/         downloaded crates (shared by all images)
#
# Release packages, one Docker image per platform (docker/<platform>.Dockerfile):
#
#   make all              every platform below
#   make linux-x86_64     .deb + AppImage
#   make deb              only the .deb
#   make appimage         only the AppImage
#   make windows-x86_64   ragamuffin-<ver>-windows-x86_64.zip
#   make windows-arm64    ragamuffin-<ver>-windows-arm64.zip
#   make macos-arm64      ragamuffin-<ver>-macos-arm64.zip
#
# The macOS image downloads the macOS SDK while it is built. Read the
# licensing note at the top of docker/macos-arm64.Dockerfile first.
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
#   make screenshot       run on a virtual display -> dist/screenshots/
#   make shell            interactive shell in the dev container
#
#   make clean            remove build output (target/, dist/), no Docker needed
#   make distclean        also remove the crate cache and the Docker images

CARGO_CACHE  := $(CURDIR)/.cache/cargo
PLATFORMS    := linux-x86_64 windows-x86_64 windows-arm64 macos-arm64
# Optional overrides for the macOS SDK download (see docker/macos-arm64.Dockerfile).
MACOS_SDK_ARGS := $(foreach v,MACOS_SDK_VERSION MACOS_SDK_URL MACOS_SDK_SHA256,$(if $($(v)),--build-arg $(v)=$($(v))))

TTY := $(shell [ -t 0 ] && [ -t 1 ] && echo -it)
# $(call docker_run,<image>,<cargo target dir>)
docker_run = mkdir -p $(CARGO_CACHE) && docker run --rm $(TTY) \
	-u $(shell id -u):$(shell id -g) \
	-e HOME=/tmp \
	-e CARGO_TARGET_DIR=/work/$(2) \
	-v $(CURDIR):/work \
	-v $(CARGO_CACHE):/cargo \
	-w /work \
	$(1)

DEV_IMAGE := ragamuffin-dev
DEV_STAMP := target/.image-dev
DEV_RUN    = $(call docker_run,$(DEV_IMAGE),target)

.PHONY: all $(PLATFORMS) deb appimage \
	build run debug test clippy fmt check-all screenshot shell clean distclean

# ---------------------------------------------------------------- releases

all: $(PLATFORMS)
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
	$(DEV_RUN) scripts/screenshot.sh dist/screenshots

shell: $(DEV_STAMP)
	$(DEV_RUN) bash

# ------------------------------------------------------------------ cleanup

clean:
	rm -rf target dist

distclean: clean
	rm -rf .cache
	-docker image rm $(DEV_IMAGE) $(addprefix ragamuffin-,$(PLATFORMS))
