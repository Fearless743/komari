# Komari Rust backend build helpers.
#
# `make release` injects a version the same way the Go release workflow did:
#   go build -ldflags "-X .../utils.CurrentVersion=$VERSION \
#                      -X .../utils.VersionHash=$VERSION_HASH"
# becomes
#   KOMARI_VERSION=$VERSION KOMARI_VERSION_HASH=$VERSION_HASH scripts/build.sh

SHELL := /bin/bash

CARGO ?= cargo
PACKAGE ?= komari-cli
PROFILE ?= release
TARGET ?=
OUT_DIR ?= dist

VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo dev)
VERSION_HASH ?= $(shell git rev-parse HEAD 2>/dev/null || echo unknown)

.PHONY: all help fmt fmt-check clippy test check build release clean version

all: check

help:
	@echo "Targets:"
	@echo "  make fmt        Format the workspace"
	@echo "  make fmt-check  Check formatting without changing files"
	@echo "  make clippy     Run clippy with warnings denied"
	@echo "  make test       Run the workspace test suite"
	@echo "  make check      fmt-check + clippy + test"
	@echo "  make build      Debug workspace build"
	@echo "  make release    Release build with version injection into $(OUT_DIR)/"
	@echo "  make clean      Remove build output"
	@echo "  make version    Print the version that would be injected"

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

test:
	$(CARGO) test --workspace --locked

check: fmt-check clippy test

build:
	$(CARGO) build --workspace --locked

release:
	KOMARI_VERSION="$(VERSION)" \
	KOMARI_VERSION_HASH="$(VERSION_HASH)" \
	PACKAGE="$(PACKAGE)" \
	PROFILE="$(PROFILE)" \
	TARGET="$(TARGET)" \
	OUT_DIR="$(OUT_DIR)" \
	./scripts/build.sh

clean:
	$(CARGO) clean
	rm -rf "$(OUT_DIR)"

version:
	@echo "version: $(VERSION)"
	@echo "hash:    $(VERSION_HASH)"
