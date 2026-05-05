//! Stage 2a — 2-pin / 3-pin Hwang RSMT tests.
//!
//! Most tests exercise the public `route_two_pin` / `route_three_pin`
//! helpers directly; the final test calls the top-level `route()`
//! pipeline with a Signal net to verify the wires reach the emitted
//! S-expr stream.

use spice_layout::net_class::NetClass;
use spice_route::{
    Direction, NetSpec, PinRef, RouteRequest, route, route_three_pin, route_two_pin,
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
    });
    assert_eq!(count_starting(&r, "(wire"), 2);
    assert_eq!(count_starting(&r, "(junction"), 0);
}

#[test]
fn route_pipeline_emits_junction_for_three_pin_t() {
    // Pins form a clear T: the Steiner point is interior, not on any pin.
    let nets = [signal_net("t", &[(0.0, 5.0), (10.0, 5.0), (5.0, 0.0)])];
    let r = route(RouteRequest {
        nets: &nets,
        scope: "root",
        library: None,
    });
    let wires = count_starting(&r, "(wire");
    assert!((2..=3).contains(&wires), "got {wires} wires");
    assert_eq!(count_starting(&r, "(junction"), 1);
}
