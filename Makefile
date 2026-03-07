BIN     := rust-code
CARGO   := cargo
RELEASE := target/release/$(BIN)

.PHONY: build release test run lint check audit-deps audit-loc audit clean install help

## Build debug binary
build:
	$(CARGO) build

## Build optimized release binary
release:
	$(CARGO) build --release -p $(BIN)

## Run all tests (whole workspace)
test:
	$(CARGO) test --workspace

## Run headless with PROMPT
run: build
	$(CARGO) run -- -p "$(PROMPT)"

## Clippy lint (baml-agent clean, rc-cli has legacy warnings)
lint:
	$(CARGO) clippy -p baml-agent -p baml-agent-tui --all-targets -- -D warnings

## Format check (skip rc-baml/rc-cli which contain generated baml_client)
fmt-check:
	$(CARGO) fmt -p baml-agent -p baml-agent-tui -- --check

## Format fix
fmt:
	$(CARGO) fmt -p baml-agent -p baml-agent-tui

## Pre-commit check (test + lint + fmt)
check: test lint fmt-check

## Audit: unused deps (requires: cargo install cargo-machete)
audit-deps:
	@echo "=== Unused dependencies ==="
	@cargo machete 2>&1 || true

## Audit: large files (>800 LOC)
audit-loc:
	@echo "=== Files >800 LOC ==="
	@tokei crates/ --types Rust --files 2>&1 | grep -E "\.rs\s+[0-9]" | awk '$$2 > 800 {print $$2, $$1}' | sort -rn || true

## Full audit
audit: audit-deps audit-loc

## Install binary
install: release
	strip $(RELEASE)
	cp $(RELEASE) /usr/local/bin/$(BIN)
	@echo "Installed to /usr/local/bin/$(BIN)"

## Clean build artifacts
clean:
	$(CARGO) clean

## Show this help
help:
	@grep -E '^## ' Makefile | sed 's/## /  /'
