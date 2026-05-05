//! Stage 4 — cleanup tests.

use spice_route::cleanup::{coalesce_collinear, dedup_junctions};
use spice_route::types::{RoutedNet, Segment};

#[test]
fn collinear_chain_coalesces_to_single_segment() {
    let mut routed = vec![RoutedNet {
        segments: vec![
            Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 5.0,
                y2: 0.0,
            },
            Segment {
                x1: 5.0,
                y1: 0.0,
                x2: 10.0,
                y2: 0.0,
            },
            Segment {
                x1: 10.0,
                y1: 0.0,
                x2: 15.0,
                y2: 0.0,
            },
        ],
        junctions: vec![],
    }];
    coalesce_collinear(&mut routed);
    assert_eq!(routed[0].segments.len(), 1, "{routed:?}");
    let s = routed[0].segments[0];
    let xs = [s.x1, s.x2];
    assert!(
        xs.contains(&0.0) && xs.contains(&15.0),
        "merged span: {s:?}"
    );
}

#[test]
fn coincident_junctions_dedup_across_nets() {
    let routed = vec![
        RoutedNet {
            segments: vec![],
            junctions: vec![(5.0, 0.0)],
        },
        RoutedNet {
            segments: vec![],
            junctions: vec![(5.0, 0.0)],
        },
    ];
    let j = dedup_junctions(&routed);
    assert_eq!(j.len(), 1);
}

#[test]
fn distinct_junctions_preserved() {
    let routed = vec![
        RoutedNet {
            segments: vec![],
            junctions: vec![(5.0, 0.0)],
        },
        RoutedNet {
            segments: vec![],
            junctions: vec![(10.0, 0.0)],
        },
    ];
    let j = dedup_junctions(&routed);
    assert_eq!(j.len(), 2);
}
