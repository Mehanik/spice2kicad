//! **The four things a human reads first** (ADR-28, informational).
//!
//! # Why this file exists
//!
//! The project's instruments have three times scored as an improvement
//! something the owner read, on sight, as damage:
//!
//! * the SA scored destroying a textbook LC-ladder drawing as a 2.7x
//!   cost improvement — its objective has no orientation term at all;
//! * phase 4.5 scored mangling two inductors as a strict V5 win, and was
//!   correct by its own metric;
//! * the ADR-23 aggregate scored `flow-seed` a large win, after which
//!   the owner said the champion is better on four of eighteen fixtures.
//!
//! Every one of those is a *true* statement about the metric that made
//! it. None of the registered metrics measures **axis consistency**,
//! **orientation uniformity**, **chain adjacency** or **device
//! stacking**, which is what the eye picks up before it reads a single
//! refdes. A defect no verifier
//! measures is invisible to the ratchets AND to the scoreboard (ADR-23
//! "Known limits"), so the fix is a measurement, not a weight.
//!
//! Four are added here, all computed from the **emitted `.kicad_sch`
//! geometry** so they grade any placer, present or future, rather than
//! restating one placer's internals.
//!
//! # A — series-chain axis uniformity (`chain.axis`, `chain.reversal`)
//!
//! A **series chain** is a maximal path of two-terminal signal elements
//! linked by nets of *signal degree 2*: each interior net of the chain
//! touches exactly two drawn, non-rail-stub elements, so the current
//! leaving one member has nowhere to go but into the next. Textbook
//! style draws such a chain along **one shared axis, in one direction** —
//! that is what makes a ladder read as a ladder.
//!
//! The motivating specimen is `lc_ladder_lpf`'s `RS -> L1 -> L2 -> L3`,
//! which the shipping placer emits at rotations 180 / 90 / 0 / 270: one
//! chain, four orientations. The deterministic seed (`--no-refine`)
//! emits all four at 90.
//!
//! For each chain, pick the axis (horizontal or vertical) that reads the
//! drawing most charitably, and report two disjoint counts:
//!
//! * **`chain.axis`** — members whose pin axis differs from the chain's
//!   chosen axis;
//! * **`chain.reversal`** — members that ARE on the chosen axis but run
//!   *against* the chain's majority direction of travel.
//!
//! The two are deliberately disjoint: an off-axis member is counted
//! once, under `chain.axis`, and is not also counted as reversed. A
//! single element cannot be blamed twice for one pose, and keeping them
//! separate is what lets a reader tell "the ladder zig-zags between
//! horizontal and vertical" (an axis defect) from "one inductor is drawn
//! backwards" (a direction defect) — different repairs in the placer.
//!
//! # C — series-chain compactness (`chain.stranded`)
//!
//! A's two counts measure a chain's axis *uniformity* and its
//! *direction*. Neither measures **adjacency**, so a chain shattered
//! into separated columns is invisible to both. `port_shapes` — a
//! four-resistor series chain — is emitted as two vertical stacks of
//! two, 31.75 mm apart, joined by a wire that jumps sideways mid-chain,
//! and it scores `chain.axis = 0, chain.reversal = 0`. A perfect pair,
//! for a drawing the owner called "completely mad" on sight; a
//! challenger that repaired it into one connected folded path scored
//! `1 / 1`, strictly worse. The metric ranked the broken drawing ABOVE
//! the better one, which is the inverse of what ADR-28 exists for.
//!
//! `chain.stranded` closes that. Two consecutive members are
//! **adjacent** when the wire between them — the Manhattan run between
//! their pins on the net they share — is no longer than **all the
//! chain's device bodies laid end to end**. One hop that swallows more
//! sheet than every device in the chain occupies is not spacing, it is a
//! break. Unbroken links partition the chain into maximal adjacent
//! clusters, and `chain.stranded` counts the members outside the largest
//! one, with `chain.run_members` as the denominator.
//!
//! Stating the threshold in the chain's own drawn material — not in
//! millimetres, and not per member pair — is what lets it pass
//! `lc_ladder_lpf`'s textbook 22.86 mm strides while failing
//! `port_shapes`'s single 41.91 mm jump. ADR-28 records the two
//! alternatives that were measured and rejected; both rank the textbook
//! ladder WORST of the three specimens.
//!
//! Unlike A, C measures the chain's **rail-terminated end element** too.
//! `port_shapes`'s `R4` terminates on ground, so `chains()` excludes it,
//! and that exclusion is half of why the broken drawing scored 0. A
//! stub's exclusion from A is a *pose* argument — its orientation is
//! fixed by its rail glyph (V14) and cannot also be asked to obey the
//! chain's direction. C makes no demand on pose, and a terminating stub
//! carries the chain's own current, so for adjacency it is on the chain.
//!
//! # B — shared-current-path stacking (`stack.side_by_side`)
//!
//! Devices in series on a DC current path — a cascode's two transistors,
//! a collector load above its transistor, a rail-to-rail bias divider —
//! are conventionally **stacked in Y**, not spread in X. The current
//! runs down the page from supply to ground, and a stack is how a reader
//! sees that it is one current.
//!
//! A **DC-series pair** is two drawn elements `u`, `v` that
//!
//! 1. each conduct DC between two *distinct* rail nets (there is a path
//!    supply -> ... -> u -> ... -> ground that does not re-use `u`), and
//! 2. share a non-rail net `N` whose **DC degree is exactly 2** — `u`
//!    and `v` are the only DC conductors on it, so all of `u`'s current
//!    flows into `v`.
//!
//! `stack.side_by_side` counts the pairs drawn wider than tall
//! (`|dx| > |dy|` between element centres). Motivating specimen:
//! `cascode_amp`'s `Q1`/`Q2`, which the champion stacks and the shipping
//! placer sets side by side — the owner's stated reason for preferring
//! the champion on that fixture.
//!
//! Clause 2 is what keeps the metric from demanding nonsense. A
//! differential pair's two transistors share `tail` with `RTAIL`, so
//! that net has DC degree 3 and the pair is NOT counted — which is
//! right, because a diff pair is drawn side by side on purpose. See
//! `stacking_discriminator_separates_the_cascode_from_the_diff_pair`,
//! the assertion that keeps this honest.
//!
//! # D — device facing (`device.facing_inverted`)
//!
//! A reader expects a transistor's **higher-DC-potential terminal drawn
//! screen-up**: an NPN's collector above its emitter, current running
//! down the page from supply to ground. `two_stage_amp`'s `Q2` is
//! emitted upside down (rot 180 + mirror) and every registered metric
//! reads it as clean — it is locally violation-free, its V5 first
//! segments both leave outward, and no chain or stacking count has an
//! opinion about a three-terminal device's pose.
//!
//! The order is derived from the netlist, not from a device library.
//! Build the same DC graph metric B uses, then rank every net by hop
//! distance to the nearest **up-rail** (a positive supply) and to the
//! nearest **down-rail** (ground, or a negative supply), with rails
//! absorbing and the device's own edge removed. The conduction terminal
//! that is strictly nearer the up-rail *and* strictly further from the
//! down-rail is the one that must be drawn on top. Because the answer
//! comes from topology, a PNP needs no special case: its emitter is the
//! terminal on the supply side, so the same comparison puts it up.
//!
//! `device.facing_inverted` counts the devices drawn the other way up;
//! `device.facing_resolved` is the denominator, so a zero that means
//! "clean" is distinguishable from a zero that means "the rank declined
//! everywhere". Declining is a real answer here — a floating pass
//! transistor, a tie, or two axes that disagree all report nothing
//! rather than guessing.
//! # E — port-terminal label direction (`port.label_vertical`,
//! `port.label_backwards`)
//!
//! A `*@port` terminal — or any one-pin interface net — is drawn as a
//! `(global_label …)`, and its rotation decides how the marker reads.
//! The owner's report: *"Both VIN and VOUT as well as capacitors
//! connected to it should be horisontal. This is common issue for many
//! circuits."* Nine of the suite's terminals are currently drawn on end.
//!
//! A/B/C are all blind to it — provably: they are byte-identical across
//! the two `terminal-series` challenger arms on all 18 fixtures, and the
//! ADR-23 aggregate consequently ranked the arm that leaves a terminal
//! vertical (and turns a second correct one sideways) ABOVE the arm that
//! repairs every one it reaches. The instrument preferred the visibly
//! worse drawing, one level down from the inversion this ADR opened with.
//!
//! Two disjoint counts over one population — every top-level
//! `(global_label …)`:
//!
//! * **`port.label_vertical`** — rotation 90 or 270. KiCad maps both to
//!   `ANGLE_VERTICAL` (`sch_label.cpp:395`), differing only in which side
//!   of the anchor the tag hangs, so there is no readable vertical
//!   option to prefer: either way the reader tilts their head at a
//!   terminal on a horizontal signal path.
//! * **`port.label_backwards`** — a horizontal terminal whose arrowhead
//!   travels *leftward*: an `input` at 0 or an `output` at 180. The
//!   arrow's direction is `CreateGraphicShape`'s
//!   (`sch_label.cpp:2146`), which points the anchor end for `L_INPUT`
//!   and the far end for `L_OUTPUT`; leftward is against the project's
//!   own left-to-right flow convention.
//!
//! `backwards` grades only terminals the SOURCE declares with `*@port`,
//! because the emitted `(shape …)` token is a default everywhere else —
//! `common_emitter`'s `out` is stamped `(shape input)`. See
//! [`port_label_metrics`] for the full argument and
//! `port_direction_is_graded_only_where_the_source_declares_it` for the
//! assertion that keeps it honest.
//!
//! # Informational at birth
//!
//! All three metrics are registered with the ADR-23 scoreboard as
//! `Tier::Info` — printed per fixture, zero aggregate weight — on the
//! precedent of Q6's balance CoV and `bend_bound.rs`'s V16 bound.
//! **None of them is a zero-slack ratchet.** A metric whose definition is
//! still being calibrated must not be able to block work: an ambiguity
//! resolved the wrong way (see ADR-28's list) would, as a gate, reject
//! correct drawings while being wrong. What would justify promoting each
//! to a ratchet is recorded in ADR-28.
//!
//! The only assertions here are therefore
//!
//! * **soundness / non-vacuity** control arms on synthetic geometry, so
//!   the metrics cannot silently degenerate to "always 0"; and
//! * **specimen rankings** in `<=` form — they fire only if the arm the
//!   owner prefers becomes strictly WORSE than the arm they reject, i.e.
//!   only if the metric's own validation inverts. They can never block a
//!   change that improves the shipping placer.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use common::spice_to_kicad;
use kicad_symbols::{Library, Orientation, Rotation};
use lexpr::Value;
use spice_diagnostics::FileId;
use spice_resolve::{ElementKind, PortDir};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("readability", name)
}

// --- lexpr helpers (mirrors flow_geometry.rs) ----------------------------

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    v.list_iter().map_or_else(
        || Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &Value>>,
        |it| Box::new(it),
    )
}

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(as_str)
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_symbol()
        .or_else(|| v.as_str())
        .or_else(|| v.as_keyword())
}

fn as_f64(v: &Value) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    list_iter(v).find(|c| c.is_list() && head(c) == Some(name))
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

fn load_test_library() -> Library {
    let libs_dir = libs_dir();
    let device =
        Library::from_file(libs_dir.join("Device.kicad_sym")).expect("parse Device.kicad_sym");
    let sim = Library::from_file(libs_dir.join("Simulation_SPICE.kicad_sym"))
        .expect("parse Simulation_SPICE.kicad_sym");
    let amp = Library::from_file(libs_dir.join("Amplifier_Operational.kicad_sym"))
        .expect("parse Amplifier_Operational.kicad_sym");
    let power =
        Library::from_file(libs_dir.join("power.kicad_sym")).expect("parse power.kicad_sym");
    device.merge(sim).merge(amp).merge(power)
}

fn libs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures")
}

fn placed_symbol_pose(sym: &Value) -> Option<(f64, f64, Orientation)> {
    let at = find_child(sym, "at")?;
    let mut it = list_iter(at);
    it.next();
    let x = it.next().and_then(as_f64)?;
    let y = it.next().and_then(as_f64)?;
    let rot_deg = it.next().and_then(as_f64).unwrap_or(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rot_u = ((rot_deg.round() as i64).rem_euclid(360)) as u16;
    let rotation = match rot_u {
        0 => Rotation::R0,
        90 => Rotation::R90,
        180 => Rotation::R180,
        270 => Rotation::R270,
        _ => return None,
    };
    let mirror_y = find_child(sym, "mirror")
        .and_then(|m| list_iter(m).nth(1).and_then(as_str))
        .is_some_and(|s| s == "y");
    Some((x, y, Orientation { rotation, mirror_y }))
}

fn placed_symbol_refdes_and_lib_id(sym: &Value) -> Option<(String, String)> {
    let lib_id = find_child(sym, "lib_id")
        .and_then(|l| list_iter(l).nth(1).and_then(as_str))
        .map(str::to_owned)?;
    let mut refdes = None;
    for prop in children(sym, "property") {
        let mut it = list_iter(prop);
        it.next();
        if it.next().and_then(as_str) == Some("Reference") {
            refdes = it.next().and_then(as_str).map(str::to_owned);
            break;
        }
    }
    Some((refdes?, lib_id))
}

// --- the measured model --------------------------------------------------

/// Two pins on the same row/column are co-axial within this slop. Grid
/// coordinates are exact multiples of 1.27 mm, so anything above f64
/// round-trip noise is a real difference.
const TOL_MM: f64 = 0.01;

/// One placed body pin in world coordinates, tagged with its SPICE net.
/// `#PWR*` glyph pins are excluded: a glyph is decoration hung off a
/// rail pin, not a device terminal.
#[derive(Debug, Clone)]
struct BodyPin {
    refdes: String,
    x_mm: f64,
    y_mm: f64,
    net: String,
}

#[derive(Debug, Clone)]
struct Elem {
    refdes: String,
    kind: ElementKind,
    /// SPICE node names, terminal order preserved.
    nodes: Vec<String>,
    /// True for a `;@ power=` source: lowered to rail glyphs, never a body.
    is_power_source: bool,
    /// True for a top-level `X<n>` lowered to a hierarchical `(sheet …)`
    /// block. It is *on the drawing* and occupies its nets, so it must
    /// raise a net's degree — but it has no body pins here and no single
    /// current path, so it is never a chain member and never a DC edge.
    is_sheet: bool,
}

/// One emitted top-level `(global_label …)`: the marker KiCad draws
/// where a net leaves the sheet. Metric D's whole population.
#[derive(Debug, Clone)]
struct PortLabel {
    net: String,
    /// The emitted `(shape …)` token, as written.
    shape: String,
    /// The emitted `(at … rot)` angle, one of 0/90/180/270.
    rot: u16,
    /// `Some(dir)` iff the SOURCE declares `*@port <net>=<dir>`.
    ///
    /// Read from the netlist, never inferred from `shape`: the emitter
    /// stamps `(shape input)` on every *undeclared* one-pin interface
    /// net, semantic outputs included, so the token is a default there
    /// rather than a statement. See [`port_label_metrics`].
    declared: Option<PortDir>,
}

#[derive(Debug)]
struct Fixture {
    name: String,
    pins: Vec<BodyPin>,
    elements: Vec<Elem>,
    rail_nets: HashSet<String>,
    /// Rail nets on the **down** side of the page — ground, and any
    /// negative supply. Metric D's rank needs the page polarity of a
    /// rail, not merely that it is one, and a negative supply is a rail
    /// that belongs at the BOTTOM (the same distinction V14 keys off).
    down_rails: HashSet<String>,
    /// Top-level `(global_label …)` terminals, in emitted order.
    labels: Vec<PortLabel>,
}

/// The canonical *ground / negative supply* names — the down half of
/// [`is_canonical_rail_name`].
fn is_canonical_ground_name(net: &str) -> bool {
    let lo = net.to_ascii_lowercase();
    net == "0" || matches!(lo.as_str(), "gnd" | "vss" | "vee" | "v-" | "vminus")
}

fn is_canonical_rail_name(net: &str) -> bool {
    let lo = net.to_ascii_lowercase();
    net == "0"
        || matches!(
            lo.as_str(),
            "gnd" | "vss" | "vee" | "v-" | "vminus" | "vcc" | "vdd" | "v+" | "vplus"
        )
}

impl Fixture {
    fn is_rail_net(&self, net: &str) -> bool {
        self.rail_nets.contains(net)
    }

    /// Is this a rail the page draws DOWNWARD from — ground, or a
    /// negative supply? Only meaningful for a rail net.
    fn is_down_rail(&self, net: &str) -> bool {
        self.down_rails.contains(net)
    }

    fn elem(&self, refdes: &str) -> Option<&Elem> {
        self.elements.iter().find(|e| e.refdes == refdes)
    }

    /// The element's emitted body pins.
    fn pins_of(&self, refdes: &str) -> Vec<&BodyPin> {
        self.pins.iter().filter(|p| p.refdes == refdes).collect()
    }

    /// The element has an emitted two-pin body whose geometry can be
    /// measured. A hierarchical-sheet instance never does.
    fn has_body(&self, refdes: &str) -> bool {
        self.pins.iter().any(|p| p.refdes == refdes)
    }

    /// Element centre = the mean of its emitted body pins.
    fn centre(&self, refdes: &str) -> Option<(f64, f64)> {
        let ps = self.pins_of(refdes);
        if ps.is_empty() {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let n = ps.len() as f64;
        Some((
            ps.iter().map(|p| p.x_mm).sum::<f64>() / n,
            ps.iter().map(|p| p.y_mm).sum::<f64>() / n,
        ))
    }

    /// **Rail stub** in ADR-15's role model: a two-terminal element with
    /// exactly one rail pin. It terminates a node rather than passing a
    /// signal along, and convention hangs it off that node's column, so
    /// it takes no part in a series chain and does not raise a net's
    /// signal degree.
    ///
    /// Re-derived from the netlist here rather than imported from
    /// `spice-layout`, following the `flow_geometry.rs` precedent: a
    /// metric that borrows the placer's own classification can only
    /// restate it, never falsify it.
    fn is_rail_stub(&self, refdes: &str) -> bool {
        let Some(e) = self.elem(refdes) else {
            return false;
        };
        e.nodes.len() == 2
            && e.nodes[0] != e.nodes[1]
            && e.nodes.iter().filter(|n| self.is_rail_net(n)).count() == 1
    }

    /// **Series-signal element**: two-terminal, not a power source, with
    /// NEITHER node rail-class. It lies *on* the signal path. This is the
    /// `flow_geometry.rs` F5 discriminator, and the candidate set for a
    /// series chain.
    fn is_series_signal(&self, refdes: &str) -> bool {
        let Some(e) = self.elem(refdes) else {
            return false;
        };
        !e.is_power_source
            && !e.is_sheet
            && e.nodes.len() == 2
            && e.nodes[0] != e.nodes[1]
            && !self.is_rail_net(&e.nodes[0])
            && !self.is_rail_net(&e.nodes[1])
    }

    /// Drawn, non-rail-stub elements touching `net` — the "signal
    /// degree" members. A net of signal degree 2 passes all of one
    /// member's signal into the other, which is exactly the link a
    /// series chain is made of.
    fn signal_members(&self, net: &str) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .elements
            .iter()
            .filter(|e| {
                !e.is_power_source
                    && !self.is_rail_stub(&e.refdes)
                    && e.nodes.iter().any(|n| n == net)
            })
            .map(|e| e.refdes.as_str())
            .collect();
        out.sort_unstable();
        out
    }
}

fn convert(name: &str, extra: &[&str]) -> (common::TempDir, PathBuf) {
    let dir = tempdir(name);
    if extra.is_empty() {
        let sch = spice_to_kicad(&fixtures_dir().join(format!("{name}.cir")), &dir)
            .unwrap_or_else(|e| panic!("convert {name}: {e}"));
        return (dir, sch);
    }
    // A pinned-arm conversion (a specific `--placer`, or `--no-refine`).
    // Deliberately NOT routed through `common::spice_to_kicad`: that
    // helper appends `common::placer_args()` — which would collide with
    // an explicit `--placer` under a scoreboard challenger run — and
    // records `t0.convert_fail` to the sink, which would file a pinned
    // arm's measurement under the row being collected for another placer.
    let out = dir.path().join(format!("{name}.kicad_sch"));
    let libs = libs_dir();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spice2kicad"));
    cmd.arg(fixtures_dir().join(format!("{name}.cir")))
        .arg("-t")
        .arg("schematic")
        .arg("-o")
        .arg(&out)
        .arg("-l")
        .arg(libs.join("Device.kicad_sym"))
        .arg("-l")
        .arg(libs.join("Simulation_SPICE.kicad_sym"))
        .arg("-l")
        .arg(libs.join("Amplifier_Operational.kicad_sym"))
        .arg("-l")
        .arg(libs.join("power.kicad_sym"))
        .arg("--no-layout-cache")
        .args(extra);
    let status = cmd.status().expect("invoke spice2kicad");
    assert!(status.success(), "convert {name} {extra:?}: {status}");
    (dir, out)
}

fn load_arm(name: &str, extra: &[&str]) -> Fixture {
    let (_guard, sch) = convert(name, extra);
    let src = std::fs::read_to_string(&sch).expect("read sch");
    let root = lexpr::from_str(&src).expect("parse sch");

    let library = load_test_library();
    let spice_src =
        std::fs::read_to_string(fixtures_dir().join(format!("{name}.cir"))).expect("read cir");
    let parsed = spice_parser::parse(&spice_src, FileId(0)).expect("parse spice");
    let resolved = spice_resolve::resolve(&parsed.netlist, &library).expect("resolve spice");

    let mut by_refdes: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut elements: Vec<Elem> = Vec::new();
    let mut rail_nets: HashSet<String> = HashSet::new();
    let mut down_rails: HashSet<String> = HashSet::new();
    for el in &resolved.elements {
        let mut pairs = Vec::with_capacity(el.pin_mapping.len());
        for (i, kicad_pin) in el.pin_mapping.iter().enumerate() {
            if let Some(net) = el.nodes.get(i) {
                pairs.push((kicad_pin.clone(), net.clone()));
            }
        }
        by_refdes.insert(el.refdes.clone(), pairs);
        let is_power_source = matches!(el.role, spice_resolve::ElementRole::Power(_));
        // A `;@ power=` string beginning with `-` marks a NEGATIVE
        // supply: a rail net, but one drawn at the bottom of the page.
        // `named_rails`' `n5` is the case a name test cannot catch.
        let negative_rail = matches!(
            &el.role,
            spice_resolve::ElementRole::Power(rail) if rail.trim_start().starts_with('-')
        );
        for net in &el.nodes {
            if is_power_source || is_canonical_rail_name(net) {
                rail_nets.insert(net.clone());
            }
            if negative_rail || net == "0" || is_canonical_ground_name(net) {
                down_rails.insert(net.clone());
            }
        }
        elements.push(Elem {
            refdes: el.refdes.clone(),
            kind: el.kind,
            nodes: el.nodes.clone(),
            is_power_source,
            is_sheet: false,
        });
    }
    // A top-level `X<n>` lowered to a hierarchical `(sheet …)` block is a
    // resolved *sheet instance*, not a resolved element, so it is absent
    // from `resolved.elements`. Leaving it out would be a silent
    // falsification: `opamp_inverting`'s `inv` node would read as degree
    // 2 (RIN, RF) and the metric would report a series CHAIN across an
    // op-amp's virtual ground — two resistors that are not in series at
    // all. It is on the drawing and it occupies its nets, so it counts.
    for si in &resolved.sheet_instances {
        for net in &si.nodes {
            if is_canonical_rail_name(net) {
                rail_nets.insert(net.clone());
            }
        }
        elements.push(Elem {
            refdes: si.refdes.clone(),
            kind: ElementKind::Subckt,
            nodes: si.nodes.clone(),
            is_power_source: false,
            is_sheet: true,
        });
    }

    let mut pins = Vec::new();
    for sym in children(&root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        if refdes.starts_with("#PWR") || refdes.starts_with("#FLG") {
            continue;
        }
        let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
            continue;
        };
        let Some(lib_sym) = library.lookup(&lib_id) else {
            continue;
        };
        let pin_to_net: HashMap<&str, &str> = by_refdes
            .get(&refdes)
            .map(|v| v.iter().map(|(p, n)| (p.as_str(), n.as_str())).collect())
            .unwrap_or_default();
        for tp in lib_sym.pins_in(orient) {
            let Some(net) = pin_to_net.get(tp.number.as_str()) else {
                continue;
            };
            pins.push(BodyPin {
                refdes: refdes.clone(),
                x_mm: ox + tp.x,
                y_mm: oy - tp.y,
                net: (*net).to_string(),
            });
        }
    }

    // Ground names reached only through a sheet instance still belong on
    // the down side.
    for net in &rail_nets {
        if net == "0" || is_canonical_ground_name(net) {
            down_rails.insert(net.clone());
        }
    }
    let labels = terminal_labels(&root, &resolved.ports);

    Fixture {
        name: name.to_string(),
        pins,
        elements,
        rail_nets,
        down_rails,
        labels,
    }
}

/// Every top-level `(global_label …)`, tagged with the direction the
/// SOURCE declares for its net (if any).
///
/// In this emitter that set is exactly the sheet's boundary terminals:
/// rails are drawn as power glyphs, internal nets get plain
/// `(label …)`, and a hierarchical `(sheet …)` port is joined by wires
/// rather than by a co-located global label (`schematic.rs`'s
/// `dangling_pin_labels`).
fn terminal_labels(sheet: &Value, ports: &[spice_resolve::PortSpec]) -> Vec<PortLabel> {
    let declared: HashMap<&str, PortDir> = ports.iter().map(|p| (p.net.as_str(), p.dir)).collect();
    let mut labels = Vec::new();
    for gl in children(sheet, "global_label") {
        let Some(net) = list_iter(gl).nth(1).and_then(as_str) else {
            continue;
        };
        let shape = find_child(gl, "shape")
            .and_then(|s| list_iter(s).nth(1).and_then(as_str))
            .unwrap_or("input")
            .to_string();
        let angle = find_child(gl, "at")
            .and_then(|a| list_iter(a).nth(3).and_then(as_f64))
            .unwrap_or(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let rot = ((angle.round() as i64).rem_euclid(360)) as u16;
        labels.push(PortLabel {
            net: net.to_string(),
            shape,
            rot,
            declared: declared.get(net).copied(),
        });
    }
    labels
}

fn load(name: &str) -> Fixture {
    load_arm(name, &[])
}

// --- A: series-chain axis uniformity -------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Axis {
    Horizontal,
    Vertical,
}

/// A member's drawn pin axis, or `None` when it is neither (which can
/// only happen off-grid, and is always counted as off-axis).
fn member_axis(f: &Fixture, refdes: &str) -> Option<Axis> {
    let ps = f.pins_of(refdes);
    if ps.len() != 2 {
        return None;
    }
    let dx = (ps[0].x_mm - ps[1].x_mm).abs();
    let dy = (ps[0].y_mm - ps[1].y_mm).abs();
    match (dx > TOL_MM, dy > TOL_MM) {
        (true, false) => Some(Axis::Horizontal),
        (false, true) => Some(Axis::Vertical),
        _ => None,
    }
}

/// One series chain, in path order.
#[derive(Debug)]
struct Chain {
    /// Members, ordered along the path.
    members: Vec<String>,
    /// `entry`/`exit` net for each member, in the same order — the net
    /// the chain arrives on and the net it leaves by. Endpoints use
    /// their free (non-chain) net for the missing side.
    ports: Vec<(String, String)>,
}

/// Every maximal series chain of two or more members.
///
/// Chain candidates are the drawn, two-pin, series-signal elements; two
/// candidates are linked when they share a non-rail net of signal degree
/// exactly 2. Every candidate has at most two nets, so every vertex has
/// chain-degree <= 2 and each component is a path or a cycle by
/// construction.
#[allow(clippy::too_many_lines)] // one cohesive walk; splitting it hides the traversal
fn chains(f: &Fixture) -> Vec<Chain> {
    let mut candidates: Vec<String> = f
        .elements
        .iter()
        .map(|e| e.refdes.clone())
        .filter(|r| f.is_series_signal(r) && f.pins_of(r).len() == 2)
        .collect();
    candidates.sort();
    let candidate_set: BTreeSet<&str> = candidates.iter().map(String::as_str).collect();

    // refdes -> the (net, neighbour) links it takes part in.
    let mut links: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();
    let mut nets: BTreeSet<&str> = BTreeSet::new();
    for r in &candidates {
        for n in &f.elem(r).expect("candidate is an element").nodes {
            nets.insert(n.as_str());
        }
    }
    for net in nets {
        if f.is_rail_net(net) {
            continue;
        }
        let members = f.signal_members(net);
        if members.len() != 2 {
            continue;
        }
        let (a, b) = (members[0], members[1]);
        if !candidate_set.contains(a) || !candidate_set.contains(b) {
            continue;
        }
        links
            .entry(a)
            .or_default()
            .push((net.to_string(), b.to_string()));
        links
            .entry(b)
            .or_default()
            .push((net.to_string(), a.to_string()));
    }

    // Walk each component from an endpoint (chain-degree <= 1). A cycle
    // has no endpoint; break it at its lexicographically smallest member
    // so the walk is deterministic (ADR-28 records this choice).
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut out: Vec<Chain> = Vec::new();
    let starts = candidates
        .iter()
        .filter(|r| links.get(r.as_str()).map_or(0, Vec::len) <= 1)
        .chain(candidates.iter());
    for start in starts {
        let start = start.as_str();
        if seen.contains(start) || !candidate_set.contains(start) {
            continue;
        }
        let mut order: Vec<&str> = vec![start];
        let mut via: Vec<String> = Vec::new(); // net joining order[i] and order[i+1]
        seen.insert(start);
        let mut cur = start;
        let mut prev: Option<&str> = None;
        loop {
            let Some(next) = links.get(cur).and_then(|ls| {
                ls.iter()
                    .find(|(_, nb)| Some(nb.as_str()) != prev && !seen.contains(nb.as_str()))
            }) else {
                break;
            };
            let nb = candidate_set
                .get(next.1.as_str())
                .copied()
                .expect("neighbour is a candidate");
            via.push(next.0.clone());
            order.push(nb);
            seen.insert(nb);
            prev = Some(cur);
            cur = nb;
        }
        if order.len() < 2 {
            continue;
        }
        // Entry/exit nets per member.
        let mut ports: Vec<(String, String)> = Vec::with_capacity(order.len());
        for (i, r) in order.iter().enumerate() {
            let nodes = &f.elem(r).expect("member is an element").nodes;
            let other = |used: &str| -> String {
                nodes
                    .iter()
                    .find(|n| n.as_str() != used)
                    .cloned()
                    .unwrap_or_else(|| used.to_string())
            };
            let exit = if i < via.len() {
                via[i].clone()
            } else {
                other(&via[i - 1])
            };
            let entry = if i == 0 {
                other(&via[0])
            } else {
                via[i - 1].clone()
            };
            ports.push((entry, exit));
        }
        // The walk direction is arbitrary (both endpoints are valid
        // starts) and every count below is invariant under reversing it,
        // so orient the chain the way a reader scans the sheet:
        // left-to-right, then top-to-bottom. Cosmetic only.
        let mut members: Vec<String> = order.iter().map(|s| (*s).to_string()).collect();
        let key = |r: &str| f.centre(r).unwrap_or((0.0, 0.0));
        let (first, last) = (key(&members[0]), key(members.last().expect("non-empty")));
        if (last.0, last.1) < (first.0, first.1) {
            members.reverse();
            ports.reverse();
            for p in &mut ports {
                std::mem::swap(&mut p.0, &mut p.1);
            }
        }
        out.push(Chain { members, ports });
    }
    out.sort_by(|a, b| a.members.cmp(&b.members));
    out
}

/// A member's direction of travel along the chain, as a unit step: the
/// sign of `exit_pin - entry_pin` on its dominant component.
fn travel(f: &Fixture, refdes: &str, entry: &str, exit: &str) -> Option<(i32, i32)> {
    let ps = f.pins_of(refdes);
    if ps.len() != 2 {
        return None;
    }
    let e = ps.iter().find(|p| p.net == entry)?;
    let x = ps.iter().find(|p| p.net == exit)?;
    let dx = x.x_mm - e.x_mm;
    let dy = x.y_mm - e.y_mm;
    if dx.abs() > dy.abs() {
        Some((if dx > 0.0 { 1 } else { -1 }, 0))
    } else if dy.abs() > TOL_MM {
        Some((0, if dy > 0.0 { 1 } else { -1 }))
    } else {
        None
    }
}

/// Per-chain `(off-axis members, reversed members)`, with the offending
/// refdeses, under the axis that reads the chain most charitably.
///
/// Charitable, not canonical: the metric picks whichever of the two axes
/// minimises `(off_axis, reversals)` lexicographically, so it never
/// invents a violation by insisting on an axis the drawing did not
/// choose. Ties break toward HORIZONTAL — the project's own convention
/// is that signal flows left to right (F3/F5), so a chain drawn half
/// horizontal and half vertical is graded against the horizontal half.
fn score_chain(f: &Fixture, c: &Chain) -> (Vec<String>, Vec<String>) {
    let mut best: Option<(usize, usize, Vec<String>, Vec<String>)> = None;
    for axis in [Axis::Horizontal, Axis::Vertical] {
        let mut off: Vec<String> = Vec::new();
        let mut on: Vec<(&str, (i32, i32))> = Vec::new();
        for (i, r) in c.members.iter().enumerate() {
            if member_axis(f, r) == Some(axis) {
                let (entry, exit) = &c.ports[i];
                if let Some(t) = travel(f, r, entry, exit) {
                    on.push((r.as_str(), t));
                } else {
                    off.push(r.clone());
                }
            } else {
                off.push(r.clone());
            }
        }
        // Majority direction among the on-axis members.
        let mut tally: BTreeMap<(i32, i32), usize> = BTreeMap::new();
        for (_, t) in &on {
            *tally.entry(*t).or_default() += 1;
        }
        let majority = tally
            .iter()
            .max_by_key(|(k, v)| (**v, **k))
            .map(|(k, _)| *k);
        let reversed: Vec<String> = on
            .iter()
            .filter(|(_, t)| Some(*t) != majority)
            .map(|(r, _)| (*r).to_string())
            .collect();
        let cand = (off.len(), reversed.len(), off, reversed);
        if best.as_ref().is_none_or(|b| (cand.0, cand.1) < (b.0, b.1)) {
            best = Some(cand);
        }
    }
    let (_, _, off, rev) = best.expect("two axes were tried");
    (off, rev)
}

/// `(chain.axis, chain.reversal, chain.members)` for a fixture, plus the
/// human-readable detail.
fn chain_metrics(f: &Fixture) -> (usize, usize, usize, Vec<String>) {
    let cs = chains(f);
    let (mut axis, mut rev, mut members) = (0, 0, 0);
    let mut detail = Vec::new();
    for c in &cs {
        let (off, reversed) = score_chain(f, c);
        members += c.members.len();
        axis += off.len();
        rev += reversed.len();
        detail.push(format!(
            "chain [{}]: axes {:?}, off-axis {off:?}, reversed {reversed:?}",
            c.members.join(" -> "),
            c.members
                .iter()
                .map(|r| member_axis(f, r))
                .collect::<Vec<_>>()
        ));
    }
    (axis, rev, members, detail)
}

// --- B: shared-current-path stacking -------------------------------------

/// The two terminals a DC current flows between, for the elements that
/// conduct DC at all.
///
/// * two-terminal conductors (R, L, D, and a *drawn* V/I source) use
///   their two nodes;
/// * a BJT uses collector-emitter, a FET drain-source. The base / gate
///   is deliberately NOT a DC edge: SPICE order is `c b e` / `d g s`,
///   and the control terminal carries either no DC (a gate) or the
///   current path's `1/beta` (a base). Treating a base as a conductor
///   would raise the DC degree of every bias node and dissolve exactly
///   the rail-to-rail divider this metric exists to see. ADR-28 records
///   this as a deliberate choice with a stated alternative;
/// * capacitors conduct no DC and have no edge — which is why an RC
///   low-pass's `R1`/`C1` is not asked to stack;
/// * `;@ power=` sources are glyphs, not bodies, and multi-terminal
///   subckt instances have no single current path, so neither gets an edge.
fn dc_edge(f: &Fixture, e: &Elem) -> Option<(String, String)> {
    if e.is_power_source || e.is_sheet || !f.has_body(&e.refdes) {
        return None;
    }
    let pick = |i: usize, j: usize| -> Option<(String, String)> {
        let (a, b) = (e.nodes.get(i)?, e.nodes.get(j)?);
        if a == b {
            return None;
        }
        Some((a.clone(), b.clone()))
    };
    match e.kind {
        ElementKind::Resistor
        | ElementKind::Inductor
        | ElementKind::Diode
        | ElementKind::VoltageSrc
        | ElementKind::CurrentSrc => pick(0, 1),
        ElementKind::Bjt | ElementKind::Mosfet | ElementKind::Jfet => pick(0, 2),
        _ => None,
    }
}

/// Rail nets reachable from `from` in the DC graph with `skip`'s edge
/// removed.
fn rails_reachable(
    f: &Fixture,
    adj: &BTreeMap<&str, Vec<(&str, &str)>>, // net -> [(other net, via refdes)]
    from: &str,
    skip: &str,
) -> BTreeSet<String> {
    let mut rails = BTreeSet::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut q: VecDeque<&str> = VecDeque::new();
    let Some((k, _)) = adj.get_key_value(from).map(|(k, v)| (*k, v)) else {
        if f.is_rail_net(from) {
            rails.insert(from.to_string());
        }
        return rails;
    };
    seen.insert(k);
    q.push_back(k);
    while let Some(n) = q.pop_front() {
        if f.is_rail_net(n) {
            rails.insert(n.to_string());
            // A rail is a terminus: current entering it leaves through
            // the supply, not through the next signal net.
            continue;
        }
        for (other, via) in adj.get(n).into_iter().flatten() {
            if *via == skip || seen.contains(other) {
                continue;
            }
            seen.insert(other);
            q.push_back(other);
        }
    }
    if f.is_rail_net(from) {
        rails.insert(from.to_string());
    }
    rails
}

/// The nets at an element's DC-relevant terminals — every terminal a DC
/// current can enter or leave by.
///
/// A *stronger* notion than "has a DC edge", and deliberately so. An
/// op-amp symbol or a hierarchical `(sheet …)` instance conducts DC at
/// its pins without having any single current path *through* it, so it
/// has no DC edge — but a net it sits on is emphatically not a
/// two-element series node, and must not be read as one. Counting its
/// terminals here is what stops `opamp_inverting`'s virtual ground from
/// reading as `RIN` in series with `RF`.
fn dc_terminals(e: &Elem) -> Vec<&str> {
    if e.is_power_source {
        return Vec::new();
    }
    match e.kind {
        // No DC through a capacitor, at either end.
        ElementKind::Capacitor => Vec::new(),
        // `c b e` / `d g s [b]`: index 1 is the control terminal.
        ElementKind::Bjt | ElementKind::Mosfet | ElementKind::Jfet => e
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, n)| n.as_str())
            .collect(),
        _ => e.nodes.iter().map(String::as_str).collect(),
    }
}

/// Every DC-series pair, as `(u, v, shared net)`. See the module docs.
fn dc_series_pairs(f: &Fixture) -> Vec<(String, String, String)> {
    let edges: BTreeMap<&str, (String, String)> = f
        .elements
        .iter()
        .filter_map(|e| dc_edge(f, e).map(|ab| (e.refdes.as_str(), ab)))
        .collect();

    let mut adj: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for (r, (a, b)) in &edges {
        adj.entry(a.as_str()).or_default().push((b.as_str(), r));
        adj.entry(b.as_str()).or_default().push((a.as_str(), r));
    }

    // Which elements sit on a path between two DISTINCT rail nets?
    let mut conducts: BTreeSet<&str> = BTreeSet::new();
    for (r, (a, b)) in &edges {
        let ra = rails_reachable(f, &adj, a, r);
        let rb = rails_reachable(f, &adj, b, r);
        if ra.iter().any(|x| rb.iter().any(|y| x != y)) {
            conducts.insert(r);
        }
    }

    // DC degree, over EVERY element's DC terminals — not just the ones
    // with an edge. See `dc_terminals`.
    let mut degree: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in &f.elements {
        for net in dc_terminals(e) {
            degree.entry(net).or_default().insert(e.refdes.as_str());
        }
    }

    let mut out = Vec::new();
    for (net, on) in &degree {
        if f.is_rail_net(net) || on.len() != 2 {
            continue;
        }
        let mut it = on.iter();
        let (u, v) = (*it.next().expect("two"), *it.next().expect("two"));
        // Both must actually carry the current *through* themselves…
        if !edges.contains_key(u) || !edges.contains_key(v) {
            continue;
        }
        // …and both must sit on a supply-to-ground path.
        if !conducts.contains(u) || !conducts.contains(v) {
            continue;
        }
        out.push((u.to_string(), v.to_string(), (*net).to_string()));
    }
    out.sort();
    out
}

/// `(stack.side_by_side, stack.pairs)` plus detail.
///
/// A pair is stacked when it is drawn taller than wide. `|dx| > |dy|` is
/// a violation; the exactly-diagonal case (`|dx| == |dy|`) is genuinely
/// ambiguous and is NOT counted, so the metric never invents a defect
/// out of a tie.
fn stacking_metrics(f: &Fixture) -> (usize, usize, Vec<String>) {
    let pairs = dc_series_pairs(f);
    let mut bad = Vec::new();
    for (u, v, net) in &pairs {
        let (Some((ux, uy)), Some((vx, vy))) = (f.centre(u), f.centre(v)) else {
            continue;
        };
        let (dx, dy) = ((ux - vx).abs(), (uy - vy).abs());
        if dx > dy + TOL_MM {
            bad.push(format!(
                "{u}/{v} (series on `{net}`) drawn side-by-side: |dx|={dx:.2} > |dy|={dy:.2}"
            ));
        }
    }
    (bad.len(), pairs.len(), bad)
}

// --- C: series-chain compactness -----------------------------------------

/// A member's own drawn length: the Manhattan distance between its two
/// body pins. This is the yardstick the adjacency threshold below is
/// stated in, and it is read off the emitted symbol, so the metric
/// carries no absolute millimetre constant and grades any symbol library.
fn member_extent(f: &Fixture, refdes: &str) -> Option<f64> {
    let ps = f.pins_of(refdes);
    if ps.len() != 2 {
        return None;
    }
    Some((ps[0].x_mm - ps[1].x_mm).abs() + (ps[0].y_mm - ps[1].y_mm).abs())
}

/// One consecutive link of a chain: the two members and the net that
/// joins them.
type Link = (String, String, String);

/// A chain's members and consecutive links **including its
/// rail-terminated ends**.
///
/// `chains()` deliberately excludes rail stubs, and for metric A that is
/// right: a stub's pose is fixed by its rail glyph (V14, a Tier-1 hard
/// constraint), so asking it to obey the chain's axis or direction would
/// demand a contradiction. Compactness asks a different question —
/// *does the current path read as ONE connected run of devices?* — and
/// it makes no demand on any member's pose. A terminating stub carries
/// the chain's own current and the reader's eye follows one wire into
/// it, so for adjacency purposes it is on the chain. `port_shapes`'s
/// `R4` is exactly that case, and its exclusion is half of why the
/// broken drawing scored a perfect 0 (ADR-28's blind spot).
///
/// Only a *terminus* is adopted, under the strict condition that the
/// endpoint's free net carries exactly two drawn elements: the chain
/// endpoint and one two-pin rail stub. A net with anything else on it is
/// a fan-out, not the end of the run. `chains()` itself is untouched, so
/// `chain.members`, `chain.axis`, `chain.reversal` and the
/// `chain_discriminator_keeps_rail_stubs_out_of_the_chain` contract are
/// all unchanged.
fn chain_run(f: &Fixture, c: &Chain) -> (Vec<String>, Vec<Link>) {
    // A two-pin rail stub sitting alone with `endpoint` on `net`.
    let terminus = |endpoint: &str, net: &str| -> Option<String> {
        if f.is_rail_net(net) {
            return None;
        }
        let on: Vec<&Elem> = f
            .elements
            .iter()
            .filter(|e| !e.is_power_source && e.nodes.iter().any(|n| n == net))
            .collect();
        if on.len() != 2 {
            return None;
        }
        let other = on.iter().find(|e| e.refdes != endpoint)?.refdes.clone();
        if f.is_rail_stub(&other) && f.pins_of(&other).len() == 2 {
            Some(other)
        } else {
            None
        }
    };

    let mut members = c.members.clone();
    let mut links: Vec<Link> = Vec::new();
    for i in 0..c.members.len() - 1 {
        links.push((
            c.members[i].clone(),
            c.members[i + 1].clone(),
            c.ports[i].1.clone(),
        ));
    }
    let head = c.members.first().expect("a chain has members").clone();
    if let Some(stub) = terminus(&head, &c.ports[0].0) {
        links.insert(0, (stub.clone(), head, c.ports[0].0.clone()));
        members.insert(0, stub);
    }
    let tail = c.members.last().expect("a chain has members").clone();
    let tail_net = c.ports.last().expect("ports match members").1.clone();
    if let Some(stub) = terminus(&tail, &tail_net) {
        links.push((tail, stub.clone(), tail_net));
        members.push(stub);
    }
    (members, links)
}

/// The drawn separation of one link: the **Manhattan** distance between
/// the two members' pins on the shared net.
///
/// Manhattan, not Euclidean, because a schematic's connection is
/// rectilinear ink — this is the length of wire the eye has to follow
/// from one device to the next, and an L-shaped jog costs what it draws.
/// Pins, not centres: the quantity of interest is the empty sheet
/// *between* the bodies, and a body's own half-length varies with its
/// pose, which has nothing to do with how far apart the two devices sit.
fn link_run(f: &Fixture, u: &str, v: &str, net: &str) -> Option<f64> {
    let pu = f.pins_of(u).into_iter().find(|p| p.net == net)?;
    let pv = f.pins_of(v).into_iter().find(|p| p.net == net)?;
    Some((pu.x_mm - pv.x_mm).abs() + (pu.y_mm - pv.y_mm).abs())
}

/// `(chain.stranded, chain.run_members)` plus detail.
///
/// **Adjacency.** Two consecutive members are adjacent when the wire
/// between them is no longer than **all of the chain's device bodies
/// laid end to end** (`Σ extent` over the extended chain). One hop that
/// swallows more sheet than every device in the chain occupies is not
/// spacing, it is a *break*: the reader stops seeing one run.
///
/// The threshold is stated in the chain's own drawn material rather than
/// in millimetres, on purpose — see ADR-28 for the two alternatives that
/// were measured and rejected, both of which rank the textbook ladder
/// WORST. Because the threshold scales with the whole chain, generous
/// but *even* spacing is not a defect: `lc_ladder_lpf` under
/// `--placer=flow-seed-v4` strides 22.86 mm between members against a
/// 40.64 mm body and reads clean, while `port_shapes`'s single 41.91 mm
/// jump against a 30.48 mm body does not. That matches the project's own
/// measured finding that spacing *along* the flow is slack while the
/// structure across it is meaning (ADR-17).
///
/// **The count.** Unbroken links partition the extended chain into
/// maximal adjacent *clusters*; `chain.stranded` is the number of
/// members outside the largest one. Members, not links, so the unit
/// matches `chain.axis` / `chain.reversal` (which also count members),
/// and so the number says what a reader sees: *this many devices were
/// drawn away from the rest of their chain*. `total − max cluster` is
/// symmetric, so no arbitrary "which cluster is the main one" tie-break
/// enters the number.
fn compactness_metrics(f: &Fixture) -> (usize, usize, Vec<String>) {
    let cs = chains(f);
    let (mut stranded, mut total) = (0usize, 0usize);
    let mut detail = Vec::new();
    for c in &cs {
        let (members, links) = chain_run(f, c);
        let body: f64 = members
            .iter()
            .filter_map(|r| member_extent(f, r))
            .sum::<f64>();
        // Cluster sizes, walking the chain in order.
        let mut clusters: Vec<usize> = vec![1];
        let mut broken: Vec<String> = Vec::new();
        let mut runs: Vec<String> = Vec::new();
        for (u, v, net) in &links {
            if let Some(run) = link_run(f, u, v, net) {
                runs.push(format!("{run:.2}"));
            }
            let cut = match link_run(f, u, v, net) {
                Some(run) if run > body + TOL_MM => {
                    broken.push(format!(
                        "{u}..{v} on `{net}`: run {run:.2} > body {body:.2}"
                    ));
                    true
                }
                // An unmeasurable link (a member without two emitted
                // body pins) is NOT a break: the metric must not invent
                // a defect out of geometry it could not read.
                _ => false,
            };
            if cut {
                clusters.push(1);
            } else {
                *clusters.last_mut().expect("seeded with one cluster") += 1;
            }
        }
        let largest = clusters.iter().copied().max().unwrap_or(0);
        stranded += members.len() - largest;
        total += members.len();
        detail.push(format!(
            "chain [{}]: body {body:.2}, runs [{}], clusters {clusters:?}, broken {broken:?}",
            members.join(" -> "),
            runs.join(" ")
        ));
    }
    (stranded, total, detail)
}

// --- D: port-terminal label direction ------------------------------------

/// Metric D, in two disjoint counts over one population: every top-level
/// `(global_label …)` the emitter draws.
///
/// Returns `(vertical, backwards, labels, directed, detail)`.
///
/// # What a rotation means, read off the renderer
///
/// KiCad's parser maps the file angle straight onto a spin style
/// (`sch_io_kicad_sexpr_parser.cpp:4653`) and `SetSpinStyle`
/// (`sch_label.cpp:395`) turns that into a text angle plus a
/// justification:
///
/// | rot | spin   | text angle        | justify | tag sits |
/// | --: | ------ | ----------------- | ------- | -------- |
/// |   0 | RIGHT  | `ANGLE_HORIZONTAL`| left    | right of the anchor |
/// |  90 | UP     | `ANGLE_VERTICAL`  | left    | above    |
/// | 180 | LEFT   | `ANGLE_HORIZONTAL`| right   | left of the anchor  |
/// | 270 | BOTTOM | `ANGLE_VERTICAL`  | right   | below    |
///
/// Two consequences, and both are the metric:
///
/// **90 and 270 are the same glyph rotation.** Both set
/// `ANGLE_VERTICAL`; they differ only in which side of the anchor the
/// tag hangs. So there is no "readable vertical" option to prefer — a
/// terminal at either angle asks the reader to tilt their head, on a
/// sheet whose signal path runs horizontally. That is `vertical`.
///
/// **0 and 180 are the same glyph rotation too**, and differ in which
/// way the tag's arrowhead points.
/// `SCH_GLOBALLABEL::CreateGraphicShape` (`sch_label.cpp:2146`) builds
/// the outline from the anchor backwards along the reading direction and
/// then points ONE end: `L_INPUT` points the end at the anchor,
/// `L_OUTPUT` points the far end. So an `input` at 180 and an `output`
/// at 0 both draw an arrow travelling **rightward**, and an `input` at 0
/// or an `output` at 180 both draw one travelling **leftward** — the
/// signal entering from the right edge, or leaving by the left. Against
/// the project's own left-to-right flow convention (F3/F5, and the
/// placer's X = signal depth) that is a terminal drawn against the
/// stream. That is `backwards`.
///
/// # Why `backwards` is graded only on DECLARED ports
///
/// The `(shape …)` token is not a measurement unless the source made a
/// statement. `label_specs` stamps `shape: "input"` on every undeclared
/// one-pin interface net whatever its real direction — `common_emitter`'s
/// `out` is emitted `(shape input)` — so grading direction there grades
/// the emitter's default, not the drawing. Concretely: both challenger
/// arms move that `out` marker from 270 to 0, which is a repair, and a
/// rule that read the defaulted token would score the repair as a NEW
/// backwards violation. So `backwards` reads `declared`, which comes
/// from the netlist's `*@port`, and `bidirectional` is exempt because it
/// asserts no direction.
///
/// `vertical` needs no direction, so it grades the whole population.
///
/// The two counts are **disjoint**: a vertical label is counted once,
/// under `vertical`, and is never also backwards. That is ADR-28
/// ambiguity 5's rule — one element, one blame — and it is what
/// separates an axis defect (repair: the placer, by turning the terminal
/// pin) from a direction defect (repair: the anchor/rotation chooser in
/// `label_specs`).
fn port_label_metrics(f: &Fixture) -> (usize, usize, usize, usize, Vec<String>) {
    let (mut vertical, mut backwards, mut directed) = (0usize, 0usize, 0usize);
    let mut detail = Vec::new();
    for l in &f.labels {
        let dir = match l.declared {
            Some(d @ (PortDir::Input | PortDir::Output)) => {
                directed += 1;
                Some(d)
            }
            _ => None,
        };
        if l.rot == 90 || l.rot == 270 {
            vertical += 1;
            detail.push(format!(
                "`{}` ({}) at {}: vertical — reads across the signal path",
                l.net, l.shape, l.rot
            ));
            continue;
        }
        let flow_rightward = match dir {
            Some(PortDir::Input) => l.rot == 180,
            Some(PortDir::Output) => l.rot == 0,
            _ => true,
        };
        if !flow_rightward {
            backwards += 1;
            detail.push(format!(
                "`{}` ({}) at {}: backwards — arrow travels leftward",
                l.net, l.shape, l.rot
            ));
        }
    }
    (vertical, backwards, f.labels.len(), directed, detail)
}

// --- the fixtures --------------------------------------------------------

// --- D: device facing (F2) -----------------------------------------------

/// The DC graph as `net -> [(other net, via refdes)]`, shared by metric
/// B's reachability walk and metric D's rank. Built from [`dc_edge`], so
/// the two can never disagree about what conducts.
fn dc_graph(f: &Fixture) -> BTreeMap<&str, Vec<(&str, &str)>> {
    let mut adj: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for e in &f.elements {
        let Some((a, b)) = dc_edge(f, e) else {
            continue;
        };
        // `dc_edge` returns owned Strings; re-borrow from the element's
        // own nodes so the adjacency can hold `&str` for the walk.
        let find = |want: &str| -> Option<&str> {
            e.nodes.iter().map(String::as_str).find(|n| *n == want)
        };
        let (Some(a), Some(b)) = (find(&a), find(&b)) else {
            continue;
        };
        adj.entry(a).or_default().push((b, e.refdes.as_str()));
        adj.entry(b).or_default().push((a, e.refdes.as_str()));
    }
    adj
}

/// Hop distance from every net to the nearest rail of one page polarity,
/// with `skip`'s edge removed.
///
/// A rail is **absorbing**: reaching one records the distance and stops.
/// Current entering a rail leaves through the supply, not out the far
/// side into the next signal net, and letting the walk continue would
/// manufacture paths that run *through* the power supply.
///
/// A net absent from the result is unreachable — treated as infinitely
/// far, so an unreachable terminal loses every strict comparison and the
/// device declines.
fn rail_hops(
    f: &Fixture,
    adj: &BTreeMap<&str, Vec<(&str, &str)>>,
    down: bool,
    skip: &str,
) -> BTreeMap<String, usize> {
    let mut dist: BTreeMap<String, usize> = BTreeMap::new();
    let mut q: VecDeque<(&str, usize)> = VecDeque::new();
    let mut roots: Vec<&str> = f
        .rail_nets
        .iter()
        .map(String::as_str)
        .filter(|n| f.is_down_rail(n) == down)
        .collect();
    roots.sort_unstable();
    for r in roots {
        dist.insert(r.to_string(), 0);
        q.push_back((r, 0));
    }
    while let Some((net, d)) = q.pop_front() {
        // A rail that is not a root of THIS walk is a terminus.
        if d > 0 && f.is_rail_net(net) {
            continue;
        }
        for (other, via) in adj.get(net).into_iter().flatten() {
            if *via == skip || dist.contains_key(*other) {
                continue;
            }
            dist.insert((*other).to_string(), d + 1);
            q.push_back((other, d + 1));
        }
    }
    dist
}

/// The net of `refdes`'s conduction terminal that must be drawn ABOVE
/// the other, as `(hi net, lo net)` — or `None` when the rank declines.
///
/// The device's own edge is removed from the walk: ranking a device by a
/// path through itself is circular, and would make every transistor read
/// "collector one hop further from ground than emitter" however it is
/// wired.
///
/// Declines on a tie, on the two axes disagreeing, and on both terminals
/// being unreachable — a floating pass transistor, a bidirectional
/// switch, a symmetric use. Declining is a real answer, not a failure.
fn facing_of(f: &Fixture, refdes: &str) -> Option<(String, String)> {
    let el = f.elem(refdes)?;
    if !matches!(
        el.kind,
        ElementKind::Bjt | ElementKind::Mosfet | ElementKind::Jfet
    ) {
        return None;
    }
    // `dc_edge` returns `(nodes[0], nodes[2])` for a device: collector /
    // drain first, emitter / source second.
    let (a, b) = dc_edge(f, el)?;
    let adj = dc_graph(f);
    let up = rail_hops(f, &adj, false, refdes);
    let dn = rail_hops(f, &adj, true, refdes);
    let hops = |m: &BTreeMap<String, usize>, n: &str| m.get(n).copied().unwrap_or(usize::MAX);
    let (ua, ub) = (hops(&up, &a), hops(&up, &b));
    let (da, db) = (hops(&dn, &a), hops(&dn, &b));
    if ua < ub && da > db {
        Some((a, b))
    } else if ub < ua && db > da {
        Some((b, a))
    } else {
        None
    }
}

/// `(device.facing_inverted, device.facing_resolved, detail)`.
///
/// A device is **inverted** when its higher-DC-potential terminal is
/// drawn strictly BELOW the other — world Y grows downward in eeschema,
/// so `y(hi) > y(lo)`. A device drawn on its side (both terminals at the
/// same height) is not counted: the convention is about which terminal
/// is on top, and a horizontal pose has no answer to that.
fn facing_metrics(f: &Fixture) -> (usize, usize, Vec<String>) {
    let mut inverted = 0usize;
    let mut resolved = 0usize;
    let mut detail = Vec::new();
    let mut refdeses: Vec<&str> = f.elements.iter().map(|e| e.refdes.as_str()).collect();
    refdeses.sort_unstable();
    for r in refdeses {
        let Some((hi, lo)) = facing_of(f, r) else {
            continue;
        };
        let pin = |net: &str| f.pins_of(r).into_iter().find(|p| p.net == net).cloned();
        let (Some(p_hi), Some(p_lo)) = (pin(&hi), pin(&lo)) else {
            continue;
        };
        resolved += 1;
        if p_hi.y_mm > p_lo.y_mm + TOL_MM {
            inverted += 1;
            detail.push(format!(
                "facing {r}: `{hi}` (y={:.2}) drawn BELOW `{lo}` (y={:.2})",
                p_hi.y_mm, p_lo.y_mm
            ));
        }
    }
    (inverted, resolved, detail)
}

/// Every fixture the suite converts, in `flow_geometry.rs`'s order.
/// There are no budget literals here on purpose — see the module docs.
const FIXTURES: &[&str] = &[
    "rc_lowpass",
    "rc_lowpass_ports",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting",
    "opamp_inverting_real",
    "port_shapes",
    "opamp_definition_level",
    "named_rails",
    "rc_phase_shift",
    "two_stage_amp",
    "cascode_amp",
    "lc_ladder_lpf",
    "sallen_key_lpf",
    "wien_bridge_osc",
    "sallen_key_driven",
    "shunt_feedback_amp",
    "stepped_attenuator",
    "opamp_transimpedance",
    "resistor_ladder_ref",
    "compensated_divider",
];

/// Report both metrics for every fixture to the ADR-23 sink.
///
/// **Informational: this test asserts no budget.** ADR-23 D2's contract
/// is that a fixture-enumerating verifier records on the line before its
/// assertion; the promotion's own post-mortem ("two blind cells") is
/// what a metric that reports nothing costs. So the recording is the
/// point of this test, and the only assertion is that the measurement
/// actually ran on every fixture — a metric silently absent is worse
/// than a metric that reads badly.
///
/// `S2K_READABILITY_DUMP=1` prints the per-fixture table and the
/// offending refdeses.
#[test]
fn readability_metrics_are_reported_for_every_fixture() {
    let dump = std::env::var("S2K_READABILITY_DUMP").is_ok();
    if dump {
        println!(
            "{:<24} {:>10} {:>10} {:>8} {:>10} {:>6} {:>10} {:>6} {:>8} {:>8} {:>9} {:>10} {:>7} {:>9}",
            "fixture",
            "chain.axis",
            "chain.rev",
            "members",
            "chain.strand",
            "run",
            "stack.sbs",
            "pairs",
            "face.inv",
            "face.res",
            "port.vert",
            "port.backw",
            "labels",
            "directed"
        );
    }
    let mut measured = 0usize;
    for name in FIXTURES {
        let f = load(name);
        let (axis, rev, members, chain_detail) = chain_metrics(&f);
        let (strand, strand_n, strand_detail) = compactness_metrics(&f);
        let (sbs, pairs, stack_detail) = stacking_metrics(&f);
        let (finv, fres, facing_detail) = facing_metrics(&f);
        let (pvert, pback, plabels, pdirected, port_detail) = port_label_metrics(&f);
        common::scoreboard::record_count("chain.axis", name, axis);
        common::scoreboard::record_count("chain.reversal", name, rev);
        common::scoreboard::record_count("chain.members", name, members);
        common::scoreboard::record_count("chain.stranded", name, strand);
        common::scoreboard::record_count("chain.run_members", name, strand_n);
        common::scoreboard::record_count("stack.side_by_side", name, sbs);
        common::scoreboard::record_count("stack.pairs", name, pairs);
        common::scoreboard::record_count("device.facing_inverted", name, finv);
        common::scoreboard::record_count("device.facing_resolved", name, fres);
        common::scoreboard::record_count("port.label_vertical", name, pvert);
        common::scoreboard::record_count("port.label_backwards", name, pback);
        common::scoreboard::record_count("port.labels", name, plabels);
        common::scoreboard::record_count("port.directed", name, pdirected);
        measured += 1;
        if dump {
            println!(
                "{:<24} {axis:>10} {rev:>10} {members:>8} {strand:>10} {strand_n:>6} {sbs:>10} {pairs:>6} {finv:>8} {fres:>8} {pvert:>9} {pback:>10} {plabels:>7} {pdirected:>9}",
                f.name
            );
            for d in chain_detail
                .iter()
                .chain(strand_detail.iter())
                .chain(stack_detail.iter())
                .chain(facing_detail.iter())
                .chain(port_detail.iter())
            {
                println!("      {d}");
            }
        }
    }
    assert_eq!(
        measured,
        FIXTURES.len(),
        "every fixture must be measured — a blind cell is not conservatively blind (ADR-23)"
    );
}

// --- specimen rankings ---------------------------------------------------

/// **The acceptance test for metric A.** `lc_ladder_lpf`'s
/// `RS -> L1 -> L2 -> L3` is one series chain; the deterministic seed
/// draws all four members on one axis in one direction, and the shipped
/// drawing draws them at four different rotations. A metric that does
/// not rank the seed better is measuring the wrong thing.
///
/// Asserted in `<=` form on purpose. It fires only if the drawing the
/// owner calls textbook becomes strictly WORSE than the one they call
/// damage — i.e. only if the metric's own validation inverts. It can
/// never block a change that *improves* the shipping placer, which is
/// what "informational at birth" requires (ADR-28). Non-vacuity is
/// carried by the synthetic control arms below, not by this test.
#[test]
fn chain_axis_ranks_the_textbook_ladder_seed_above_the_shipped_drawing() {
    let seed = load_arm("lc_ladder_lpf", &["--no-refine"]);
    let shipped = load("lc_ladder_lpf");

    let seed_chains = chains(&seed);
    assert_eq!(
        seed_chains.len(),
        1,
        "lc_ladder_lpf must present exactly one series chain; got {:?}",
        seed_chains
            .iter()
            .map(|c| c.members.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        seed_chains[0].members,
        vec!["RS", "L1", "L2", "L3"],
        "the chain must be the ladder's series path, in order"
    );

    let (s_axis, s_rev, _, s_detail) = chain_metrics(&seed);
    let (p_axis, p_rev, _, p_detail) = chain_metrics(&shipped);
    println!("lc_ladder_lpf seed     : axis={s_axis} reversal={s_rev}  {s_detail:?}");
    println!("lc_ladder_lpf shipped  : axis={p_axis} reversal={p_rev}  {p_detail:?}");

    assert!(
        s_axis <= p_axis && s_rev <= p_rev,
        "metric A inverted the specimen ranking: the --no-refine seed \
         (axis={s_axis}, reversal={s_rev}) must not read worse than the shipped drawing \
         (axis={p_axis}, reversal={p_rev}).\n  seed:    {s_detail:?}\n  shipped: {p_detail:?}"
    );
}

/// **The acceptance test for metric C**, and the case ADR-28's own
/// metric was blind to.
///
/// `port_shapes` is a four-resistor series chain `src → R1 → ni → R2 →
/// no → R3 → nb → R4 → 0`. The shipping placer draws it as **two
/// vertical stacks of two, 31.75 mm apart**, joined by a wire that jumps
/// sideways mid-chain — and it scores `chain.axis = 0,
/// chain.reversal = 0`, a perfect pair, because the three surviving
/// members happen to share one axis and one direction. A challenger that
/// repaired it into one connected folded path scored `1 / 1` — the
/// metric ranked the broken drawing **above** the better one.
///
/// Three drawings, best to worst, and `chain.stranded` must order them:
///
/// 1. `lc_ladder_lpf` under `--placer=flow-seed-v4` — the textbook
///    ladder, five members on one line;
/// 2. `port_shapes` as one connected folded path;
/// 3. `port_shapes` as two disconnected stacks.
///
/// Arms 2 and 3 are **transcribed pin-for-pin** from the two emitted
/// `.kicad_sch` files rather than re-converted, and for arm 3 that is
/// load-bearing: it is the drawing the *shipping* placer makes today, so
/// asserting `mid < worst` strictly against a live conversion of it
/// would make this test block the very repair the metric exists to
/// motivate. Pinned geometry is what keeps metric C informational at
/// birth. The live default's score is printed, never asserted; it is
/// recorded per fixture by
/// `readability_metrics_are_reported_for_every_fixture`.
///
/// Arm 2's placer, `--placer=divider-rails`, was not on `master` when
/// this metric was written and **is** now. It is therefore converted for
/// real alongside its transcription and checked in `<=` form — the live
/// arm may improve on the transcription, never read worse. That keeps
/// the transcription from silently going stale if that placer changes,
/// without letting a change to it be blocked here.
#[test]
fn chain_stranded_ranks_the_ladder_the_fold_and_the_shattered_chain() {
    let ladder = load_arm("lc_ladder_lpf", &["--placer", "flow-seed-v4"]);
    let (best, best_n, best_detail) = compactness_metrics(&ladder);

    let fold = port_shapes_folded();
    let (mid, mid_n, mid_detail) = compactness_metrics(&fold);

    let shattered = port_shapes_shattered();
    let (worst, worst_n, worst_detail) = compactness_metrics(&shattered);

    let live_fold = load_arm("port_shapes", &["--placer", "divider-rails"]);
    let (live_mid, live_mid_n, live_mid_detail) = compactness_metrics(&live_fold);

    println!("lc_ladder_lpf flow-seed-v4 : stranded={best} of {best_n}  {best_detail:?}");
    println!("port_shapes   folded path  : stranded={mid} of {mid_n}  {mid_detail:?}");
    println!("port_shapes   two stacks   : stranded={worst} of {worst_n}  {worst_detail:?}");
    println!(
        "port_shapes   divider-rails: stranded={live_mid} of {live_mid_n}  {live_mid_detail:?}"
    );
    let live = load("port_shapes");
    let (l_strand, l_n, _) = compactness_metrics(&live);
    let (l_axis, l_rev, _, _) = chain_metrics(&live);
    println!(
        "port_shapes   live default : stranded={l_strand} of {l_n}, \
         axis={l_axis} reversal={l_rev}  (reported, never asserted)"
    );

    assert!(
        live_mid <= mid,
        "arm 2 is transcribed from `--placer=divider-rails`' own output; the live \
         conversion now reads WORSE ({live_mid}) than the transcription ({mid}), so \
         either that placer regressed or the transcription is stale.\n  live: \
         {live_mid_detail:?}\n  transcribed: {mid_detail:?}"
    );
    assert_eq!(
        (best_n, mid_n, worst_n, live_mid_n),
        (5, 4, 4, 4),
        "the measured population is a property of the NETLIST, not of the drawing: \
         the ladder's chain is VIN→RS→L1→L2→L3 and port_shapes' is R1→R2→R3→R4 on \
         all three of its arms"
    );
    assert!(
        best <= mid,
        "the textbook ladder ({best}) must not read worse than the folded path ({mid})"
    );
    assert!(
        mid < worst,
        "THE defect this metric exists for: the connected folded path ({mid}) must read \
         strictly better than the same chain shattered into two stacks ({worst}). \
         `chain.axis` / `chain.reversal` rank these two the other way round.\n  \
         fold:      {mid_detail:?}\n  shattered: {worst_detail:?}"
    );

    // And the thing the older pair cannot see, asserted on the same
    // pinned geometry so it stays true whatever the placer later does.
    let (f_axis, f_rev, _, _) = chain_metrics(&fold);
    let (s_axis, s_rev, _, _) = chain_metrics(&shattered);
    assert_eq!(
        ((s_axis, s_rev), (f_axis, f_rev)),
        ((0, 0), (1, 1)),
        "the premise: on these two drawings metric A scores the SHATTERED one perfect \
         (0, 0) and the repaired fold (1, 1) — strictly worse. That inversion is what \
         metric C exists to correct, and it is why C is not a refinement of A"
    );
}

/// **The acceptance test for metric B.** The owner prefers the
/// pre-promotion placer on `cascode_amp` specifically because it stacks
/// `Q1`/`Q2`; the shipping placer sets them side by side. Same `<=`
/// form, same reason, as the chain test above.
#[test]
fn stacking_ranks_the_champion_above_flow_seed_on_the_cascode() {
    let champion = load_arm("cascode_amp", &["--placer", "champion"]);
    let shipped = load("cascode_amp");

    let (c_sbs, c_pairs, c_detail) = stacking_metrics(&champion);
    let (s_sbs, s_pairs, s_detail) = stacking_metrics(&shipped);
    println!("cascode_amp champion : side_by_side={c_sbs} of {c_pairs} pairs  {c_detail:?}");
    println!("cascode_amp shipped  : side_by_side={s_sbs} of {s_pairs} pairs  {s_detail:?}");

    assert_eq!(
        c_pairs, s_pairs,
        "the DC-series pair set is a property of the NETLIST and must not depend on \
         the placer; champion saw {c_pairs} pairs, the shipping placer {s_pairs}"
    );
    assert!(
        c_pairs >= 5,
        "cascode_amp's own header names a two-transistor stack and a three-resistor \
         rail-to-ground ladder; the metric must see at least 5 DC-series pairs, saw {c_pairs}"
    );
    assert!(
        c_sbs <= s_sbs,
        "metric B inverted the specimen ranking: the champion ({c_sbs}) must not read \
         worse than the shipping placer ({s_sbs}) on the fixture the owner prefers the \
         champion for.\n  champion: {c_detail:?}\n  shipped:  {s_detail:?}"
    );
}

// --- discriminators: the assertions that keep the metrics falsifiable ----

/// Metric B says "devices in series on a DC current path stack in Y". If
/// the predicate silently widened to "any two devices sharing a net",
/// the metric would demand a **differential pair** be stacked — which is
/// wrong, and worse, a placer changed to satisfy it would score 0.
///
/// `cascode_amp` and `diff_pair` carry the discriminating pair: the
/// cascode's `Q1`/`Q2` share `c1`, whose only DC conductors are those
/// two, so all of Q1's collector current is Q2's emitter current — one
/// current, one column. `diff_pair`'s `Q1`/`Q2` share `tail` with
/// `RTAIL`, so the net has DC degree 3, the current *splits*, and the
/// conventional drawing is side by side. Same element kind, opposite
/// verdicts, decided from the netlist alone.
#[test]
fn stacking_discriminator_separates_the_cascode_from_the_diff_pair() {
    let cascode = load("cascode_amp");
    let pairs = dc_series_pairs(&cascode);
    let has = |a: &str, b: &str| {
        pairs
            .iter()
            .any(|(u, v, _)| (u == a && v == b) || (u == b && v == a))
    };
    assert!(
        has("Q1", "Q2"),
        "the cascode stack Q1/Q2 must be a DC-series pair; got {pairs:?}"
    );
    assert!(
        has("RC", "Q2"),
        "a collector load sits directly above its transistor; RC/Q2 must be a pair"
    );
    assert!(
        has("Q1", "RE"),
        "an emitter degeneration resistor sits directly below its transistor"
    );
    assert!(
        has("RB1", "RB2") && has("RB2", "RB3"),
        "the rail-to-ground bias ladder must be three stacked resistors; got {pairs:?}"
    );

    let dp = load("diff_pair");
    let dp_pairs = dc_series_pairs(&dp);
    assert!(
        !dp_pairs
            .iter()
            .any(|(u, v, _)| (u == "Q1" && v == "Q2") || (u == "Q2" && v == "Q1")),
        "a differential pair is drawn SIDE BY SIDE on purpose — its two transistors \
         share `tail` with RTAIL, so the current splits and they are not in series. \
         If this fires, metric B has widened into a demand that every pair stack: \
         {dp_pairs:?}"
    );
}

/// Metric A's mirror of the same rule. If the chain predicate widened to
/// "every two-terminal element", a bypass capacitor would be dragged
/// into the chain and asked to lie down sideways — ADR-15's recorded
/// "capacitors are horizontal is WRONG" trap.
///
/// `common_emitter` carries the discriminating pair: `CIN` (`in` -> `b`,
/// both signal nets) is a chain candidate; `CE` (`e` -> `0`, one rail
/// pin) is a rail stub and must never be one.
#[test]
fn chain_discriminator_keeps_rail_stubs_out_of_the_chain() {
    let f = load("common_emitter");
    assert!(
        f.is_rail_stub("CE") && !f.is_series_signal("CE"),
        "CE (bypass cap, one pin on ground) is a rail stub, never a chain member"
    );
    assert!(
        f.is_series_signal("CIN") && !f.is_rail_stub("CIN"),
        "CIN (in -> b, both signal nets) is a chain candidate"
    );
    for c in chains(&f) {
        assert!(
            !c.members.iter().any(|r| r == "CE"),
            "a rail stub reached a series chain: {:?}",
            c.members
        );
    }
}

// --- D: device facing ----------------------------------------------------

/// **The acceptance test for metric D**, and the defect it was written
/// for. `two_stage_amp`'s seed emits BOTH transistors upside down; the
/// shipping placer's phase 4.5 repairs `Q1` and never looks at `Q2`,
/// because at its post-SA position the flipped `Q2` carries no V5 and no
/// V12 violation of its own. The ADR-23 challenger `facing-trigger` adds
/// the reach.
///
/// Asserted in `<=` form, like metrics A / B / C: it fires only if the
/// arm that repairs the drawing reads strictly WORSE than the one that
/// leaves it flipped — i.e. only if the metric's own validation
/// inverts. It can never block a change that improves the shipping
/// placer. Non-vacuity is carried by the synthetic control arm below.
#[test]
fn facing_ranks_the_repaired_arm_above_the_shipped_two_stage_amp() {
    let shipped = load("two_stage_amp");
    let repaired = load_arm("two_stage_amp", &["--placer", "facing-trigger"]);

    let (s_inv, s_res, s_detail) = facing_metrics(&shipped);
    let (r_inv, r_res, r_detail) = facing_metrics(&repaired);
    println!("two_stage_amp shipped : inverted={s_inv}/{s_res} {s_detail:?}");
    println!("two_stage_amp repaired: inverted={r_inv}/{r_res} {r_detail:?}");

    assert!(
        s_res >= 2,
        "two_stage_amp has two NPN stages; the rank must resolve both, resolved {s_res}"
    );
    assert!(
        r_inv <= s_inv,
        "metric D inverted the specimen ranking: the arm that repairs the pose \
         ({r_inv} inverted) must not read worse than the one that leaves it flipped \
         ({s_inv}).\n  shipped:  {s_detail:?}\n  repaired: {r_detail:?}"
    );
}

/// The facing rank is derived from the **netlist**, so which devices
/// resolve must not depend on which placer drew the sheet. Only the
/// *pose* is a placer property; the denominator is not.
///
/// Same shape as metric B's `c_pairs == s_pairs` assertion, and it
/// exists for the same reason: a metric whose denominator moves with the
/// arm cannot compare two arms.
#[test]
fn the_facing_rank_is_a_property_of_the_netlist_not_the_placer() {
    for name in ["two_stage_amp", "cascode_amp", "diff_pair"] {
        let a = load(name);
        let b = load_arm(name, &["--placer", "champion"]);
        let (_, a_res, _) = facing_metrics(&a);
        let (_, b_res, _) = facing_metrics(&b);
        assert_eq!(
            a_res, b_res,
            "{name}: the resolved-device set is a property of the netlist; the shipping \
             placer saw {a_res}, the champion {b_res}"
        );
    }
}

/// **Non-vacuity, and the sign convention.** A hand-drawn common-emitter
/// stage: collector to the supply through `RC`, emitter to ground
/// through `RE`. Drawn collector-up it scores 0; drawn collector-down —
/// the same netlist, one pose changed — it scores 1. A metric that
/// always returns 0 fails the second half.
#[test]
fn facing_metric_counts_a_known_synthetic_inversion() {
    let els = vec![
        two_pin(ElementKind::Resistor, "RC", "vcc", "c"),
        two_pin(ElementKind::Resistor, "RE", "e", "0"),
        two_pin(ElementKind::Resistor, "RB", "vcc", "b"),
        three_pin(ElementKind::Bjt, "Q1", "c", "b", "e"),
    ];
    let passives = vec![
        pin("RC", "vcc", 0.0, 0.0),
        pin("RC", "c", 0.0, 10.0),
        pin("RE", "e", 0.0, 40.0),
        pin("RE", "0", 0.0, 50.0),
        pin("RB", "vcc", -20.0, 0.0),
        pin("RB", "b", -20.0, 25.0),
    ];

    // (a) upright: collector above emitter (world Y grows DOWNWARD).
    let mut up = passives.clone();
    up.extend([
        pin("Q1", "c", 0.0, 20.0),
        pin("Q1", "b", -10.0, 25.0),
        pin("Q1", "e", 0.0, 30.0),
    ]);
    let good = synthetic(els.clone(), up, &["vcc", "0"]);
    assert_eq!(
        facing_of(&good, "Q1"),
        Some(("c".to_string(), "e".to_string())),
        "collector must rank above emitter: it is one hop from the supply, \
         the emitter one hop from ground"
    );
    let (inv, res, detail) = facing_metrics(&good);
    assert_eq!(
        (inv, res),
        (0, 1),
        "upright stage must be clean: {detail:?}"
    );

    // (b) the same netlist, the transistor flipped.
    let mut down = passives;
    down.extend([
        pin("Q1", "c", 0.0, 30.0),
        pin("Q1", "b", -10.0, 25.0),
        pin("Q1", "e", 0.0, 20.0),
    ]);
    let bad = synthetic(els, down, &["vcc", "0"]);
    let (inv, res, detail) = facing_metrics(&bad);
    assert_eq!(
        (inv, res),
        (1, 1),
        "a flipped transistor must be counted, or the metric is vacuous: {detail:?}"
    );
}

/// **The decline set is the point.** Metric D must report *nothing*
/// rather than guess when the topology does not order the two conduction
/// terminals — a device floating between two nets that reach no rail
/// through any DC conductor, and a device wired symmetrically so both
/// terminals sit the same distance from both rails.
///
/// This is the assertion that keeps the rank falsifiable: without it, a
/// predicate that silently widened to "collector is whatever SPICE
/// listed first" would score identically on every fixture in the suite
/// (all eleven of whose devices resolve), and nothing would notice.
#[test]
fn facing_discriminator_declines_rather_than_guessing() {
    // (a) floating: `na` / `nb` touch no DC conductor but Q1 itself.
    let floating = synthetic(
        vec![
            two_pin(ElementKind::Resistor, "RG", "vcc", "g"),
            three_pin(ElementKind::Bjt, "Q1", "na", "g", "nb"),
        ],
        vec![
            pin("RG", "vcc", -20.0, 0.0),
            pin("RG", "g", -20.0, 25.0),
            pin("Q1", "na", 0.0, 30.0),
            pin("Q1", "g", -10.0, 25.0),
            pin("Q1", "nb", 0.0, 20.0),
        ],
        &["vcc", "0"],
    );
    assert_eq!(
        facing_of(&floating, "Q1"),
        None,
        "both conduction terminals are unreachable from any rail — decline"
    );
    assert_eq!(
        facing_metrics(&floating),
        (0, 0, Vec::new()),
        "a declined device must not reach the numerator OR the denominator"
    );

    // (b) a symmetric tie: each terminal is one resistor from the supply
    // and one from ground, so neither axis separates them.
    let tie = synthetic(
        vec![
            two_pin(ElementKind::Resistor, "RA", "vcc", "na"),
            two_pin(ElementKind::Resistor, "RB", "na", "0"),
            two_pin(ElementKind::Resistor, "RC", "vcc", "nb"),
            two_pin(ElementKind::Resistor, "RD", "nb", "0"),
            two_pin(ElementKind::Resistor, "RG", "vcc", "g"),
            three_pin(ElementKind::Bjt, "Q1", "na", "g", "nb"),
        ],
        vec![
            pin("RA", "vcc", -40.0, 0.0),
            pin("RA", "na", -40.0, 10.0),
            pin("RB", "na", -40.0, 20.0),
            pin("RB", "0", -40.0, 30.0),
            pin("RC", "vcc", 40.0, 0.0),
            pin("RC", "nb", 40.0, 10.0),
            pin("RD", "nb", 40.0, 20.0),
            pin("RD", "0", 40.0, 30.0),
            pin("RG", "vcc", -20.0, 0.0),
            pin("RG", "g", -20.0, 25.0),
            pin("Q1", "na", 0.0, 30.0),
            pin("Q1", "g", -10.0, 25.0),
            pin("Q1", "nb", 0.0, 20.0),
        ],
        &["vcc", "0"],
    );
    assert_eq!(
        facing_of(&tie, "Q1"),
        None,
        "a symmetric tie has no preferred way up — decline"
    );
}

// --- synthetic control arms: the metrics are not vacuously zero ----------

/// Build a fixture out of hand-placed pins, bypassing conversion.
///
/// The specimen tests above are `<=` rankings, which a metric that
/// always returns 0 would satisfy vacuously. These control arms are what
/// make the metrics falsifiable: hand-drawn geometry with a known answer,
/// independent of any placer. Same role as
/// `placement_stability.rs::p11_delta_grouping_catches_one_symbol_out_of_step`.
fn synthetic(elements: Vec<Elem>, pins: Vec<BodyPin>, rails: &[&str]) -> Fixture {
    let rail_nets: HashSet<String> = rails.iter().map(|s| (*s).to_string()).collect();
    // Page polarity by canonical name — a synthetic arm names its rails
    // `vcc` / `0`, so no `;@ power=` string is involved.
    let down_rails = rail_nets
        .iter()
        .filter(|n| is_canonical_ground_name(n))
        .cloned()
        .collect();
    Fixture {
        name: "synthetic".to_string(),
        pins,
        elements,
        rail_nets,
        down_rails,
        labels: Vec::new(),
    }
}

fn two_pin(kind: ElementKind, refdes: &str, a: &str, b: &str) -> Elem {
    Elem {
        refdes: refdes.to_string(),
        kind,
        nodes: vec![a.to_string(), b.to_string()],
        is_power_source: false,
        is_sheet: false,
    }
}

/// A three-terminal device, in SPICE order (`c b e` / `d g s`).
fn three_pin(kind: ElementKind, refdes: &str, a: &str, b: &str, c: &str) -> Elem {
    Elem {
        refdes: refdes.to_string(),
        kind,
        nodes: vec![a.to_string(), b.to_string(), c.to_string()],
        is_power_source: false,
        is_sheet: false,
    }
}

fn pin(refdes: &str, net: &str, x: f64, y: f64) -> BodyPin {
    BodyPin {
        refdes: refdes.to_string(),
        net: net.to_string(),
        x_mm: x,
        y_mm: y,
    }
}

/// A four-element chain `n0 -> n1 -> n2 -> n3 -> n4`, drawn the textbook
/// way (all horizontal, all left-to-right) and then three ways it can go
/// wrong. The counts are the metric's definition, checked against a
/// drawing whose right answer is obvious by eye.
#[test]
fn chain_metric_counts_a_known_synthetic_ladder() {
    let els = vec![
        two_pin(ElementKind::Resistor, "E1", "n0", "n1"),
        two_pin(ElementKind::Inductor, "E2", "n1", "n2"),
        two_pin(ElementKind::Inductor, "E3", "n2", "n3"),
        two_pin(ElementKind::Inductor, "E4", "n3", "n4"),
    ];

    // (a) textbook: four horizontal members, all running +X.
    let good = synthetic(
        els.clone(),
        vec![
            pin("E1", "n0", 0.0, 0.0),
            pin("E1", "n1", 10.0, 0.0),
            pin("E2", "n1", 20.0, 0.0),
            pin("E2", "n2", 30.0, 0.0),
            pin("E3", "n2", 40.0, 0.0),
            pin("E3", "n3", 50.0, 0.0),
            pin("E4", "n3", 60.0, 0.0),
            pin("E4", "n4", 70.0, 0.0),
        ],
        &[],
    );
    let (axis, rev, members, detail) = chain_metrics(&good);
    assert_eq!(
        (axis, rev, members),
        (0, 0, 4),
        "the textbook ladder must score zero: {detail:?}"
    );

    // (b) one member rotated 90 degrees: one off-axis, no reversal.
    let mut pins = good.pins.clone();
    for p in &mut pins {
        if p.refdes == "E3" {
            p.x_mm = 45.0;
            p.y_mm = if p.net == "n2" { 0.0 } else { 10.0 };
        }
    }
    let tilted = synthetic(els.clone(), pins, &[]);
    let (axis, rev, _, detail) = chain_metrics(&tilted);
    assert_eq!(
        (axis, rev),
        (1, 0),
        "one rotated member is ONE axis violation and no reversal: {detail:?}"
    );

    // (c) one member drawn backwards: on-axis, reversed.
    let mut pins = good.pins.clone();
    for p in &mut pins {
        if p.refdes == "E3" {
            p.x_mm = if p.net == "n2" { 50.0 } else { 40.0 };
        }
    }
    let flipped = synthetic(els.clone(), pins, &[]);
    let (axis, rev, _, detail) = chain_metrics(&flipped);
    assert_eq!(
        (axis, rev),
        (0, 1),
        "one backwards member is ONE reversal and no axis violation — the two counts \
         are disjoint by construction: {detail:?}"
    );

    // (d) the `lc_ladder_lpf` shape: four members, four orientations —
    // two horizontal (one backwards) and two vertical (one backwards).
    // Both readings score (2 off-axis, 1 reversed), so the HORIZONTAL
    // tie-break decides which two are named, and the totals are (2, 1).
    let mixed = synthetic(
        els,
        vec![
            pin("E1", "n0", 0.0, 10.0),
            pin("E1", "n1", 0.0, 0.0),
            pin("E2", "n1", 20.0, 0.0),
            pin("E2", "n2", 30.0, 0.0),
            pin("E3", "n2", 40.0, 0.0),
            pin("E3", "n3", 40.0, 10.0),
            pin("E4", "n3", 70.0, 0.0),
            pin("E4", "n4", 60.0, 0.0),
        ],
        &[],
    );
    let (axis, rev, _, detail) = chain_metrics(&mixed);
    assert_eq!(
        (axis, rev),
        (2, 1),
        "the four-orientation ladder must read as two off-axis members plus one \
         reversal: {detail:?}"
    );
    let chosen = chains(&mixed);
    let (off, _) = score_chain(&mixed, &chosen[0]);
    assert_eq!(
        off,
        vec!["E1".to_string(), "E3".to_string()],
        "on a tie the metric grades against the HORIZONTAL reading, so the two \
         VERTICAL members are the ones named off-axis"
    );
}

/// `port_shapes` drawn as ONE connected folded path — arm 2 of metric
/// C's acceptance ranking, transcribed pin-for-pin from
/// `--placer=divider-rails`' emitted `.kicad_sch`. The acceptance test
/// converts that placer for real as well and checks the live result
/// against this transcription, so a drift here cannot go unnoticed.
///
/// `R1` runs down, `R2` turns right (and is drawn backwards, which is
/// why the arm scores `chain.axis = 1, chain.reversal = 1`), `R3`
/// continues right along the row above, `R4` drops to ground. Four
/// devices, one path, nothing stranded.
fn port_shapes_folded() -> Fixture {
    synthetic(
        vec![
            two_pin(ElementKind::Resistor, "R1", "src", "ni"),
            two_pin(ElementKind::Resistor, "R2", "ni", "no"),
            two_pin(ElementKind::Resistor, "R3", "no", "nb"),
            two_pin(ElementKind::Resistor, "R4", "nb", "0"),
        ],
        vec![
            pin("R1", "src", 35.56, 35.56),
            pin("R1", "ni", 35.56, 43.18),
            pin("R2", "no", 46.99, 39.37),
            pin("R2", "ni", 54.61, 39.37),
            pin("R3", "no", 54.61, 35.56),
            pin("R3", "nb", 62.23, 35.56),
            pin("R4", "nb", 62.23, 40.64),
            pin("R4", "0", 62.23, 48.26),
        ],
        &["0"],
    )
}

/// `port_shapes` as the shipping placer drew it when metric C was
/// written — arm 3 of the acceptance ranking, transcribed pin-for-pin.
///
/// Two vertical stacks of two: `R1`/`R2` in a column at x = 35.56 and
/// `R3`/`R4` in a column at x = 67.31, joined by a single 41.91 mm run
/// that goes 31.75 mm across and 10.16 mm back up. Every member is
/// vertical and every member travels down the page, which is exactly why
/// `chain.axis` and `chain.reversal` both read 0.
fn port_shapes_shattered() -> Fixture {
    synthetic(
        vec![
            two_pin(ElementKind::Resistor, "R1", "src", "ni"),
            two_pin(ElementKind::Resistor, "R2", "ni", "no"),
            two_pin(ElementKind::Resistor, "R3", "no", "nb"),
            two_pin(ElementKind::Resistor, "R4", "nb", "0"),
        ],
        vec![
            pin("R1", "src", 35.56, 31.75),
            pin("R1", "ni", 35.56, 39.37),
            pin("R2", "ni", 35.56, 40.64),
            pin("R2", "no", 35.56, 48.26),
            pin("R3", "no", 67.31, 38.10),
            pin("R3", "nb", 67.31, 45.72),
            pin("R4", "nb", 67.31, 46.99),
            pin("R4", "0", 67.31, 54.61),
        ],
        &["0"],
    )
}

/// Metric C's non-vacuity control arm: the same four-element chain drawn
/// four ways, with the right answer obvious by eye.
///
/// Also pins down the **rail-terminated terminus** decision. `R4`
/// terminates on ground, so `chains()` excludes it — correctly, because
/// metric A's axis and direction counts are *pose* questions and a
/// stub's pose belongs to its rail glyph (V14). Compactness makes no
/// demand on pose, and a terminating stub carries the chain's own
/// current, so it IS measured here: `chain.run_members` is 4 where
/// `chain.members` is 3.
#[test]
fn chain_stranded_counts_a_known_synthetic_split() {
    let els = vec![
        two_pin(ElementKind::Resistor, "R1", "src", "ni"),
        two_pin(ElementKind::Resistor, "R2", "ni", "no"),
        two_pin(ElementKind::Resistor, "R3", "no", "nb"),
        two_pin(ElementKind::Resistor, "R4", "nb", "0"),
    ];
    // Every member is 7.62 long, so the chain body is 30.48 and a link
    // is a break only above that.
    let column = |xs: [f64; 4], ys: [f64; 4]| -> Fixture {
        let mut pins = Vec::new();
        for (i, (r, nets)) in [
            ("R1", ["src", "ni"]),
            ("R2", ["ni", "no"]),
            ("R3", ["no", "nb"]),
            ("R4", ["nb", "0"]),
        ]
        .iter()
        .enumerate()
        {
            pins.push(pin(r, nets[0], xs[i], ys[i]));
            pins.push(pin(r, nets[1], xs[i], ys[i] + 7.62));
        }
        synthetic(els.clone(), pins, &["0"])
    };

    // (a) one tight column: four members, three 1.27 mm links.
    let good = column([0.0; 4], [0.0, 8.89, 17.78, 26.67]);
    let (stranded, total, detail) = compactness_metrics(&good);
    assert_eq!(
        (stranded, total),
        (0, 4),
        "a single tight column strands nobody, and the rail-terminated R4 IS \
         measured — 4 members, not 3: {detail:?}"
    );
    assert_eq!(
        chain_metrics(&good).2,
        3,
        "`chain.members` still excludes the rail stub — `chains()` is untouched"
    );

    // (b) a long stride is not a break. Each link is 22.86 mm — three
    // times any single member, and the exact stride the textbook
    // `lc_ladder_lpf` drawing uses — yet the chain reads as one run.
    // This is the case that kills a per-member distance threshold.
    let strided = column([0.0; 4], [0.0, 30.48, 60.96, 91.44]);
    let (stranded, _, detail) = compactness_metrics(&strided);
    assert_eq!(
        stranded, 0,
        "a chain strided three member-lengths apart, evenly, is not shattered — a          per-member threshold would call every link here a break: {detail:?}"
    );

    // (c) the `port_shapes` shape: two tight pairs, one long jump.
    let split = column([0.0, 0.0, 31.75, 31.75], [0.0, 8.89, 6.35, 15.24]);
    let (stranded, total, detail) = compactness_metrics(&split);
    assert_eq!(
        (stranded, total),
        (2, 4),
        "two stacks of two, a chain-body-plus jump apart, strand the smaller half — \
         and with equal halves the count is 2 either way, so no tie-break enters \
         the number: {detail:?}"
    );

    // (d) one member flung off the end strands exactly one.
    let one_off = column([0.0, 0.0, 0.0, 63.5], [0.0, 8.89, 17.78, 17.78]);
    let (stranded, _, detail) = compactness_metrics(&one_off);
    assert_eq!(
        stranded, 1,
        "a three-member run plus one outlier strands the outlier only: {detail:?}"
    );
}

/// The terminus rule is a *terminus* rule, not "adopt any stub".
///
/// A chain endpoint whose free net fans out to more than one other drawn
/// element is not the end of a run, and adopting one of them would make
/// the measured population depend on which. `lc_ladder_lpf` carries both
/// cases: `RS`'s free net `src` reaches only the drawn source `VIN`
/// (adopted — five members measured against four chain members), while
/// `L3`'s free net `out` carries `C4` and `RL` as well (not adopted).
#[test]
fn chain_terminus_is_adopted_only_at_a_genuine_end_of_run() {
    let f = load_arm("lc_ladder_lpf", &["--placer", "flow-seed-v4"]);
    let cs = chains(&f);
    assert_eq!(cs.len(), 1, "lc_ladder_lpf presents one chain");
    let (members, links) = chain_run(&f, &cs[0]);
    assert_eq!(
        members,
        vec!["VIN", "RS", "L1", "L2", "L3"],
        "the drawn source terminates the chain at `src` and is adopted; `out` fans \
         out to C4 and RL, so nothing is adopted at the far end"
    );
    assert_eq!(links.len(), members.len() - 1, "a path has n-1 links");
}

/// A rail-to-rail divider drawn stacked, then drawn side by side. Also
/// pins down the two exclusions the definition depends on: a capacitor
/// carries no DC and never forms a pair, and a resistor whose far end
/// reaches no second rail is not on a supply path.
#[test]
fn stacking_metric_counts_a_known_synthetic_divider() {
    let els = vec![
        two_pin(ElementKind::Resistor, "R1", "VCC", "mid"),
        two_pin(ElementKind::Resistor, "R2", "mid", "0"),
    ];
    let stacked = synthetic(
        els.clone(),
        vec![
            pin("R1", "VCC", 0.0, 0.0),
            pin("R1", "mid", 0.0, 10.0),
            pin("R2", "mid", 0.0, 20.0),
            pin("R2", "0", 0.0, 30.0),
        ],
        &["VCC", "0"],
    );
    let (sbs, pairs, detail) = stacking_metrics(&stacked);
    assert_eq!(
        (sbs, pairs),
        (0, 1),
        "a stacked divider is one pair and no violation: {detail:?}"
    );

    let spread = synthetic(
        els,
        vec![
            pin("R1", "VCC", 0.0, 0.0),
            pin("R1", "mid", 10.0, 0.0),
            pin("R2", "mid", 20.0, 0.0),
            pin("R2", "0", 30.0, 0.0),
        ],
        &["VCC", "0"],
    );
    let (sbs, pairs, detail) = stacking_metrics(&spread);
    assert_eq!(
        (sbs, pairs),
        (1, 1),
        "the same divider drawn sideways is ONE violation: {detail:?}"
    );

    // A capacitor carries no DC: R + C is not a current path, so an RC
    // low-pass is never asked to stack.
    let rc = synthetic(
        vec![
            two_pin(ElementKind::Resistor, "R1", "in", "out"),
            two_pin(ElementKind::Capacitor, "C1", "out", "0"),
        ],
        vec![
            pin("R1", "in", 0.0, 0.0),
            pin("R1", "out", 10.0, 0.0),
            pin("C1", "out", 20.0, 0.0),
            pin("C1", "0", 30.0, 0.0),
        ],
        &["0"],
    );
    let (sbs, pairs, _) = stacking_metrics(&rc);
    assert_eq!(
        (sbs, pairs),
        (0, 0),
        "an RC low-pass has no DC current path and must form no stacking pair"
    );

    // Two resistors in series on a SIGNAL path (no second rail at the
    // far end) are a chain, not a stack — they belong to metric A.
    let ladder = synthetic(
        vec![
            two_pin(ElementKind::Resistor, "R1", "in", "n1"),
            two_pin(ElementKind::Resistor, "R2", "n1", "out"),
        ],
        vec![
            pin("R1", "in", 0.0, 0.0),
            pin("R1", "n1", 10.0, 0.0),
            pin("R2", "n1", 20.0, 0.0),
            pin("R2", "out", 30.0, 0.0),
        ],
        &["0"],
    );
    let (sbs, pairs, _) = stacking_metrics(&ladder);
    assert_eq!(
        (sbs, pairs),
        (0, 0),
        "a signal-path resistor ladder reaches no second rail and must NOT be asked \
         to stack — that is metric A's business, and demanding both would be a \
         contradiction"
    );
}

// --- metric D: acceptance ranking, control arm, discriminator ------------

/// The `*@port` declarations the SOURCE makes, resolved live from the
/// `.cir`.
///
/// The transcribed arms below freeze only each terminal's *rotation* —
/// the one quantity the two placers actually differ in. The direction
/// half of every transcribed label is re-derived from the fixture on
/// every run, so a change to a fixture's `*@port` lines cannot leave an
/// arm silently stale.
fn declared_ports(name: &str) -> HashMap<String, PortDir> {
    let spice_src =
        std::fs::read_to_string(fixtures_dir().join(format!("{name}.cir"))).expect("read cir");
    let parsed = spice_parser::parse(&spice_src, FileId(0)).expect("parse spice");
    let library = load_test_library();
    let resolved = spice_resolve::resolve(&parsed.netlist, &library).expect("resolve spice");
    resolved
        .ports
        .iter()
        .map(|p| (p.net.clone(), p.dir))
        .collect()
}

/// A labels-only fixture: metric D reads nothing else.
fn labels_only(name: &str, rows: &[(&str, u16)]) -> Fixture {
    let declared = declared_ports(name);
    Fixture {
        name: name.to_string(),
        pins: Vec::new(),
        elements: Vec::new(),
        rail_nets: HashSet::new(),
        down_rails: HashSet::new(),
        labels: rows
            .iter()
            .map(|&(net, rot)| {
                let dir = declared.get(net).copied();
                PortLabel {
                    net: net.to_string(),
                    shape: match dir {
                        Some(PortDir::Output) => "output",
                        Some(PortDir::Bidir) => "bidirectional",
                        // `label_specs` stamps `input` on every
                        // undeclared one-pin interface net.
                        Some(PortDir::Input) | None => "input",
                    }
                    .to_string(),
                    rot,
                    declared: dir,
                }
            })
            .collect(),
    }
}

/// Sum metric D over a whole transcribed arm: `(vertical, backwards)`.
fn score_arm(arm: &[(&str, &[(&str, u16)])]) -> (usize, usize) {
    let mut v = 0;
    let mut b = 0;
    for &(name, rows) in arm {
        let (vert, back, _, _, _) = port_label_metrics(&labels_only(name, rows));
        v += vert;
        b += back;
    }
    (v, b)
}

/// `--placer=terminal-series`, transcribed from its emitted
/// `.kicad_sch` files (branch `feat/f1-terminal-series`, `ef3a6d9`).
///
/// It repairs six of the nine vertical terminals the shipping default
/// draws and **breaks a seventh**: `common_emitter`'s `in` goes 180 → 90.
const ARM_TERMINAL_SERIES: &[(&str, &[(&str, u16)])] = &[
    ("cascode_amp", &[("in", 180), ("out", 0)]),
    ("common_emitter", &[("in", 90), ("out", 0)]),
    ("diff_pair", &[("in1", 180), ("in2", 0)]),
    ("lc_ladder_lpf", &[("out", 0)]),
    ("named_rails", &[("in", 180)]),
    ("opamp_definition_level", &[("in1", 180), ("in2", 180)]),
    ("opamp_inverting", &[("in", 180)]),
    ("opamp_inverting_real", &[("in", 180)]),
    (
        "port_shapes",
        &[("nb", 180), ("ni", 180), ("no", 90), ("src", 90)],
    ),
    ("rc_lowpass", &[("in", 180)]),
    ("rc_lowpass_ports", &[("in", 180), ("out", 0)]),
    ("rc_phase_shift", &[("in", 180), ("out", 0)]),
    ("sallen_key_driven", &[("out", 0)]),
    ("sallen_key_lpf", &[("in", 180), ("out", 0)]),
    ("shunt_feedback_amp", &[("in", 180), ("out", 0)]),
    ("two_stage_amp", &[("in", 90), ("out", 0)]),
    ("wien_bridge_osc", &[("out", 0)]),
];

/// `--placer=terminal-series-divider`, transcribed the same way. It
/// repairs every one of the seven reachable verticals and breaks none;
/// the two it leaves are `port_shapes`' `no` / `src`, which neither arm
/// reaches.
const ARM_TERMINAL_SERIES_DIVIDER: &[(&str, &[(&str, u16)])] = &[
    ("cascode_amp", &[("in", 180), ("out", 0)]),
    ("common_emitter", &[("in", 180), ("out", 0)]),
    ("diff_pair", &[("in1", 180), ("in2", 0)]),
    ("lc_ladder_lpf", &[("out", 0)]),
    ("named_rails", &[("in", 180)]),
    ("opamp_definition_level", &[("in1", 180), ("in2", 180)]),
    ("opamp_inverting", &[("in", 180)]),
    ("opamp_inverting_real", &[("in", 180)]),
    (
        "port_shapes",
        &[("nb", 180), ("ni", 180), ("no", 90), ("src", 90)],
    ),
    ("rc_lowpass", &[("in", 180)]),
    ("rc_lowpass_ports", &[("in", 180), ("out", 0)]),
    ("rc_phase_shift", &[("in", 180), ("out", 0)]),
    ("sallen_key_driven", &[("out", 0)]),
    ("sallen_key_lpf", &[("in", 180), ("out", 0)]),
    ("shunt_feedback_amp", &[("in", 180), ("out", 0)]),
    ("two_stage_amp", &[("in", 180), ("out", 0)]),
    ("wien_bridge_osc", &[("out", 0)]),
];

/// **The acceptance test for metric D.**
///
/// Two challenger arms exist for the port-terminal defect, and the
/// registered metrics cannot tell them apart: `chain.*` and `stack.*` are
/// byte-identical across both on all 18 fixtures, so the ADR-23 aggregate
/// judged them on wirelength-ish Tier-2 residue and made
/// `terminal-series` — the arm that leaves one terminal vertical that the
/// other repairs, and breaks a second one that was already correct —
/// PROMOTABLE, while ranking `terminal-series-divider` below it. The
/// owner reads the second arm as strictly better. A metric that does not
/// reproduce that order is measuring the wrong thing.
///
/// Asserted **strictly**, and for ADR-28 metric C's reason: a tie here is
/// exactly the failure mode the metric exists to close. Strictness is
/// safe because both arms are **transcribed** rather than converted —
/// neither placer is on `master`, and freezing them means this assertion
/// is a fact about the metric's arithmetic, not a gate on any placer.
/// The shipping default is converted live and only *printed*, never
/// asserted, so repairing it can never fail this test.
#[test]
fn port_label_direction_ranks_the_divider_arm_above_terminal_series() {
    let (series_v, series_b) = score_arm(ARM_TERMINAL_SERIES);
    let (divider_v, divider_b) = score_arm(ARM_TERMINAL_SERIES_DIVIDER);

    let mut live_v = 0;
    let mut live_b = 0;
    for name in FIXTURES {
        let (v, b, _, _, _) = port_label_metrics(&load(name));
        live_v += v;
        live_b += b;
    }
    println!(
        "port.label_vertical / backwards, summed over the suite:\n  \
         shipping default        {live_v} / {live_b}   (printed, never asserted)\n  \
         terminal-series         {series_v} / {series_b}\n  \
         terminal-series-divider {divider_v} / {divider_b}"
    );

    assert!(
        (divider_v, divider_b) < (series_v, series_b),
        "metric D must rank `terminal-series-divider` strictly above \
         `terminal-series`: divider ({divider_v}, {divider_b}) vs series \
         ({series_v}, {series_b})"
    );
    // Non-vacuity of the ranking itself: the arm the owner rejects must
    // actually carry violations, or the comparison above is 0 < 0.
    assert!(
        series_v > 0,
        "the rejected arm must score at least one vertical terminal"
    );
}

/// Metric D's non-vacuity control arm: hand-written terminals whose right
/// answer is obvious, so the counts cannot silently degenerate to
/// "always 0" and the two counts are shown to be disjoint.
#[test]
fn port_label_metric_counts_a_known_synthetic_terminal_set() {
    // `port_shapes` declares ni=input, no=output, nb=bidir; `src` is an
    // undeclared one-pin interface net.
    let clean = labels_only(
        "port_shapes",
        &[("ni", 180), ("no", 0), ("nb", 180), ("src", 180)],
    );
    assert_eq!(
        {
            let (v, b, n, d, _) = port_label_metrics(&clean);
            (v, b, n, d)
        },
        (0, 0, 4, 2),
        "input at 180 and output at 0 both travel rightward; bidir asserts \
         no direction; the undeclared net is not direction-graded"
    );

    // Every terminal turned a quarter turn: all four vertical, none
    // charged a direction defect on top (the counts are disjoint).
    let vertical = labels_only(
        "port_shapes",
        &[("ni", 270), ("no", 90), ("nb", 90), ("src", 270)],
    );
    assert_eq!(
        {
            let (v, b, ..) = port_label_metrics(&vertical);
            (v, b)
        },
        (4, 0),
        "90 and 270 are the same ANGLE_VERTICAL glyph rotation, and a \
         vertical terminal is blamed once"
    );

    // Horizontal but pointing upstream: the input's arrow now travels
    // leftward, the output's likewise. `nb` and `src` stay unblamed.
    let backwards = labels_only(
        "port_shapes",
        &[("ni", 0), ("no", 180), ("nb", 0), ("src", 0)],
    );
    assert_eq!(
        {
            let (v, b, ..) = port_label_metrics(&backwards);
            (v, b)
        },
        (0, 2),
        "an input at 0 and an output at 180 both draw an arrow travelling \
         leftward, against the sheet's left-to-right flow"
    );
}

/// **The discriminator**, and the premise the `backwards` definition
/// rests on: the emitted `(shape …)` token is a *default* wherever the
/// source declares nothing.
///
/// `common_emitter`'s `out` is the schematic's output — and the emitter
/// stamps it `(shape input)`, because `label_specs` gives every
/// undeclared one-pin interface net that token. Grading direction off
/// the token would therefore call the arms' repair of that very terminal
/// (270 → 0) a NEW backwards violation. So `backwards` reads the
/// netlist's `*@port`, and this test fails the day the emitter learns to
/// infer a direction — at which point the restriction should be revisited
/// rather than kept out of habit.
///
/// Both halves are properties of `label_specs`, not of placement, so
/// neither moves when a placer changes.
#[test]
fn port_direction_is_graded_only_where_the_source_declares_it() {
    let ce = load("common_emitter");
    let out = ce
        .labels
        .iter()
        .find(|l| l.net == "out")
        .expect("common_emitter draws an `out` terminal");
    assert_eq!(
        out.shape, "input",
        "the emitter defaults an undeclared interface terminal to `input` \
         even when the net is the circuit's output"
    );
    assert!(
        out.declared.is_none(),
        "`common_emitter` declares no `*@port`, so `out` carries no direction"
    );

    // A declared terminal, by contrast, is graded — and the fixture that
    // proves it is the one the suite currently gets wrong.
    let sk = load("sallen_key_lpf");
    let sk_out = sk
        .labels
        .iter()
        .find(|l| l.net == "out")
        .expect("sallen_key_lpf draws an `out` terminal");
    assert_eq!(
        sk_out.declared,
        Some(PortDir::Output),
        "`sallen_key_lpf` declares `*@port out=output`"
    );
    assert_eq!(sk_out.shape, "output");

    // Same rotation, opposite verdict, purely because one is declared.
    let graded = labels_only("sallen_key_lpf", &[("out", 180)]);
    let ungraded = labels_only("common_emitter", &[("out", 180)]);
    assert_eq!(port_label_metrics(&graded).1, 1);
    assert_eq!(port_label_metrics(&ungraded).1, 0);
}
