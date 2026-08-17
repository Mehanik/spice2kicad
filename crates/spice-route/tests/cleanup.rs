//! Stage 4 — cleanup tests.

use spice_route::cleanup::{
    add_connection_junctions, coalesce_collinear, collapse_collinear_overlaps, dedup_junctions,
    split_at_interior_attachments,
};
use spice_route::types::{RoutedNet, Segment};

/// No own pins — for the cases below that exercise wire geometry alone.
/// The pin-bearing cases live in `crates/spice-route/src/cleanup.rs`'s
/// unit tests, next to the rule they check.
const NO_PINS: &[std::collections::HashSet<(i64, i64)>] = &[];

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

// ---------------------------------------------------------------------
// Unsoundness 1 — `try_merge` uses the span between the pair's FAR
// endpoints. That equals their union only for genuine end-to-end
// abutment. For any overlapping pair it is a set DIFFERENCE, deleting
// the run between the shared point and the nearer far endpoint — along
// with the shared point itself, orphaning whatever attached there.
// ---------------------------------------------------------------------

/// True iff `p` is an endpoint of at least one segment (KiCad's only
/// connection point — `SCH_LINE::GetConnectionPoints`).
fn is_endpoint(segments: &[Segment], p: (f64, f64)) -> bool {
    segments.iter().any(|s| {
        ((s.x1 - p.0).abs() < 1e-6 && (s.y1 - p.1).abs() < 1e-6)
            || ((s.x2 - p.0).abs() < 1e-6 && (s.y2 - p.1).abs() < 1e-6)
    })
}

/// True iff `p` lies anywhere on the drawn ink (endpoint or interior).
fn is_covered(segments: &[Segment], p: (f64, f64)) -> bool {
    segments.iter().any(|s| {
        let on_v = (s.x1 - s.x2).abs() < 1e-6
            && (p.0 - s.x1).abs() < 1e-6
            && p.1 >= s.y1.min(s.y2) - 1e-6
            && p.1 <= s.y1.max(s.y2) + 1e-6;
        let on_h = (s.y1 - s.y2).abs() < 1e-6
            && (p.1 - s.y1).abs() < 1e-6
            && p.0 >= s.x1.min(s.x2) - 1e-6
            && p.0 <= s.x1.max(s.x2) + 1e-6;
        on_v || on_h
    })
}

#[test]
fn partially_overlapping_collinear_pair_keeps_its_full_union() {
    // `0→10` and `0→5` share the endpoint x=0. The far-endpoint rule
    // rewrites them as `10→5` — the run `0→5` and the shared point x=0
    // both vanish. Anything anchored at x=0 (a pin, a branch) is
    // silently orphaned: a Tier-0 split net.
    let mut routed = vec![RoutedNet {
        segments: vec![
            Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 10.0,
                y2: 0.0,
            },
            Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 5.0,
                y2: 0.0,
            },
        ],
        junctions: vec![],
    }];
    coalesce_collinear(&mut routed);
    collapse_collinear_overlaps(&mut routed);
    let segs = &routed[0].segments;
    for x in [0.0, 2.5, 5.0, 7.5, 10.0] {
        assert!(
            is_covered(segs, (x, 0.0)),
            "x={x} lost from the union: {segs:?}"
        );
    }
    assert!(
        is_endpoint(segs, (0.0, 0.0)),
        "shared point x=0 must stay an endpoint: {segs:?}"
    );
}

#[test]
fn nested_collinear_pair_keeps_its_full_union() {
    // `0→10` fully contains `2→10`; they share the endpoint x=10. The
    // far-endpoint rule yields `0→2`, deleting 80% of the run.
    let mut routed = vec![RoutedNet {
        segments: vec![
            Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 10.0,
                y2: 0.0,
            },
            Segment {
                x1: 2.0,
                y1: 0.0,
                x2: 10.0,
                y2: 0.0,
            },
        ],
        junctions: vec![],
    }];
    coalesce_collinear(&mut routed);
    collapse_collinear_overlaps(&mut routed);
    let segs = &routed[0].segments;
    for x in [0.0, 1.0, 2.0, 6.0, 10.0] {
        assert!(
            is_covered(segs, (x, 0.0)),
            "x={x} lost from the union: {segs:?}"
        );
    }
}

#[test]
fn vertical_partial_overlap_keeps_its_full_union() {
    // Same defect on the vertical arm of `try_merge`.
    let mut routed = vec![RoutedNet {
        segments: vec![
            Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 0.0,
                y2: 10.0,
            },
            Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 0.0,
                y2: 5.0,
            },
        ],
        junctions: vec![],
    }];
    coalesce_collinear(&mut routed);
    collapse_collinear_overlaps(&mut routed);
    let segs = &routed[0].segments;
    for y in [0.0, 2.5, 5.0, 10.0] {
        assert!(
            is_covered(segs, (0.0, y)),
            "y={y} lost from the union: {segs:?}"
        );
    }
    assert!(
        is_endpoint(segs, (0.0, 0.0)),
        "shared point y=0 must stay an endpoint: {segs:?}"
    );
}

#[test]
fn end_to_end_abutment_still_merges() {
    // The guard must not be so broad it disables the pass: a genuine
    // end-to-end pair (opposite sides of the shared point) still folds
    // into one segment.
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
        ],
        junctions: vec![],
    }];
    coalesce_collinear(&mut routed);
    assert_eq!(routed[0].segments.len(), 1, "{routed:?}");
}

// ---------------------------------------------------------------------
// Unsoundness 2 — a true same-net perpendicular CROSSING. Neither arm
// has an endpoint at the crossing point, so KiCad's endpoint-only rule
// leaves the two arms unconnected. Correct for two different nets;
// within one net it is a split net, and the endpoint-based
// `split_at_interior_attachments` scan cannot see it.
// ---------------------------------------------------------------------

#[test]
fn same_net_crossing_is_split_into_shared_endpoints() {
    let mut routed = vec![RoutedNet {
        segments: vec![
            Segment {
                x1: 5.0,
                y1: -5.0,
                x2: 5.0,
                y2: 5.0,
            },
            Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 10.0,
                y2: 0.0,
            },
        ],
        junctions: vec![],
    }];
    split_at_interior_attachments(&mut routed);
    let segs = &routed[0].segments;
    assert_eq!(
        segs.len(),
        4,
        "both arms must be split at the crossing: {segs:?}"
    );
    assert!(
        is_endpoint(segs, (5.0, 0.0)),
        "crossing must be an endpoint: {segs:?}"
    );
    // Every arm terminates at the crossing.
    for far in [(5.0, -5.0), (5.0, 5.0), (0.0, 0.0), (10.0, 0.0)] {
        assert!(
            segs.iter().any(|s| {
                let a = ((s.x1 - far.0).abs() < 1e-6 && (s.y1 - far.1).abs() < 1e-6)
                    || ((s.x2 - far.0).abs() < 1e-6 && (s.y2 - far.1).abs() < 1e-6);
                let c = ((s.x1 - 5.0).abs() < 1e-6 && (s.y1 - 0.0).abs() < 1e-6)
                    || ((s.x2 - 5.0).abs() < 1e-6 && (s.y2 - 0.0).abs() < 1e-6);
                a && c
            }),
            "no arm runs from {far:?} to the crossing: {segs:?}"
        );
    }
    // Ink is unchanged — only the segmentation differs.
    let total: f64 = segs.iter().map(seg_len).sum();
    assert!((total - 20.0).abs() < 1e-6, "ink changed: {total}");
}

#[test]
fn same_net_crossing_gets_a_junction_dot() {
    let mut routed = vec![RoutedNet {
        segments: vec![
            Segment {
                x1: 5.0,
                y1: -5.0,
                x2: 5.0,
                y2: 5.0,
            },
            Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 10.0,
                y2: 0.0,
            },
        ],
        junctions: vec![],
    }];
    split_at_interior_attachments(&mut routed);
    add_connection_junctions(&mut routed, NO_PINS);
    let j = dedup_junctions(&routed);
    assert_eq!(
        j.len(),
        1,
        "a 4-ray same-net cross needs exactly one dot: {j:?}"
    );
    assert!(
        (j[0].0 - 5.0).abs() < 1e-6 && (j[0].1 - 0.0).abs() < 1e-6,
        "{j:?}"
    );
}

#[test]
fn touching_but_not_crossing_arms_are_left_alone() {
    // A T whose stem already ENDS on the trunk is handled by the
    // endpoint scan; a vertical that merely touches the trunk's
    // endpoint is not a crossing and must not gain a spurious split.
    let mut routed = vec![RoutedNet {
        segments: vec![
            Segment {
                x1: 0.0,
                y1: 0.0,
                x2: 10.0,
                y2: 0.0,
            },
            Segment {
                x1: 10.0,
                y1: 0.0,
                x2: 10.0,
                y2: 5.0,
            },
        ],
        junctions: vec![],
    }];
    split_at_interior_attachments(&mut routed);
    assert_eq!(routed[0].segments.len(), 2, "{routed:?}");
}
