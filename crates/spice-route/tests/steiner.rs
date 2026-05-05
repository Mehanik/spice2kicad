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
            })
            .collect(),
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
    });
    assert_eq!(count_starting(&r, "(wire"), 2);
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

#[test]
fn ten_pin_falls_back_to_mst() {
    // 10 pins → plain rectilinear MST path (no Steiner). The output
    // is a valid spanning structure: at least N-1 = 9 segments
    // (could be more with L-bends), and total length is finite.
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
    assert!(len.is_finite() && len > 0.0, "got total len {len}");
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
    });
    let wires = count_starting(&r, "(wire");
    assert!((2..=3).contains(&wires), "got {wires} wires");
    assert_eq!(count_starting(&r, "(junction"), 1);
}
