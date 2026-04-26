default: check

# Install git pre-commit hook
hooks:
    git config core.hooksPath .githooks
    @echo "Pre-commit hook installed."

# What CI runs
check: fmt-check clippy test

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

# Round-trip functional tests. Requires kicad-cli on PATH (skipped otherwise;
# set REQUIRE_KICAD_CLI=1 to fail-hard instead). Most are #[ignore]d until
# the schematic emitter lands.
test-roundtrip:
    cargo test -p spice2kicad --test roundtrip -- --ignored --nocapture

build:
    cargo build --workspace

run *ARGS:
    cargo run -p spice2kicad -- {{ARGS}}

audit:
    cargo audit

deny:
    cargo deny check
