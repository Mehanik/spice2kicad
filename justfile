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
    # Memory cap (RUST_TEST_MAX_VSZ_KB, default 4 GiB) and thread limit
    # (RUST_TEST_THREADS, default 2) keep a runaway test from OOM-killing
    # the host. Override e.g. `RUST_TEST_MAX_VSZ_KB=8388608 just test`.
    #
    # `--no-fail-fast` is load-bearing, not tidiness: cargo's default aborts
    # at the first failing BINARY, so a red run reports a truncated failure
    # list and the untouched binaries never run at all. A ratchet suite that
    # never executed is indistinguishable from one that passed. This is how
    # the ADR-19 M4 regression stayed invisible (`docs/layout-adr.md`
    # § "M4 reverted" — gate-set lesson).
    bash -c 'ulimit -v ${RUST_TEST_MAX_VSZ_KB:-4194304} && cargo test --workspace --no-fail-fast -- --test-threads=${RUST_TEST_THREADS:-2}'

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
