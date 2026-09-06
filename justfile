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

# The STRUCTURAL GATE (ADR-40 follow-up, "Challenger blindness").
#
# Every `spice-layout` integration test built its `LayoutOptions` with
# `..LayoutOptions::default()`, so no test in the tree ever ran a
# registered challenger. `dc-series-column-pinned` was graded end-to-end
# by the scoreboard -- 22 fixtures, ~60 verifiers, a k=9 multi-seed
# replay, a 1320-conversion seed sweep -- and still shipped a column
# anchored on the barycenter of its members' ORIGINS rather than their
# shared PINS, against CLAUDE.md's pin-anchored invariant. It surfaced
# only when promotion made the arm the default: the moment the most
# geometry is moving and the least attention is available per fixture.
#
# This runs the placer-agnostic STRUCTURAL checks (yes/no geometric facts
# with one correct answer, in CLAUDE.md's constraints-vs-costs sense)
# under ONE arm. `scoreboard-run` and `scoreboard-run-multi` depend on
# it, so a challenger cannot be collected without passing it. Continuous
# quality gradients stay aggregate and stay in the scoreboard; nothing
# here is a ratchet, and this grants no exception to one.
#
# Cost: ~1 s per arm on a warm target dir (the sweep of all 25 arms is
# ~18 s at --test-threads=2, and it runs on the ordinary `just test`
# path too).
#
# `--nocapture` on purpose: the sweep reports through `eprintln!` which
# deferred defects the arm REPAIRS, and cargo swallows that on a passing
# test -- the trap CLAUDE.md records against the first two promotions.
structural-gate placer="champion":
    bash -c 'ulimit -v ${RUST_TEST_MAX_VSZ_KB:-8388608} && \
      cargo test -p spice-layout --test challenger_structural -- \
        --exact "arm_$(echo {{placer}} | tr - _)" --nocapture'

# --- champion/challenger scoreboard (ADR-23) --------------------------------
#
# The ratchets answer "did this change break what we shipped?". The
# scoreboard answers "is placer B better than placer A?" — a different
# question needing a different instrument. It is for WHOLE-PLACER
# comparisons only and is NOT a licence to bypass a ratchet.
#
# Collect one placer's measurements. The verifiers themselves report the
# numbers they already compute, so this is just the suite with the sink
# switched on. A challenger run is EXPECTED to be red (every zero-slack
# ratchet is calibrated on the champion's output); `--no-fail-fast` is
# what keeps the measurements complete anyway.
scoreboard-run placer="champion" out="target/scoreboard": (structural-gate placer)
    rm -rf {{out}}/{{placer}}
    mkdir -p {{out}}/{{placer}}
    -bash -c 'ulimit -v ${RUST_TEST_MAX_VSZ_KB:-8388608} && \
      S2K_PLACER={{placer}} \
      S2K_SCOREBOARD_DIR="$PWD/{{out}}/{{placer}}" \
      cargo test --workspace --no-fail-fast -- --test-threads=${RUST_TEST_THREADS:-2}'

# Collect ONE placer over k SA seeds (ADR-32). Each seed lands in its own
# sink under {{out}}/{{placer}}/seed-N, so the single-seed `scoreboard`
# recipe still works against {{out}}/{{placer}}/seed-1 unchanged.
#
# Why this exists: `scoreboard-run` takes one draw from a stochastic
# optimiser. ADR-31 measured sd = 1.57 bends on `named_rails` where the
# recorded arm-to-arm "regression" was +5 -- i.e. the instrument could
# not distinguish a real effect from a lucky pair of draws, and one ADR
# had to be retracted because of it. k = 9 resolves a 1-bend effect at
# that spread (SE = sd/sqrt(k) ~ 0.5).
#
# Cost is linear: one full suite run per seed, ~8-12 min each.
scoreboard-run-multi placer="flow-seed-v4" k="9" out="target/scoreboard": (structural-gate placer)
    rm -rf {{out}}/{{placer}}
    for s in $(seq 1 {{k}}); do       mkdir -p {{out}}/{{placer}}/seed-$s;       bash -c "ulimit -v ${RUST_TEST_MAX_VSZ_KB:-8388608} &&         S2K_PLACER={{placer}}         S2K_SA_SEED=$s         S2K_SCOREBOARD_DIR=\"$PWD/{{out}}/{{placer}}/seed-$s\"         cargo test --workspace --no-fail-fast -- --test-threads=${RUST_TEST_THREADS:-2}"         > {{out}}/{{placer}}/seed-$s.log 2>&1 || true;       echo "  seed $s done";     done

# Compare two multi-seed collections: per-cell mean +/- sd, Welch t, and
# which cells clear |t| > 2. Cells whose spread swamps their difference
# are reported as UNRESOLVED rather than as a result.
scoreboard-multi champion="flow-seed-v4" challenger="y-sign" out="target/scoreboard":
    python3 scripts/scoreboard_multi.py {{out}}/{{champion}} {{out}}/{{challenger}} {{champion}} {{challenger}}

# Print the fixture x metric table, the tier-weighted aggregate, and the
# promotion verdict for two previously collected runs.
scoreboard champion="champion" challenger="m4-ydatum" out="target/scoreboard":
    S2K_SCOREBOARD_CHAMPION="$PWD/{{out}}/{{champion}}" \
    S2K_SCOREBOARD_CHALLENGER="$PWD/{{out}}/{{challenger}}" \
    S2K_SCOREBOARD_CHAMPION_NAME={{champion}} \
    S2K_SCOREBOARD_CHALLENGER_NAME={{challenger}} \
    cargo test -p spice2kicad --test scoreboard -- --ignored --nocapture

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
