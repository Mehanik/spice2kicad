//! Stage 3 — cross-net endpoint conflict resolution tests.

use spice_route::conflict::resolve_conflicts;
use spice_route::types::{RoutedNet, Segment};

#[test]
fn two_nets_sharing_endpoint_get_jogged_apart() {
    // Both nets touch (5.08, 0.0).
    let mut routed = vec![
        RoutedNet {
            segments: vec![Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 5.08,
                y2: 0.0,
            }],
            junctions: vec![],
        },
        RoutedNet {
            segments: vec![Segment {
                x1: 5.08,
                y1: 0.0,
                x2: 10.16,
                y2: 0.0,
            }],
            junctions: vec![],
        },
    ];
    let warnings = resolve_conflicts::<std::collections::hash_map::RandomState>(&mut routed, &[]);
    // Walk endpoints — no coordinate should be touched by both nets.
    #[allow(clippy::cast_possible_truncation)]
    let key = |x: f64, y: f64| -> (i64, i64) {
        ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
    };
    let mut points: Vec<std::collections::HashSet<(i64, i64)>> = Vec::new();
    for net in &routed {
        let mut s = std::collections::HashSet::new();
        for seg in &net.segments {
            for (x, y) in [(seg.x1, seg.y1), (seg.x2, seg.y2)] {
                s.insert(key(x, y));
            }
        }
        points.push(s);
    }
    let intersect: std::collections::HashSet<_> =
        points[0].intersection(&points[1]).copied().collect();
    assert!(
        intersect.is_empty(),
        "shared endpoints between distinct nets after resolve: {intersect:?} warnings={warnings:?}"
    );
}

#[test]
fn disjoint_nets_unchanged() {
    let mut routed = vec![
        RoutedNet {
            segments: vec![Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 5.08,
                y2: 0.0,
            }],
            junctions: vec![],
        },
        RoutedNet {
            segments: vec![Segment {
                x1: 20.0,
                y1: 20.0,
                x2: 30.0,
                y2: 20.0,
            }],
            junctions: vec![],
        },
    ];
    let before = routed.clone();
    let warnings = resolve_conflicts::<std::collections::hash_map::RandomState>(&mut routed, &[]);
    assert!(warnings.is_empty());
    assert_eq!(before.len(), routed.len());
    for (a, b) in before.iter().zip(routed.iter()) {
        assert_eq!(a.segments.len(), b.segments.len());
    }
}
