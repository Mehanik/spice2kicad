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

#[test]
fn duplicate_collinear_segments_are_not_merged_into_nothing() {
    // Two IDENTICAL vertical segments (the shared drop a 3-pin Steiner
    // tree emits twice when its Steiner point lands on a pin). They
    // share BOTH endpoints, so the end-to-end merge rule — "rewrite the
    // pair as the span between their far endpoints" — yields a
    // zero-length stub that `drop_zero_length` then deletes, severing
    // the branch. Overlapping segments must be left for
    // `collapse_collinear_overlaps`, which unions them properly.
    let dup = Segment {
        x1: 40.64,
        y1: 25.4,
        x2: 40.64,
        y2: 26.67,
    };
    let mut routed = vec![RoutedNet {
        segments: vec![dup, dup],
        junctions: vec![],
    }];
    coalesce_collinear(&mut routed);
    assert!(
        routed[0]
            .segments
            .iter()
            .any(|s| (seg_len(s) - 1.27).abs() < 1e-6),
        "the 1.27 mm drop must survive coalescing, got {:?}",
        routed[0].segments
    );
}

fn seg_len(s: &Segment) -> f64 {
    (s.x1 - s.x2).abs() + (s.y1 - s.y2).abs()
}
