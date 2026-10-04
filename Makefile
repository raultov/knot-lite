.PHONY: all fmt fmt-check clippy test test-lib dupes dupes-cleanup check check-all e2e build install help

# Crate the quality gates operate on. The knot-lite root crate exists as of
# Phase 1, so the default is the repository root. The Phase 0 spike crate (a
# standalone [workspace]) is still gated with
# `make check CRATE_DIR=docs/spikes/phase0`.
CRATE_DIR ?= $(if $(wildcard Cargo.toml),.,docs/spikes/phase0)
MANIFEST  := --manifest-path $(CRATE_DIR)/Cargo.toml

# cargo-dupes reads dupes.toml ONLY from the analysed path (no parent lookup)
# and silently passes when it finds none, so the gate refuses to run without it.
DUPES_CFG := $(CRATE_DIR)/dupes.toml

# Default target: run all mandatory quality gates
all: check

# Auto-fix code formatting
fmt:
	cargo fmt $(MANIFEST)

# Verify code formatting
fmt-check:
	cargo fmt $(MANIFEST) -- --check

# Run linter checks
clippy:
	cargo clippy $(MANIFEST) --all-targets --all-features -- -D warnings

# Run every test target `cargo test` runs: library unit tests, unit tests in
# binaries, integration tests under tests/ and doctests. The gate must cover
# exactly what `cargo test` covers, so nothing passes locally that the
# validator would fail.
test:
	cargo test $(MANIFEST) --all-features

# Fast inner loop: library unit tests only (not part of `make check`)
test-lib:
	cargo test $(MANIFEST) --lib --all-features

# Check for code duplication (requires cargo-dupes)
dupes:
	@test -x "$$(command -v cargo-dupes)" || (echo "cargo-dupes not found. Installing..." && cargo install cargo-dupes --version 0.2.1 --locked)
	@test -f "$(DUPES_CFG)" || (echo "ERROR: $(DUPES_CFG) missing; cargo-dupes would silently pass without thresholds" && exit 1)
	cargo dupes --path $(CRATE_DIR) check

# Show stale duplication suppressions
dupes-cleanup:
	@test -x "$$(command -v cargo-dupes)" || (echo "cargo-dupes not found. Installing..." && cargo install cargo-dupes --version 0.2.1 --locked)
	cargo dupes --path $(CRATE_DIR) cleanup --dry-run

# Run all local quality gates sequentially (fmt, clippy, all tests, dupes)
check: fmt-check clippy test dupes

# Build release binaries
build:
	cargo build $(MANIFEST) --release --all-features

# Detect OS binary extension
ifeq ($(OS),Windows_NT)
    EXT := .exe
else
    EXT :=
endif

# Install release binaries to CARGO_HOME/bin (defaults to ~/.cargo/bin)
CARGO_HOME ?= $(HOME)/.cargo
DESTDIR ?= $(CARGO_HOME)/bin

# knot-lite binaries (created in Phase 1). The Phase 0 spike binaries are
# measurement tools and are deliberately not installable.
BINS := knot-lite knot-lite-mcp

install: build
	@test "$(CRATE_DIR)" = "." || (echo "install: no knot-lite binaries before Phase 1 (CRATE_DIR=$(CRATE_DIR))" && exit 1)
	@mkdir -p "$(DESTDIR)"
	@for b in $(BINS); do cp -f "target/release/$$b$(EXT)" "$(DESTDIR)/$$b$(EXT)"; done
	@echo "Installed binaries to $(DESTDIR):"
	@for b in $(BINS); do echo "  - $$b$(EXT)"; done

# Run E2E integration tests (no databases needed). The suite arrives in
# Phase 7; until then the target reports that it was skipped.
E2E_SCRIPT ?= tests/run_all_e2e.sh
e2e:
	@if [ -x "$(E2E_SCRIPT)" ]; then ./$(E2E_SCRIPT); else echo "e2e: $(E2E_SCRIPT) not present yet - skipped"; fi

# Run all local quality gates AND all E2E integration tests
check-all: check e2e

# Display available make targets
help:
	@echo "Available targets (CRATE_DIR=$(CRATE_DIR)):"
	@echo "  make (or make all) - Default target: alias for 'make check'"
	@echo "  make check         - Run all local quality gates (fmt-check, clippy, test, dupes)"
	@echo "  make test-lib      - Run library unit tests only (fast inner loop, not a gate)"
	@echo "  make check-all     - Run all local quality gates AND E2E integration tests"
	@echo "  make fmt           - Auto-fix code formatting with cargo fmt"
	@echo "  make fmt-check     - Verify code formatting with cargo fmt -- --check"
	@echo "  make clippy        - Run linter checks with cargo clippy"
	@echo "  make test          - Run all tests (unit, binaries, integration, doctests)"
	@echo "  make dupes         - Run code duplication check with cargo-dupes (auto-installs if missing)"
	@echo "  make dupes-cleanup - Show stale duplication suppressions"
	@echo "  make build         - Build release binaries"
	@echo "  make install       - Build release binaries and copy them to ~/.cargo/bin (Phase 1+)"
	@echo "  make e2e           - Run all E2E integration tests (Phase 7+, skipped before)"
