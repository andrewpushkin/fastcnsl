# All cargo commands run inside the development container (Dockerfile.dev).
#
# The project directory is mounted into the container and the container runs
# as your user, so everything it builds lands on the host with your ownership:
#   target/...        cargo's normal output
#   dist/ragamuffin   the release binary, copied by `make` / `make build`
#
#   make            release build (same as `make all`)
#   make debug      debug build
#   make test       unit tests
#   make clippy     lints, warnings are errors
#   make fmt        format the code
#   make check-all  type-check Linux, Windows and macOS targets
#   make windows    cross-compile dist/ragamuffin.exe
#   make run        build, then start dist/ragamuffin on this machine
#   make screenshot run it on a virtual display, save dist/screenshot.png
#   make shell      interactive shell inside the container
#   make clean      remove build output
#   make distclean  also remove the image and the cargo cache volume

IMAGE        := ragamuffin-dev
CARGO_VOLUME := ragamuffin-cargo
STAMP        := target/.dev-image-stamp

TTY := $(shell [ -t 0 ] && [ -t 1 ] && echo -it)
DOCKER_RUN := docker run --rm $(TTY) \
	-u $(shell id -u):$(shell id -g) \
	-e HOME=/tmp \
	-v $(CURDIR):/work \
	-v $(CARGO_VOLUME):/cargo \
	-w /work \
	$(IMAGE)

.PHONY: all build debug test clippy fmt check-all windows run screenshot shell image clean distclean

all: build

# Rebuild the image when Dockerfile.dev changes.
$(STAMP): Dockerfile.dev
	docker build -f Dockerfile.dev -t $(IMAGE) .
	@mkdir -p $(dir $@) && touch $@

image: $(STAMP)

build: $(STAMP)
	$(DOCKER_RUN) cargo build --release
	@mkdir -p dist && cp target/release/ragamuffin dist/
	@echo "-> dist/ragamuffin"

debug: $(STAMP)
	$(DOCKER_RUN) cargo build

test: $(STAMP)
	$(DOCKER_RUN) cargo test

clippy: $(STAMP)
	$(DOCKER_RUN) cargo clippy --all-targets -- -D warnings

fmt: $(STAMP)
	$(DOCKER_RUN) cargo fmt

check-all: $(STAMP)
	$(DOCKER_RUN) cargo check
	$(DOCKER_RUN) cargo check --target x86_64-pc-windows-gnu
	$(DOCKER_RUN) cargo check --target x86_64-apple-darwin

windows: $(STAMP)
	$(DOCKER_RUN) cargo build --release --target x86_64-pc-windows-gnu
	@mkdir -p dist && cp target/x86_64-pc-windows-gnu/release/ragamuffin.exe dist/
	@echo "-> dist/ragamuffin.exe"

# The binary is built in Debian bookworm (glibc 2.36), so it runs on any
# newer Linux host, including WSLg.
run: build
	./dist/ragamuffin

screenshot: build
	$(DOCKER_RUN) scripts/screenshot.sh dist/screenshot.png

shell: $(STAMP)
	$(DOCKER_RUN) bash

clean: $(STAMP)
	$(DOCKER_RUN) cargo clean
	rm -rf dist

distclean:
	-docker run --rm -v $(CURDIR):/work -w /work $(IMAGE) rm -rf target dist
	-docker image rm $(IMAGE)
	-docker volume rm $(CARGO_VOLUME)
