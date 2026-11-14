//! The shape generator's tests, split from the shapes themselves at
//! the thousand-line ceiling (the memory_shape/tests.rs pattern).
use super::*;

fn none_small(shape: GraphShape) -> ShapeCensus {
    census(
        shape,
        ShapeScale::Small,
        IndexCounts::NONE,
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap()
}

#[test]
fn statements_are_stable_and_indexes_apply_to_every_filled_shape() {
    let once = statements(
        GraphShape::Ring,
        ShapeScale::Small,
        IndexCounts::NONE,
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap();
    let twice = statements(
        GraphShape::Ring,
        ShapeScale::Small,
        IndexCounts::NONE,
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap();
    assert_eq!(once, twice);
    assert!(once
        .iter()
        .any(|s| s.contains("id: 999") && s.contains("id: 0")));
    assert!(once.iter().all(|s| !s.contains("CREATE TEXT INDEX")));
    assert!(statements(GraphShape::Flat, ShapeScale::Small, IndexCounts::NONE, 0).is_err());
    assert!(statements(
        GraphShape::Flat,
        ShapeScale::Small,
        IndexCounts {
            text: 0,
            point: 0,
            range: 2
        },
        1
    )
    .is_err());

    let indexed = IndexCounts {
        text: 1,
        point: 1,
        range: 1,
    };
    let ring = statements(
        GraphShape::Ring,
        ShapeScale::Small,
        indexed,
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap();
    assert!(ring.iter().any(|s| s.contains("CREATE TEXT INDEX Ring_s0")));
    assert!(ring
        .iter()
        .any(|s| s.contains("CREATE POINT INDEX Ring_loc0")));
    assert!(ring
        .iter()
        .any(|s| s.contains("CREATE RANGE INDEX Ring_id")));
    assert!(ring.iter().any(|s| s.contains("s0: 'abcdefghijklmnop'")));
    assert!(statements(
        GraphShape::Base,
        ShapeScale::Small,
        indexed,
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap()
    .is_empty());

    let fanout = statements(
        GraphShape::Fanout,
        ShapeScale::Small,
        IndexCounts {
            text: 1,
            point: 0,
            range: 0,
        },
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap();
    assert!(fanout
        .iter()
        .any(|s| s.contains("CREATE TEXT INDEX Hub_s0")));
    assert!(fanout
        .iter()
        .any(|s| s.contains("CREATE TEXT INDEX Leaf_s0")));
    assert_eq!(indexed.token(), "text1+point1+range1");
    assert_eq!(IndexCounts::NONE.token(), "none");
}

#[test]
fn mid_degree_ladder_rungs_bound_and_average_the_seal_floor() {
    // The shape's contract with the sealed-delta tier (ADR-0551): every
    // rung is at or above the seal floor, inside the mid-degree band,
    // and a power of two so the logical charge is exact.
    for degree in MID_DEGREE_LADDER {
        assert!(degree >= 32, "rung {degree} dips under the seal floor");
        assert!(degree <= 512, "rung {degree} leaves the mid-degree band");
        assert!(
            degree.is_power_of_two(),
            "rung {degree} is not a power of two"
        );
    }
    // The derived facts follow the table: the floor is its minimum, the
    // group its length, the average its sum over its length.
    assert_eq!(MID_DEGREE_FLOOR, *MID_DEGREE_LADDER.iter().min().unwrap());
    assert_eq!(MID_DEGREE_GROUP, MID_DEGREE_LADDER.len() as u32);
    assert_eq!(
        MID_DEGREE_AVG * u64::from(MID_DEGREE_GROUP),
        u64::from(MID_DEGREE_GROUP_HOPS),
        "the average is the group's hops over its nodes, exact"
    );
    // The ladder is periodic, not a head: the same rungs repeat.
    assert_eq!(MID_DEGREE_LADDER[0], 512);
    assert_eq!(MID_DEGREE_LADDER[1], 256);
    assert_eq!(MID_DEGREE_LADDER[15], 32);
}

#[test]
fn mid_degree_statements_are_one_per_group_and_generated_from_the_ladder() {
    let once = statements(
        GraphShape::MidDegree,
        ShapeScale::Small,
        IndexCounts::NONE,
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap();
    let twice = statements(
        GraphShape::MidDegree,
        ShapeScale::Small,
        IndexCounts::NONE,
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap();
    assert_eq!(once, twice);
    // One create batch for the 1,000 nodes, then one link statement per
    // 16-node ladder group: 62 whole groups plus the 8-node trailing
    // half-group.
    assert_eq!(once.len(), 64, "{:?} statements", once.len());
    assert!(once[0].contains("CREATE (:MidDegree"));
    // The ladder's CASE arms come from the table: the rungs above the
    // floor are named, the floor is the ELSE.
    assert!(once.iter().skip(1).all(|s| s.contains("CASE i % 16")));
    assert!(once[1].contains("WHEN 0 THEN 512 WHEN 1 THEN 256"));
    assert!(once[1].contains("ELSE 32 END"));
    assert!(
        !once[1].contains("WHEN 8"),
        "the 32-floor rungs are the ELSE"
    );
    // A link statement links against the full pin count.
    assert!(once[1].contains("% 1000"));
    assert!(once.iter().all(|s| !s.contains("CREATE TEXT INDEX")));
}

#[test]
fn mid_degree_census_pins() {
    // The pin's 1,000 nodes are not a whole group count: the trailing
    // 8-node half-group carries the ladder's head (1,280 hops), so the
    // pin census is 96,512 relationships — summed from the ladder,
    // never a hand-typed constant.
    assert_eq!(
        none_small(GraphShape::MidDegree),
        ShapeCensus {
            nodes: 1_000,
            relationships: 96_512,
        }
    );
    // Large lands on whole 16-node groups so the average degree of 96
    // is exact: the census solver's own answer at the default target,
    // mirrored by the loadtest's fill target.
    let target = ShapeScale::Large {
        logical_bytes: DEFAULT_LOGICAL_TARGET_BYTES,
    };
    let got = census(
        GraphShape::MidDegree,
        target,
        IndexCounts::NONE,
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap();
    assert_eq!(
        got,
        ShapeCensus {
            nodes: 1_184,
            relationships: 113_664,
        }
    );
    assert_eq!(
        got.nodes % u64::from(MID_DEGREE_GROUP),
        0,
        "whole ladder groups only"
    );
    assert_eq!(got.relationships, got.nodes * MID_DEGREE_AVG);
}

#[test]
fn census_matches_the_scale_constants() {
    assert_eq!(
        none_small(GraphShape::Base),
        ShapeCensus {
            nodes: 0,
            relationships: 0
        }
    );
    assert_eq!(
        none_small(GraphShape::Ring),
        ShapeCensus {
            nodes: 1_000,
            relationships: 1_000
        }
    );
    assert_eq!(
        none_small(GraphShape::Fanout),
        ShapeCensus {
            nodes: 1_000,
            relationships: 996
        }
    );
    assert_eq!(none_small(GraphShape::Flat).nodes, 1_000);
    assert_eq!(
        none_small(GraphShape::DenseRing),
        ShapeCensus {
            nodes: 1_000,
            relationships: 8_000
        }
    );
    let target = ShapeScale::Large {
        logical_bytes: DEFAULT_LOGICAL_TARGET_BYTES,
    };
    let attrs = DEFAULT_NODE_ATTRIBUTES;
    let none = IndexCounts::NONE;
    assert_eq!(
        census(GraphShape::Ring, target, none, attrs).unwrap(),
        ShapeCensus {
            nodes: 46_603,
            relationships: 46_603
        }
    );
    assert_eq!(
        census(GraphShape::Fanout, target, none, attrs).unwrap(),
        ShapeCensus {
            nodes: 16 + 16 * 3_244,
            relationships: 16 * 3_244
        }
    );
    assert_eq!(
        census(GraphShape::Flat, target, none, attrs).unwrap().nodes,
        121_574
    );
    assert_eq!(
        census(GraphShape::DenseRing, target, none, attrs).unwrap(),
        ShapeCensus {
            nodes: 5_010,
            relationships: 5_010 * 20
        }
    );
    let text = IndexCounts {
        text: 1,
        point: 0,
        range: 0,
    };
    assert_eq!(
        census(GraphShape::Flat, target, text, attrs).unwrap().nodes,
        15_190
    );
    assert!(census(GraphShape::Ring, target, text, attrs).unwrap().nodes < 46_603);
    assert!(
        census(GraphShape::Fanout, target, text, attrs)
            .unwrap()
            .nodes
            < 16 + 16 * 3_244
    );
    assert!(
        census(GraphShape::DenseRing, target, text, attrs)
            .unwrap()
            .nodes
            < 5_010
    );
    let half = ShapeScale::Large {
        logical_bytes: DEFAULT_LOGICAL_TARGET_BYTES / 2,
    };
    let ring_half = census(GraphShape::Ring, half, none, attrs).unwrap().nodes;
    assert_eq!(ring_half, 23_302);
    assert!(census(
        GraphShape::Ring,
        ShapeScale::Large { logical_bytes: 0 },
        none,
        attrs
    )
    .is_err());
    let wider = census(GraphShape::Ring, target, none, 2).unwrap().nodes;
    assert_eq!(wider, 42_799);
}

/// Fanout used to cross every hub with one leaf window (`16 × 2,000`
/// rows). Dense ring used to link the whole graph in one statement.
/// Both now stay within the same 2,000-row cap as flat and ring.
#[test]
fn relationship_batches_match_the_node_batch() {
    let target = ShapeScale::Large {
        logical_bytes: DEFAULT_LOGICAL_TARGET_BYTES,
    };
    let none = IndexCounts::NONE;
    let attrs = DEFAULT_NODE_ATTRIBUTES;

    let dense = statements(GraphShape::DenseRing, target, none, attrs).unwrap();
    let links: Vec<_> = dense.iter().filter(|s| s.contains("[:LINK]")).collect();
    assert!(
        links.len() > 1,
        "dense links must be split across statements"
    );
    for statement in &links {
        assert!(!statement.contains("collect("));
        let rows = unwind_width(statement, "i") * unwind_width(statement, "d");
        assert!(rows <= u64::from(BATCH), "{statement}");
    }

    let fanout = statements(GraphShape::Fanout, target, none, attrs).unwrap();
    let leaves: Vec<_> = fanout.iter().filter(|s| s.contains("[:LEAF]")).collect();
    assert!(leaves.len() > 16, "each hub needs its own leaf batches");
    for statement in &leaves {
        assert!(
            !statement.contains("UNWIND range(0, 15)"),
            "one statement must not cross every hub"
        );
        assert!(unwind_width(statement, "l") <= u64::from(BATCH));
    }
}

fn unwind_width(statement: &str, alias: &str) -> u64 {
    let marker = format!("AS {alias}");
    let head = statement
        .split(&marker)
        .next()
        .and_then(|prefix| prefix.rsplit("UNWIND range(").next())
        .unwrap_or_else(|| panic!("no UNWIND for {alias} in {statement}"));
    let (start, last) = head
        .trim()
        .trim_end_matches(')')
        .split_once(", ")
        .unwrap_or_else(|| panic!("range in {head}"));
    let start: u64 = start.parse().unwrap();
    let last: u64 = last.parse().unwrap();
    last - start + 1
}

#[test]
fn fill_cursor_keeps_emitting_the_large_batch_shape() {
    let mut base = FillCursor::new(GraphShape::Base, 1, IndexCounts::NONE);
    assert!(base.next_statement().unwrap().is_none());

    let mut flat = FillCursor::new(GraphShape::Flat, 1, IndexCounts::NONE);
    let first = flat.next_statement().unwrap().unwrap();
    assert!(first.contains("range(0, 1999)") && first.contains(":Flat"));
    flat.ack().unwrap();
    let second = flat.next_statement().unwrap().unwrap();
    assert!(second.contains("range(2000, 3999)"));
    flat.nack().unwrap();
    assert_eq!(flat.next_statement().unwrap().unwrap(), second);

    let mut ring = FillCursor::new(GraphShape::Ring, 1, IndexCounts::NONE);
    ring.next_statement().unwrap().unwrap();
    ring.ack().unwrap();
    assert!(ring.ring_wrap_statement().is_none());
    let link = ring.next_statement().unwrap().unwrap();
    assert!(link.contains("[:NEXT]"));
    ring.ack().unwrap();
    assert_eq!((ring.nodes, ring.rels), (2_000, 1_999));
    let wrap = ring.ring_wrap_statement().unwrap();
    assert!(wrap.contains("id: 1999") && wrap.contains("id: 0"));

    let mut dense = FillCursor::new(GraphShape::DenseRing, 1, IndexCounts::NONE);
    let window = dense.next_statement().unwrap().unwrap();
    assert!(window.contains("range(0, 99)"));
    assert!(window.contains("% 100") && window.contains("range(1, 20)"));
    dense.ack().unwrap();
    assert_eq!((dense.nodes, dense.rels), (100, 2_000));

    let mut fanout = FillCursor::new(GraphShape::Fanout, 1, IndexCounts::NONE);
    let hubs = fanout.next_statement().unwrap().unwrap();
    assert!(hubs.contains("range(0, 15)") && hubs.contains(":Hub"));
    fanout.ack().unwrap();
    let leaves = fanout.next_statement().unwrap().unwrap();
    assert!(leaves.contains(":Leaf") && leaves.contains("range(0, 1999)"));
    fanout.ack().unwrap();
    assert_eq!((fanout.nodes, fanout.rels), (16 + 2_000, 2_000));

    let indexed = IndexCounts {
        text: 1,
        point: 0,
        range: 0,
    };
    let mut text = FillCursor::new(GraphShape::Flat, 1, indexed);
    let ddl = text.next_statement().unwrap().unwrap();
    assert!(ddl.contains("CREATE TEXT INDEX Flat_s0"));
    text.ack().unwrap();
    assert_eq!(text.nodes, 0);
}

/// [`ALL`]'s doc promises the RSS matrix covers the same set — the
/// matrix's *order* is the report's, its *members* are the inventory's.
#[test]
fn rss_matrix_covers_every_shape() {
    let mut matrix = rss_matrix();
    let mut all = ALL;
    matrix.sort_by_key(|shape| shape.spellings().0);
    all.sort_by_key(|shape| shape.spellings().0);
    assert_eq!(matrix, all);
}

#[test]
fn shape_cli_spelling_roundtrips() {
    let pinned = [
        (GraphShape::Ring, "ring"),
        (GraphShape::Fanout, "fanout"),
        (GraphShape::Flat, "flat"),
        (GraphShape::DenseRing, "dense-ring"),
        (GraphShape::MidDegree, "mid-degree"),
    ];
    // The pinned list is the point (it pins the strings); [`loadable`]
    // keeps it complete, so a new shape cannot skip this test.
    for shape in loadable() {
        assert!(
            pinned.iter().any(|(pinned, _)| *pinned == shape),
            "{shape} has no pinned CLI spelling"
        );
    }
    for (shape, kebab) in pinned {
        assert_eq!(shape.to_string(), kebab);
        assert_eq!(kebab.parse::<GraphShape>().unwrap(), shape);
    }
    assert!("base".parse::<GraphShape>().is_err());
    assert!("dense ring".parse::<GraphShape>().is_err());
    assert!("mid degree".parse::<GraphShape>().is_err());
}
