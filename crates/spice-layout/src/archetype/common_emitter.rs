//! Common-emitter BJT amplifier archetype.
//!
//! Pattern (NPN; PNP is a future extension):
//! ```text
//!   col:    -2     -1      0      1      2
//!   top:                 (Vcc)   RC
//!   mid:    CIN    R1            COUT
//!           ↓      R2     Q1
//!   bot:    (gnd)  RE     CE
//! ```
//! Q1 sits at the cluster origin. Vcc rail runs along the top of the
//! bounding box (RC hangs from it); GND rail along the bottom (RE/CE
//! drop into it). Signal flow left-to-right: CIN → Q1.base, then
//! Q1.collector → COUT.
//!
//! The matcher is intentionally pin-role-driven, not refdes-name-driven
//! — the user can name parts however they like as long as the
//! connectivity is correct.

use std::collections::HashMap;

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole, ResolvedElement};

use crate::{CELL_H, CELL_W, GridPoint};

use super::Archetype;

pub(super) struct CommonEmitter;

impl Archetype for CommonEmitter {
    fn match_and_seed(&self, checked: &CheckedNetlist) -> Option<HashMap<String, GridPoint>> {
        let elems = &checked.elements;

        // 1. Identify a single NPN BJT. (Multi-BJT circuits are out
        //    of scope for this archetype — diff pairs / cascodes get
        //    their own templates.)
        let bjts: Vec<&ResolvedElement> = elems
            .iter()
            .filter(|e| e.kind == ElementKind::Bjt)
            .collect();
        let q = match bjts.as_slice() {
            [single] => *single,
            _ => return None,
        };
        if q.nodes.len() < 3 {
            return None;
        }
        let collector_net = q.nodes[0].as_str();
        let base_net = q.nodes[1].as_str();
        let emitter_net = q.nodes[2].as_str();
        // Reject degenerate nets — collapsing onto ground or into one
        // another means this isn't a CE amp.
        if [collector_net, base_net, emitter_net].contains(&"0") {
            return None;
        }
        if collector_net == base_net || base_net == emitter_net || collector_net == emitter_net {
            return None;
        }

        // 2. Find the power net: the non-ground node of any element
        //    whose role is Power(_).
        let mut power_nets: Vec<&str> = elems
            .iter()
            .filter(|e| matches!(e.role, ElementRole::Power(_)))
            .flat_map(|e| e.nodes.iter().map(String::as_str))
            .filter(|n| *n != "0")
            .collect();
        power_nets.sort_unstable();
        power_nets.dedup();
        if power_nets.is_empty() {
            return None;
        }

        // Helper: is `net` a power rail? (Either marked via *@power
        // or one of the conventional names.)
        let is_power = |net: &str| {
            power_nets.contains(&net)
                || matches!(
                    net.to_ascii_lowercase().as_str(),
                    "vcc" | "vdd" | "v+" | "vplus"
                )
        };
        let is_gnd = |net: &str| net == "0";

        // 3. RC: a 2-terminal element on `collector_net` whose other
        //    terminal sits on a power rail.
        let rc = find_two_terminal(elems, collector_net, &is_power, ElementKind::Resistor)?;

        // 4. RE: a 2-terminal resistor on `emitter_net` whose other
        //    terminal is ground.
        let re = find_two_terminal(elems, emitter_net, &is_gnd, ElementKind::Resistor)?;

        // 5. CE: optional bypass cap on `emitter_net` to ground.
        let ce = find_two_terminal(elems, emitter_net, &is_gnd, ElementKind::Capacitor);

        // 6. Base bias divider:
        //    R1: base_net <-> power; R2: base_net <-> ground.
        let r1 = find_two_terminal(elems, base_net, &is_power, ElementKind::Resistor)?;
        let r2 = find_two_terminal(elems, base_net, &is_gnd, ElementKind::Resistor);

        // 7. CIN: cap with one terminal on base_net, other terminal
        //    *not* power, ground, or the same base_net.
        let cin =
            find_two_terminal_not(elems, base_net, &is_power, &is_gnd, ElementKind::Capacitor)?;

        // 8. COUT: cap with one terminal on collector_net, other
        //    terminal not power/ground/collector.
        let cout = find_two_terminal_not(
            elems,
            collector_net,
            &is_power,
            &is_gnd,
            ElementKind::Capacitor,
        );

        // We need at least Q + RC + RE + R1 + CIN.
        // Build the seed map using the column/row template.
        let mut seeds: HashMap<String, GridPoint> = HashMap::new();
        // Origin: pick a free anchor far from align clusters at (0,0).
        // Stage-1's auto-fill row sits at `max_y + 2*(CELL_H+1)` for
        // unconstrained elements — we place the archetype at the same
        // (0,0) anchor; the placer's other phases shift around us if
        // they need more room.
        let q_x = 0;
        let q_y = 0;
        let dx = CELL_W + 1;
        let dy = CELL_H + 1;

        // Q1 at origin.
        seeds.insert(q.refdes.clone(), GridPoint::new(q_x, q_y));
        // RC above Q1, slightly to the right (between Q.col and Vcc rail).
        seeds.insert(rc.refdes.clone(), GridPoint::new(q_x + dx, q_y - 2 * dy));
        // RE below Q1, on Q's column-1 (left).
        seeds.insert(re.refdes.clone(), GridPoint::new(q_x - dx, q_y + 2 * dy));
        if let Some(ce) = ce {
            // CE below Q1, on Q's column (centred under emitter).
            seeds.insert(ce.refdes.clone(), GridPoint::new(q_x, q_y + 2 * dy));
        }
        // R1 above-left of Q (between Vcc and base).
        seeds.insert(r1.refdes.clone(), GridPoint::new(q_x - dx, q_y - dy));
        if let Some(r2) = r2 {
            // R2 below-left of Q (between base and gnd).
            seeds.insert(r2.refdes.clone(), GridPoint::new(q_x - dx, q_y + dy));
        }
        // CIN: input side, two columns to the left.
        seeds.insert(cin.refdes.clone(), GridPoint::new(q_x - 2 * dx, q_y));
        if let Some(cout) = cout {
            // COUT: output side, two columns to the right.
            seeds.insert(cout.refdes.clone(), GridPoint::new(q_x + 2 * dx, q_y));
        }

        Some(seeds)
    }
}

/// Find the *first* element (skipping ones already returned by an
/// earlier search isn't necessary here — each role is distinct enough
/// in the CE archetype that we accept the first hit) of `kind` whose
/// nodes are exactly `{near_net, X}` where `X` satisfies `far_pred`.
fn find_two_terminal<'a>(
    elems: &'a [ResolvedElement],
    near_net: &str,
    far_pred: &dyn Fn(&str) -> bool,
    kind: ElementKind,
) -> Option<&'a ResolvedElement> {
    for e in elems {
        if e.kind != kind || e.nodes.len() != 2 {
            continue;
        }
        let (a, b) = (e.nodes[0].as_str(), e.nodes[1].as_str());
        if a == near_net && far_pred(b) {
            return Some(e);
        }
        if b == near_net && far_pred(a) {
            return Some(e);
        }
    }
    None
}

/// Like [`find_two_terminal`] but the *far* terminal must satisfy
/// **neither** of the two predicates and must differ from `near_net`.
/// Used for AC-coupling caps whose far end is a signal net.
fn find_two_terminal_not<'a>(
    elems: &'a [ResolvedElement],
    near_net: &str,
    not_pred_a: &dyn Fn(&str) -> bool,
    not_pred_b: &dyn Fn(&str) -> bool,
    kind: ElementKind,
) -> Option<&'a ResolvedElement> {
    for e in elems {
        if e.kind != kind || e.nodes.len() != 2 {
            continue;
        }
        let (a, b) = (e.nodes[0].as_str(), e.nodes[1].as_str());
        let check = |near: &str, far: &str| {
            near == near_net && far != near_net && !not_pred_a(far) && !not_pred_b(far)
        };
        if check(a, b) {
            return Some(e);
        }
        if check(b, a) {
            return Some(e);
        }
    }
    None
}
