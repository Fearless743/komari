# Komari Rust backend build helpers.
#
# `make release` injects a version the same way the Go release workflow did:
#   go build -ldflags "-X .../utils.CurrentVersion=$VERSION \
#                      -X .../utils.VersionHash=$VERSION_HASH"
# becomes
#   KOMARI_VERSION=$VERSION KOMARI_VERSION_HASH=$VERSION_HASH scripts/build.sh
#
# For a real web UI, point DEFAULT_THEME_DIR at a normalized theme directory
# (one containing dist/, komari-theme.json and preview.png):
#   make release DEFAULT_THEME_DIR=/path/to/defaultTheme
# Without it, B14's committed placeholder theme is embedded.

SHELL := /bin/bash

CARGO ?= cargo
PACKAGE ?=
PROFILE ?= release
TARGET ?=
CROSS ?=
OUT_DIR ?= dist
DEFAULT_THEME_DIR ?=

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
	@echo ""
	@echo "Variables:"
	@echo "  DEFAULT_THEME_DIR=/path/to/defaultTheme  Embed a real B14 theme"
	@echo "  PACKAGE=komari-cli                     Limit the build (default: workspace)"
	@echo "  TARGET=<triple> CROSS=cross            Cross-compile with cross"

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
	KOMARI_DEFAULT_THEME_DIR="$(if $(DEFAULT_THEME_DIR),$(abspath $(DEFAULT_THEME_DIR)),)" \
	PACKAGE="$(PACKAGE)" \
	PROFILE="$(PROFILE)" \
	TARGET="$(TARGET)" \
	CROSS="$(CROSS)" \
	OUT_DIR="$(OUT_DIR)" \
	./scripts/build.sh

clean:
	$(CARGO) clean
	rm -rf "$(OUT_DIR)"

version:
	@echo "version: $(VERSION)"
	@echo "hash:    $(VERSION_HASH)"
