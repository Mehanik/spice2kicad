//! Stage 2 — per-net rectilinear Steiner trees.
//!
//! Stage 2a covers the small-N closed-form cases:
//!
//! * **N = 2** — single segment if pins share an axis, otherwise an
//!   L-shape via the corner `(b.x, a.y)`. The bend is not a junction
//!   (only two endpoints touch the net at this point).
//! * **N = 3** — Hwang's exact rectilinear Steiner minimum tree. The
//!   single Steiner point is the coordinate-wise median of the three
//!   pins; this is provably optimal.
//!
//! Higher-N nets fall through to a stub that lands in Task 4
//! (Hanan-grid DP) and Task 5 (FLUTE for N ≥ 10).

use crate::types::{PinRef, Segment};
use lexpr::Value as Sexpr;

const EPS: f64 = 1e-6;

/// Route a 2-pin net: single segment when collinear on either axis,
/// otherwise an L-shape via `(b.x, a.y)` (horizontal-then-vertical).
///
/// Inputs are expected to already be on the KiCad schematic grid
/// (1.27 mm) — that is a placer-side invariant, not a routing
/// responsibility. The router preserves the coordinates verbatim.
#[must_use]
pub fn route_two_pin(a: (f64, f64), b: (f64, f64)) -> Vec<Segment> {
    let (x1, y1) = (a.0, a.1);
    let (x2, y2) = (b.0, b.1);
    if (x1 - x2).abs() < EPS && (y1 - y2).abs() < EPS {
        // Coincident pins: nothing to route.
        return Vec::new();
    }
    if (y1 - y2).abs() < EPS || (x1 - x2).abs() < EPS {
        return vec![Segment { x1, y1, x2, y2 }];
    }
    vec![
        Segment { x1, y1, x2, y2: y1 },
        Segment { x1: x2, y1, x2, y2 },
    ]
}

/// Route a 3-pin net via Hwang's exact RSMT. The Steiner point is
/// `(median(xs), median(ys))`; each pin connects to it through up to
/// two axis-parallel segments. Degenerate cases (Steiner point
/// coincides with a pin, or two pins share a coordinate with the
/// Steiner point) collapse naturally — no zero-length segments are
/// emitted.
#[must_use]
pub fn route_three_pin(pins: [(f64, f64); 3]) -> Vec<Segment> {
    let xs = [pins[0].0, pins[1].0, pins[2].0];
    let ys = [pins[0].1, pins[1].1, pins[2].1];

    let mut sx = xs;
    sx.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut sy = ys;
    sy.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (mx, my) = (sx[1], sy[1]);

    let mut segs: Vec<Segment> = Vec::new();
    for i in 0..3 {
        let dx = (xs[i] - mx).abs() > EPS;
        let dy = (ys[i] - my).abs() > EPS;
        if dx {
            segs.push(Segment {
                x1: xs[i],
                y1: ys[i],
                x2: mx,
                y2: ys[i],
            });
        }
        if dy {
            segs.push(Segment {
                x1: mx,
                y1: ys[i],
                x2: mx,
                y2: my,
            });
        }
    }

    // Coalesce collinear horizontal segments through the Steiner X
    // band: when two pins share Y with the Steiner point and sit on
    // opposite sides of `mx`, the per-pin horizontal segments meet
    // at `mx` and can be merged into a single span. Same for the
    // vertical band. Without this the wire count and total length
    // are still correct, but adjacent segments duplicate the bend
    // at the Steiner point.
    coalesce_at(&mut segs, mx, my);
    segs
}

/// Merge two collinear segments that meet exactly at the Steiner
/// point `(mx, my)` into a single span. Idempotent; no-op when the
/// pair isn't present.
fn coalesce_at(segs: &mut Vec<Segment>, mx: f64, my: f64) {
    // Horizontal pair through (mx, my): two segments on y == my,
    // one ending at x == mx, another starting at x == mx.
    let mut left: Option<usize> = None;
    let mut right: Option<usize> = None;
    for (i, s) in segs.iter().enumerate() {
        if (s.y1 - my).abs() < EPS && (s.y2 - my).abs() < EPS {
            if (s.x2 - mx).abs() < EPS && (s.x1 - mx).abs() > EPS {
                left = Some(i);
            } else if (s.x1 - mx).abs() < EPS && (s.x2 - mx).abs() > EPS {
                right = Some(i);
            }
        }
    }
    if let (Some(l), Some(r)) = (left, right) {
        let merged = Segment {
            x1: segs[l].x1,
            y1: my,
            x2: segs[r].x2,
            y2: my,
        };
        let (a, b) = if l > r { (l, r) } else { (r, l) };
        segs.remove(a);
        segs.remove(b);
        segs.push(merged);
        return;
    }

    // Vertical pair through (mx, my).
    let mut up: Option<usize> = None;
    let mut down: Option<usize> = None;
    for (i, s) in segs.iter().enumerate() {
        if (s.x1 - mx).abs() < EPS && (s.x2 - mx).abs() < EPS {
            if (s.y2 - my).abs() < EPS && (s.y1 - my).abs() > EPS {
                up = Some(i);
            } else if (s.y1 - my).abs() < EPS && (s.y2 - my).abs() > EPS {
                down = Some(i);
            }
        }
    }
    if let (Some(u), Some(d)) = (up, down) {
        let merged = Segment {
            x1: mx,
            y1: segs[u].y1,
            x2: mx,
            y2: segs[d].y2,
        };
        let (a, b) = if u > d { (u, d) } else { (d, u) };
        segs.remove(a);
        segs.remove(b);
        segs.push(merged);
    }
}

/// Route the signal net by pin count, dispatching to the
/// closed-form 2-pin / 3-pin cases. Returns `(segments, junctions)`.
/// Junctions are emitted only at branch points — i.e. the 3-pin
/// Steiner point when it does not coincide with a pin and at least
/// three segments meet there.
pub(crate) fn route_signal(net: &crate::NetSpec) -> (Vec<Segment>, Vec<(f64, f64)>) {
    match net.pins.len() {
        0 | 1 => (Vec::new(), Vec::new()),
        2 => {
            let a = pin_xy(&net.pins[0]);
            let b = pin_xy(&net.pins[1]);
            (route_two_pin(a, b), Vec::new())
        }
        3 => {
            let pts = [
                pin_xy(&net.pins[0]),
                pin_xy(&net.pins[1]),
                pin_xy(&net.pins[2]),
            ];
            let segs = route_three_pin(pts);
            let junctions = steiner_junctions(&pts, &segs);
            (segs, junctions)
        }
        _ => {
            // N ≥ 4 lands in Task 4 (Hanan-grid DP). For now: passthrough.
            (Vec::new(), Vec::new())
        }
    }
}

fn pin_xy(p: &PinRef) -> (f64, f64) {
    (p.x_mm, p.y_mm)
}

/// Compute junction points for a 3-pin Steiner tree. A junction is
/// emitted at the Steiner point when at least three segment endpoints
/// meet there — this excludes the degenerate cases where the
/// Steiner point coincides with a pin (yielding a plain L-shape).
fn steiner_junctions(pts: &[(f64, f64); 3], segs: &[Segment]) -> Vec<(f64, f64)> {
    let xs = [pts[0].0, pts[1].0, pts[2].0];
    let ys = [pts[0].1, pts[1].1, pts[2].1];
    let mut sx = xs;
    sx.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut sy = ys;
    sy.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (mx, my) = (sx[1], sy[1]);

    // Steiner point coincides with a pin iff (mx, my) equals one of
    // the pins exactly. In that case the routing collapses to two
    // segments and no branch junction is needed.
    let coincident = (0..3).any(|i| (xs[i] - mx).abs() < EPS && (ys[i] - my).abs() < EPS);
    if coincident {
        return Vec::new();
    }

    // Count segment endpoints meeting at (mx, my). A junction is
    // needed only when three or more meet (T-junction / cross).
    let mut hits = 0usize;
    for s in segs {
        if (s.x1 - mx).abs() < EPS && (s.y1 - my).abs() < EPS {
            hits += 1;
        }
        if (s.x2 - mx).abs() < EPS && (s.y2 - my).abs() < EPS {
            hits += 1;
        }
    }
    if hits >= 3 {
        vec![(mx, my)]
    } else {
        Vec::new()
    }
}

/// Render a `Segment` as a `(wire (pts (xy …) (xy …)))` S-expr.
pub(crate) fn segment_to_sexpr(s: &Segment) -> Sexpr {
    let txt = format!(
        "(wire (pts (xy {:.2} {:.2}) (xy {:.2} {:.2})))",
        s.x1, s.y1, s.x2, s.y2
    );
    lexpr::from_str(&txt).expect("wire sexpr parses")
}

/// Render a junction point as a `(junction (at …))` S-expr.
pub(crate) fn junction_sexpr(p: (f64, f64)) -> Sexpr {
    let txt = format!("(junction (at {:.2} {:.2}))", p.0, p.1);
    lexpr::from_str(&txt).expect("junction sexpr parses")
}
