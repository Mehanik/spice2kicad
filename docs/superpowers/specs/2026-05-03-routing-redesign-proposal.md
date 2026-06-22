# Wiring redesign — proposal

> **SUPERSEDED — historical design artifact.** This proposal predates
> implementation and has shipped. The authoritative contract is now
> `CLAUDE.md` (invariants V10/V11/V12) plus the as-built router in
> `crates/spice-route/`. NOTE: the RSMT algorithm discussion here
> (FLUTE / Hanan-grid DP / "exact for N<=9") was NOT what shipped. The
> as-built `crates/spice-route/src/steiner.rs` is Hwang-exact only at
> N=3 and uses a rectilinear-MST + Borah-Owens-Irwin Steinerization
> heuristic for 4<=N<=9 (plain RMST for N>=10). Do not treat any
> decision below as current; consult CLAUDE.md and the code. Kept for
> history only.

**Status:** proposal, awaiting approval. Author: 2026-05-03.

## Problem

Current renderings (`/tmp/v6final/*/zoom-1.png`) show the placement is
acceptable but the wiring is messy. Wire counts per fixture:

| fixture | wires | labels | nets | wires/net |
| --- | --- | --- | --- | --- |
| rc_lowpass | 7 | 6 | 4 | ~1.7 |
| diff_pair | 16 | 14 | 7 | ~2.3 |
| common_emitter | 33 | 12 | 6 | ~5.5 |
| multivibrator | 33 | 12 | 6 | ~5.5 |
| opamp_inverting_real | 14 | 11 | 5 | ~2.8 |

Anything > ~2 wires/net is being routed by the channel router
(`crates/kicad-emitter/src/schematic.rs::route_nets`, lines 756–924).
Its per-net path looks like this for every multi-pin net:

```
pin_A → escape_row_A (vertical) → trunk_x (horizontal)
      → escape_row_B (vertical) → pin_B (horizontal)
      → escape_row_C (vertical) → pin_C (horizontal)
      ...
```

Each pin gets its own globally-unique horizontal escape row, and each
net gets its own dedicated trunk column. That guarantees no collinear
merges between distinct nets, but it produces ~5 segments per net for
4-pin nets, plus the long parallel trunks visible at the bottom of the
common-emitter render.

The 2-pin and 3-pin "fast paths" added in T8b cover the simple cases.
What remains is **multi-pin nets in dense regions**, which is exactly
where every analog circuit spends most of its complexity.

After Stage 1 (power-symbol placement, see below) Power and Ground
nets contribute zero wires — they're rendered as glyphs. The
remaining Signal-net wire count drops by roughly the proportion of
pins on rails: in `common_emitter` that's about 40% of all pins,
so the post-Stage-1 baseline is ~20 wires (vs 33 today) before
Stage 2 even runs.

## What state-of-the-art does

I surveyed the current research and tooling landscape:

1. **VLSI physical design** uses two stages:
   - **Global routing** assigns each net to a coarse channel grid.
   - **Detail routing** runs a maze (Lee/A*) inside the channel.
   - Multi-pin nets are decomposed via **Rectilinear Steiner Minimum
     Tree (RSMT)** — typically [FLUTE][flute] (Chu/Wong, ICCAD 2004) —
     which gives optimal trees up to 9 pins from a precomputed lookup
     table and ≤3% error up to 100 pins. FLUTE has open-source C/C++
     implementations and is used in OpenROAD's global router.

2. **General orthogonal graph drawing** (yFiles, Eclipse Layout
   Kernel) uses **orthogonal edge routing** with a port-aware A* that
   minimizes bends first, length second. ELK Layered's orthogonal
   router handles "circuit schematics" specifically — based on Sander
   and Di Battista et al.'s work.

3. **Schematic-specific routing** is a recognized hard problem.
   The seminal paper is *Tilford, "Aesthetic routing for transistor
   schematics" (1994, IEEE)* — a two-step approach with global
   abstraction levels + channel routing. Modern follow-ups
   (Frontiers in IT/EE 2024 review) confirm this remains a partially
   solved problem; commercial tools (Cadence Virtuoso, Synopsys Custom
   Compiler) use semi-automated workflows. Open-source schematic
   auto-generation is rare — most tools require manual layout
   (lcapy, schemdraw, gEDA).

**Key insight from the literature:** schematic aesthetics differ from
PCB aesthetics. PCB autorouters minimize length and via count;
schematic readers prefer **few bends, T-junctions for branching,
horizontal rails for power, and named labels for distance**. Length
is secondary. The current channel router optimizes the wrong objective.

## Proposal: per-net rectilinear Steiner trees

Replace the channel router's main path with a four-stage pipeline.
The 2-pin and 3-pin fast paths from T8b stay; they are special cases
of stage 2.

### Stage 1 — Power symbol placement

Power and Ground nets (already classified by `net_class.rs`) emit
**no wires at all**. Instead, for every pin connected to such a net,
the router places a small power-symbol glyph from the standard KiCad
`power` library. KiCad treats matching `power:*` symbols as
electrically connected globally — the glyph itself acts as the named
net marker, no trunk wire required.

Concretely:

- For each pin on a Power-classified net, emit
  `(symbol (lib_id "power:VCC") (at <pin_x> <pin_y - 2.54> 0) …)`
  (one grid cell above the pin if the pin's outward direction points
  up; placed on the outward side regardless). The `lib_id` is chosen
  by best-match against the rail's net name: `vcc` → `power:VCC`,
  `vdd` → `power:VDD`, `+5v` → `power:+5V`, `+12v` → `power:+12V`,
  etc. Default to `power:VCC` if no specific match.
- For each pin on a Ground net, emit
  `(symbol (lib_id "power:GND") (at <pin_x> <pin_y + 2.54> 0) …)`
  on the outward side of the pin. Variants `power:GNDA`,
  `power:GNDPWR` are not used in v0.1.
- The power symbol's *active* pin sits exactly on the host element's
  pin, so KiCad's connectivity engine merges them on file load
  without an explicit `(wire …)` segment.
- Emit ZERO wires and ZERO `(label …)` / `(global_label …)`
  S-expressions for Power or Ground nets in this stage. The glyph
  *is* the connection.

The outward direction is taken from the host element's pin
geometry: a pin pointing right has its power symbol placed to the
right of the host bounding box, and so on. Rotation of the
`power:*` symbol matches: 0° for upward (default), 90° for
rightward, 180° for downward, 270° for leftward. This keeps the
glyph's stem touching the host pin without the symbol body
overlapping the host.

Library dependency: the standard KiCad install ships
`power.kicad_sym` containing all the `power:VCC` / `power:GND` /
`power:+5V` / `power:+12V` / `power:VDD` / `power:VSS` / etc.
symbols. We assume it is available. If a requested `lib_id` cannot
be resolved (the user's library set lacks it), fall back to
emitting a `(global_label …)` at the pin instead — same electrical
semantics, less pretty.

### Stage 2 — Per-net RSMT for signal nets

For each Signal net with N pins:

- **N = 2:** existing 2-pin fast path. L-shape or single segment.
- **N = 3:** Hwang's 3/2 algorithm — exact optimal, three-cases
  based on the bounding box. Always a T-junction. (T8b's "pick
  closest pin as center" heuristic is replaced by this.)
- **N = 4–9:** FLUTE-style lookup. Either port the FLUTE lookup
  tables (POWVs and trees, ~100 KB of data, BSD-licensed) or
  reimplement the small-degree exact algorithm from the paper. For
  ≤9 pins the exact Hanan-grid enumeration is tractable.
- **N ≥ 10:** rare in our fixtures (none today). Defer; if it ever
  matters, port full FLUTE.

The RSMT for a net produces a set of axis-parallel segments meeting
at Hanan-grid Steiner points. Bend count is roughly 2× pin count for
a Steiner tree vs ~5× for a channel route — a 2-3× wire-count
reduction on the multi-pin nets that dominate the visual mess.

### Stage 3 — Conflict resolution (rip-up & retry)

Run all nets through Stage 2 in parallel; then walk every emitted
segment and detect:

- Same-coordinate horizontal segments on different nets at the same Y.
- Overlapping vertical segments at the same X.
- Wire crossings (segments of distinct nets that cross transversally).

For each conflict, escalate in this order:

1. **Alternate Steiner topology.** RSMTs of equal length but
   different shape often differ in segment placement. FLUTE returns
   a list of POWVs (Possibly Optimal Wirelength Vectors); pick the
   second-best if the first conflicts.
2. **Y/X jog.** Add a one-grid-cell offset to one segment to break
   the collinear merge. Adds two bends but kills the conflict.
3. **Label promotion.** Replace the longest run of the conflicting
   net with named labels at each end ("net jump"). KiCad's standard
   convention; spec V4 already says "≤2 labels per net" allows
   exactly this. If the net name is auto-generated (parser node
   numbers), generate a stable derived name.

Stage 3 is the rip-up-and-reroute loop. Cap at 10 iterations; if
unresolved, emit a warning diagnostic and accept the residual
crossings.

### Stage 4 — Coalesce and snap

Walk emitted segments per net; merge collinear adjacent segments
into one (eliminates redundant junctions). Snap endpoints to
1.27 mm grid. Emit final `(wire …)` S-expressions plus
`(junction …)` markers at Steiner points and T-junctions.

## Architecture

New crate or module: `crates/spice-route/` (sibling to `spice-layout`).
The placer hands off `Placement` + pin geometry, the router emits
`Vec<Sexpr>` of wires/junctions/labels. Replaces the body of
`route_nets` in `crates/kicad-emitter/src/schematic.rs` — the
emitter just stitches the router's output into the final
S-expression tree.

```
spice-route/
  src/
    lib.rs              -- public entry: route(placement, nets) -> RoutedSchematic
    rails.rs            -- stage 1: power-symbol placement (power:VCC, power:GND, …)
    steiner.rs          -- stage 2: RSMT (Hwang's for ≤3 pins, FLUTE for ≥4)
    flute_tables.rs     -- precomputed lookup tables (or generated at build time)
    rip_up.rs           -- stage 3: conflict resolution
    cleanup.rs          -- stage 4: coalesce + snap
    label.rs            -- stage 3 escape: promote to labeled net
```

## Why not maze routing / A*

Maze routing (Lee, Soukup, A*) routes one net at a time on a grid,
finding shortest paths around obstacles. It's the right tool when
**routing space is congested** — IC routing through dense cell rows.

Schematics are not congested. We have lots of empty grid, and the
cost we minimize is **bends per net, not length**. Steiner trees
pick optimal bend structure directly; maze routing has to be
post-processed for bend minimization. The literature consistently
favors Steiner-first for analog schematic routing; maze is the
fallback for whatever Steiner can't place cleanly (Stage 3).

## Why not just integrate ELK / yFiles

Both are excellent, both are JVM-based (Java/JavaScript). Adding a
JVM dependency to a Rust CLI is heavy. The algorithms are not magic
— they are RSMT + maze + bend-minimization — and the schematic case
is small enough (≤50 nets, ≤200 pins) that a from-scratch Rust
implementation of the Stage 2/3 core will be ~1500 LOC and faster
than calling out to an external tool.

## Effort estimate

- **Stage 1** (power symbol placement): ~150 LOC. Half a day.
  Highest visual ROI — replaces every Power/Ground trunk with a
  trail of small glyphs, eliminating the longest wires entirely.
- **Stage 2 — Hwang's for N≤3**: ~150 LOC. Half a day. Replaces
  current 2/3-pin fast paths cleanly.
- **Stage 2 — FLUTE for N=4–9**: ~600 LOC + ~100 KB lookup tables.
  2–3 days. Optional: port from the BSD-licensed C implementation.
- **Stage 3 — rip-up & retry**: ~400 LOC. 2 days.
- **Stage 4 — cleanup**: ~100 LOC. Half a day.
- **Test fixtures + tightening**: 1–2 days. Visual confirmation
  per fixture; tighten the loosened crossing budgets back to plan
  values (0/2/4/2/2).

Total: ~9 working days for the full pipeline; ~2.5 days for
Stages 1+2 alone, which should give 70–80% of the visual win.

## Recommended order

1. **Stage 1 (power symbols) first.** Most visual impact for least
   code. Power/ground are the longest, busiest trunks and currently
   look the worst — replacing them with glyphs removes the entire
   problem class instead of routing it more cleverly.
2. **Stage 2 (Hwang's for N≤3)** to replace current heuristic
   fast paths with provably optimal ones.
3. **Stage 2 (FLUTE for N≥4)** when N≤3 is solid.
4. **Stage 3 (conflicts)** only if Stage 2 produces visible
   crossings on real fixtures. Likely not needed for fixtures we
   have; needed for circuits with >50 elements.
5. **Stage 4 (cleanup)** is mechanical; do it whenever.

## Open questions

- **FLUTE port vs reimplementation.** FLUTE3 is 4 KLoC of C++.
  Porting to Rust is mechanical but tedious. Calling via FFI is
  faster to ship but adds a build dependency. My lean: reimplement
  the lookup-table lookup (the hard part — POWV generation — is
  done at build time, not runtime).
- **Label promotion threshold.** When does a net become "too long
  for a wire"? A heuristic: bbox diagonal > 75 mm (about the
  width of a small fixture). Tune by visual.
- **Junction emission.** KiCad uses `(junction ...)` markers at
  3+ way connections. Emit them at Steiner points; the existing
  endpoint_count machinery does this for the channel router and
  can be reused.
- **`power:*` library availability.** The standard KiCad install
  ships `power.kicad_sym`. We assume it is on the user's library
  search path (passed via `-l`). If a requested `lib_id` cannot be
  resolved at emit time, fall back to a `(global_label …)` with
  the same net name — electrically equivalent, visually a labelled
  stub instead of a glyph. We may want to ship a minimal
  `power.kicad_sym` in `crates/kicad-symbols/tests/fixtures/` to
  guarantee fixture tests don't depend on the user's environment.

[flute]: https://github.com/The-OpenROAD-Project-Attic/flute3
