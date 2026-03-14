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

## Run all tests (our crates + genai gemini tests)
test:
	$(CARGO) test -p sgr-agent --features "agent search" -p sgr-agent-tui -p rust-code -p solograph
	@echo "=== genai gemini tests ==="
	$(CARGO) test -p genai --test tests_p_gemini 2>&1 || echo "(genai gemini tests skipped — no GEMINI_API_KEY?)"

## Run headless with PROMPT
run: build
	$(CARGO) run -- -p "$(PROMPT)"

## Clippy lint (sgr-agent + sgr-agent-tui clean, rc-cli has legacy warnings)
lint:
	$(CARGO) clippy -p sgr-agent -p sgr-agent-tui -p solograph --all-targets -- -D warnings

## Format check
fmt-check:
	$(CARGO) fmt -p sgr-agent -p sgr-agent-tui -p rust-code -p solograph -- --check

## Format fix
fmt:
	$(CARGO) fmt -p sgr-agent -p sgr-agent-tui -p rust-code -p solograph

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
