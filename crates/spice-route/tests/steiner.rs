//! Stage 2a — 2-pin / 3-pin Hwang RSMT tests.
//!
//! Most tests exercise the public `route_two_pin` / `route_three_pin`
//! helpers directly; the final test calls the top-level `route()`
//! pipeline with a Signal net to verify the wires reach the emitted
//! S-expr stream.

use spice_layout::net_class::NetClass;
use spice_route::{
    Direction, NetSpec, PinRef, RouteRequest, route, route_n_pin, route_three_pin, route_two_pin,
};

const EPS: f64 = 1e-6;

fn seg_len(s: &spice_route::Segment) -> f64 {
    (s.x1 - s.x2).abs() + (s.y1 - s.y2).abs()
}

fn total_len(segs: &[spice_route::Segment]) -> f64 {
    segs.iter().map(seg_len).sum()
}

#[test]
fn two_pin_collinear_x_emits_single_segment() {
    let segs = route_two_pin((0.0, 0.0), (10.0, 0.0));
    assert_eq!(segs.len(), 1);
    let s = segs[0];
    assert!(
        ((s.x1 - 0.0).abs() < EPS && (s.x2 - 10.0).abs() < EPS)
            || ((s.x1 - 10.0).abs() < EPS && (s.x2 - 0.0).abs() < EPS)
    );
    assert!((s.y1 - s.y2).abs() < EPS);
}

#[test]
fn two_pin_collinear_y_emits_single_segment() {
    let segs = route_two_pin((0.0, 0.0), (0.0, 10.0));
    assert_eq!(segs.len(), 1);
    let s = segs[0];
    assert!((s.x1 - s.x2).abs() < EPS);
}

#[test]
fn two_pin_diagonal_emits_l_shape() {
    let segs = route_two_pin((0.0, 0.0), (10.0, 5.0));
    assert_eq!(segs.len(), 2);
    // Total Manhattan length is |dx| + |dy| = 15.
    assert!((total_len(&segs) - 15.0).abs() < EPS);
    // The two segments meet at exactly one point — the bend corner.
    let endpoints = [
        (segs[0].x1, segs[0].y1),
        (segs[0].x2, segs[0].y2),
        (segs[1].x1, segs[1].y1),
        (segs[1].x2, segs[1].y2),
    ];
    let mut shared = 0;
    for i in 0..endpoints.len() {
        for j in (i + 1)..endpoints.len() {
            if (endpoints[i].0 - endpoints[j].0).abs() < EPS
                && (endpoints[i].1 - endpoints[j].1).abs() < EPS
            {
                shared += 1;
            }
        }
    }
    assert_eq!(
        shared, 1,
        "L-shape segments must meet at exactly one corner"
    );
}

#[test]
fn three_pin_steiner_point_is_median() {
    // pins (0,0), (10,0), (5,10): median X = 5, median Y = 0.
    // Steiner point (5, 0) sits on the edge between (0,0) and (10,0).
    // Total RSMT wire length = 5 + 5 + 10 = 20.
    let segs = route_three_pin([(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)]);
    assert!(
        (total_len(&segs) - 20.0).abs() < EPS,
        "expected total length 20, got {}",
        total_len(&segs)
    );
}

#[test]
fn three_pin_collinear_x_no_steiner_branch() {
    // Median X = 5, median Y = 0; pin at (5,0) is the Steiner point.
    let segs = route_three_pin([(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)]);
    assert_eq!(segs.len(), 2, "got segs: {segs:?}");
    assert!((total_len(&segs) - 10.0).abs() < EPS);
}

#[test]
fn three_pin_l_shape() {
    // Pins (0,0), (10,10), (10,0). Median X = 10, median Y = 0.
    // Steiner point (10, 0) coincides with the third pin → two segments.
    let segs = route_three_pin([(0.0, 0.0), (10.0, 10.0), (10.0, 0.0)]);
    assert_eq!(segs.len(), 2);
    assert!((total_len(&segs) - 20.0).abs() < EPS);
}

fn signal_net(name: &str, pins: &[(f64, f64)]) -> NetSpec {
    NetSpec {
        name: name.into(),
        class: NetClass::Signal,
        pins: pins
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| PinRef {
                element_idx: i,
                pin_number: 1,
                x_mm: x,
                y_mm: y,
                outward: Direction::Right,
                drives: false,
                requires_driver: false,
                on_sheet_edge: false,
            })
            .collect(),
        negative_rail: false,
    }
}

fn count_starting(out: &spice_route::RouteResult, prefix: &str) -> usize {
    out.sexprs
        .iter()
        .map(std::string::ToString::to_string)
        .filter(|s| s.starts_with(prefix))
        .count()
}

#[test]
fn route_pipeline_emits_wires_for_two_pin_signal_net() {
    let nets = [signal_net("n", &[(0.0, 0.0), (10.0, 5.0)])];
    let r = route(RouteRequest {
        nets: &nets,
        scope: "root",
        library: None,
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    // The router may emit a third segment as an outward-direction
    // stub when the synthetic `Direction::Right` outward on both pins
    // is not satisfied by either L corner (V5 enforcement). Accept
    // the 2- or 3-segment outcome here; the closed-form 2-pin tests
    // above already pin down the unconstrained shape.
    let wires = count_starting(&r, "(wire");
    assert!(
        (2..=3).contains(&wires),
        "expected 2 or 3 wires, got {wires}"
    );
    assert_eq!(count_starting(&r, "(junction"), 0);
}

// ----- Stage 2b: N ≥ 4 -----

#[test]
fn four_pin_square_emits_steiner_tree() {
    // Pins at the corners of a 10×10 square. Optimal RSMT length is
    // 30 (any spanning of 3 sides; a centered Steiner cross is also
    // 30). Our MST baseline is 30 already, so Steinerization either
    // keeps it or improves only ties.
    let segs = route_n_pin(&[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0), (10.0, 10.0)]);
    let len = total_len(&segs);
    assert!(
        (len - 30.0).abs() < EPS,
        "expected total length 30, got {len}, segs: {segs:?}"
    );
}

#[test]
fn four_pin_t_pattern() {
    // Plus-shaped fixture: pins on the four cardinals around (5,5).
    // Optimal RSMT has a single Steiner point at the centre with
    // four 5-unit spokes — total length 20. The baseline MST
    // (without Steiner) connects them as a chain and costs 30; the
    // Borah-Owens-Irwin pass must rescue 10 units.
    let segs = route_n_pin(&[(0.0, 5.0), (10.0, 5.0), (5.0, 0.0), (5.0, 10.0)]);
    let len = total_len(&segs);
    assert!(
        (len - 20.0).abs() < EPS,
        "expected total length 20 (with Steiner), got {len}; segs: {segs:?}"
    );
}

#[test]
fn five_pin_known_layout() {
    // Five pins: corners of a 10×10 square + centre. The optimal
    // RSMT exploits Steiner points at (0,5) and (10,5): two vertical
    // 10-unit spans on x=0 and x=10 (covering the four corners) plus
    // a horizontal 10-unit span at y=5 connecting them through the
    // centre pin. Total = 10+10+10 = 30. This is strictly better
    // than the naive "centre as hub" cost of 40, and our Hanan-grid
    // Steinerization should find it.
    let pins = [
        (0.0, 0.0),
        (10.0, 0.0),
        (0.0, 10.0),
        (10.0, 10.0),
        (5.0, 5.0),
    ];
    let segs = route_n_pin(&pins);
    let len = total_len(&segs);
    assert!(
        (len - 30.0).abs() < EPS,
        "expected optimal RSMT length 30, got {len}; segs: {segs:?}"
    );
}

#[test]
fn eight_pin_runtime_under_100ms() {
    // Sanity: 8-pin fixture must complete under 100 ms. Hanan grid
    // is 8 × 8 = 64 candidates per pass, manageable in pure Rust.
    let pins: Vec<(f64, f64)> = vec![
        (0.0, 0.0),
        (12.7, 0.0),
        (25.4, 0.0),
        (0.0, 12.7),
        (12.7, 12.7),
        (25.4, 12.7),
        (6.35, 6.35),
        (19.05, 19.05),
    ];
    let start = std::time::Instant::now();
    let segs = route_n_pin(&pins);
    let dt = start.elapsed();
    assert!(!segs.is_empty(), "expected segments");
    assert!(
        dt < std::time::Duration::from_millis(100),
        "8-pin route took {dt:?}, expected < 100ms"
    );
}

/// Rectilinear (Manhattan) MST length over `pins`, computed
/// independently of the router so tests can assert the Steinerized
/// tree is never longer than the spanning-tree baseline.
fn rmst_length(pins: &[(f64, f64)]) -> f64 {
    let n = pins.len();
    if n <= 1 {
        return 0.0;
    }
    let man = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() + (a.1 - b.1).abs();
    let mut in_tree = vec![false; n];
    let mut best = vec![f64::INFINITY; n];
    in_tree[0] = true;
    for j in 1..n {
        best[j] = man(pins[0], pins[j]);
    }
    let mut total = 0.0;
    for _ in 1..n {
        let mut pick = usize::MAX;
        let mut pd = f64::INFINITY;
        for j in 0..n {
            if !in_tree[j] && best[j] < pd {
                pick = j;
                pd = best[j];
            }
        }
        if pick == usize::MAX {
            break;
        }
        in_tree[pick] = true;
        total += pd;
        for j in 0..n {
            if !in_tree[j] {
                let d = man(pins[pick], pins[j]);
                if d < best[j] {
                    best[j] = d;
                }
            }
        }
    }
    total
}

#[test]
fn ten_pin_steiner_never_longer_than_mst() {
    // 10 pins → Steinerized rectilinear tree. The Steiner pass may
    // only shorten (or tie) the spanning-tree baseline; it must never
    // produce a longer tree, and it must remain a valid connected
    // structure (≥ N-1 segments, finite positive length).
    let pins: Vec<(f64, f64)> = (0..10_i32)
        .map(|i| {
            let f = f64::from(i);
            (f * 7.0 % 30.0, (f * 11.0) % 25.0)
        })
        .collect();
    let segs = route_n_pin(&pins);
    assert!(
        segs.len() >= pins.len() - 1,
        "expected ≥ {} segments for {} pins, got {}",
        pins.len() - 1,
        pins.len(),
        segs.len()
    );
    let len = total_len(&segs);
    let mst = rmst_length(&pins);
    assert!(len.is_finite() && len > 0.0, "got total len {len}");
    assert!(
        len <= mst + EPS,
        "Steinerized tree (len {len}) must not exceed MST baseline {mst}"
    );
}

#[test]
fn twelve_pin_steiner_strictly_beats_mst() {
    // A double "plus" sign: two overlapping cross fixtures that share a
    // common vertical spine. Each cross has four arm pins that pull
    // toward a central median, so Hanan-grid Steiner points strictly
    // shorten the spanning tree. With N = 12 this exercises the
    // large-N (≥ 10) Steiner path, which previously fell back to plain
    // RMST and therefore could NOT find these savings.
    //
    // Three independent "plus" crosses, each with four arm pins around
    // an EMPTY centre. The optimal RSMT for each plus adds a Steiner
    // point at the (empty) centre and runs four spokes to it; the
    // spanning-tree baseline must instead chain the arms and is
    // strictly longer. The three crosses are placed far apart so the
    // saving is per-cross and unambiguous.
    let plus = |cx: f64, cy: f64| {
        [
            (cx - 10.0, cy),
            (cx + 10.0, cy),
            (cx, cy - 10.0),
            (cx, cy + 10.0),
        ]
    };
    let mut pins: Vec<(f64, f64)> = Vec::new();
    pins.extend_from_slice(&plus(0.0, 0.0));
    pins.extend_from_slice(&plus(60.0, 0.0));
    pins.extend_from_slice(&plus(120.0, 0.0));
    let segs = route_n_pin(&pins);
    let len = total_len(&segs);
    let mst = rmst_length(&pins);
    assert!(
        len + EPS < mst,
        "expected Steinerized tree strictly shorter than MST {mst}, got {len}"
    );
}

#[test]
fn large_n_steiner_completes_in_time() {
    // ADR-8 perf floor: a single net with 60 pins must Steinerize in
    // interactive time. A 60-pin net is already far beyond any real
    // ngspice-tractable signal net (those rarely exceed a handful of
    // pins); this guards the O() bound on the large-N Steiner path.
    let pins: Vec<(f64, f64)> = (0..60_i32)
        .map(|i| {
            let f = f64::from(i);
            ((f * 13.0) % 40.0, (f * 17.0) % 40.0)
        })
        .collect();
    let start = std::time::Instant::now();
    let segs = route_n_pin(&pins);
    let dt = start.elapsed();
    let len = total_len(&segs);
    let mst = rmst_length(&pins);
    assert!(!segs.is_empty(), "expected segments");
    assert!(
        len <= mst + EPS,
        "Steiner tree (len {len}) must not exceed MST {mst}"
    );
    assert!(
        dt < std::time::Duration::from_secs(2),
        "60-pin route took {dt:?}, expected < 2s (ADR-8 interactive)"
    );
}

#[test]
fn route_pipeline_emits_junction_for_three_pin_t() {
    // Pins form a clear T: the Steiner point is interior, not on any pin.
    let nets = [signal_net("t", &[(0.0, 5.0), (10.0, 5.0), (5.0, 0.0)])];
    let r = route(RouteRequest {
        nets: &nets,
        scope: "root",
        library: None,
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    let wires = count_starting(&r, "(wire");
    assert!((2..=3).contains(&wires), "got {wires} wires");
    assert_eq!(count_starting(&r, "(junction"), 1);
}
