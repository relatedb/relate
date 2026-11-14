//! Exact logical-memory pins for the shared shape generator (ADR-0042).
//! These integers move only when the live-graph formula changes.

use relate::Database;
use relate_datasets::shapes::{
    census, statements, GraphShape, IndexCounts, ShapeScale, DEFAULT_NODE_ATTRIBUTES,
};

/// Measured `Database::memory_usage` for [`ShapeScale::Small`] at one attribute
/// and no indexes.
// Every pin is 129 bytes smaller: ADR-0229 dropped the
// always-allocated empty `system_ontologies` graph.
// Property slots use `size_of::<Value>()`. `Node`, `Relationship`, and
// `Path` live behind `Arc`, so that slot is 32 bytes smaller than when
// those payloads were inline in the enum.
// The stub label cache is charged once per distinct label-id list.
// The rel-bearing shapes' bytes include the (label, rel-type) degree
// histograms' accounting term (+288 at Small scale, LABS-351) and the
// (start-label, rel-type, end-label) pair counts' term (+132 at Small
// scale, LABS-370) — the entries each shape's label triples create,
// charged to used_bytes with the rest of the retained structure.
// LABS-370 v1 follow-up: every pin with a node population is +124 per
// distinct node label — the exact label-count map's LABEL_COUNT_ENTRY_BYTES
// entries (Flat/Ring/DenseRing one label, Fanout two).
// ADR-0529: a never-updated node or relationship is an inline slot, not a
// one-version list — 32 bytes less per node, 24 less per relationship.
// ADR-0532: those slots sit in an id vector. The hash-map slot is gone and
// the array reserves the next power of two, so flat (1,000 nodes, no
// relationships) drops another 22,080. Relationship shapes also drop the
// relationship and adjacency map slots.
// ADR-0589: each distinct label set adds one entry to the label-set map
// (148 bytes for a one-label set): flat and ring carry one set, fanout two.
const BASE: u64 = 258;
const RING: u64 = 575_784;
const FANOUT: u64 = 520_046;
const FLAT: u64 = 222_688;
const DENSE_RING: u64 = 2_256_986;
/// One label set like ring and dense ring (ADR-0589's +148), one node
/// label count entry, and the same per-relationship terms as dense ring
/// at 96,512 relationships — 12× dense ring's volume. The adjacency is
/// the dominant term: 96 average hops on both endpoints of every node.
const MID_DEGREE: u64 = 28_850_522;
/// Flat, one attribute, one range index on `id`. `1,000 × 62` on top of [`FLAT`].
const FLAT_RANGE: u64 = 284_688;
/// Flat, one attribute, plus one text, one point, and one range index.
/// The text term is 14 width-3 postings on `abcdefghijklmnop` (ADR-0207).
/// The index term is unchanged by the id vector, so this is the previous
/// pin minus flat's 22,080.
const FLAT_INDEXED: u64 = 688_822;

fn apply(
    db: &Database,
    shape: GraphShape,
    scale: ShapeScale,
    indexes: IndexCounts,
    attributes: u32,
) {
    let mut conn = db.connect().unwrap();
    for statement in statements(shape, scale, indexes, attributes).unwrap() {
        conn.query(&statement)
            .unwrap_or_else(|e| panic!("{statement}: {e}"));
    }
}

fn measure(db: &Database, shape: GraphShape, indexes: IndexCounts, attributes: u32) -> u64 {
    let expected = census(shape, ShapeScale::Small, indexes, attributes).unwrap();
    let mut conn = db.connect().unwrap();
    // The pins include the stub label cache (charged once per distinct
    // label-id list, on a node's first materialization), so the measure
    // materializes every node once — explicitly, never as a side effect
    // of the count: `count(n)` answers from the count store (LABS-851)
    // without touching a node, and a replayed graph has materialized
    // nothing until a read does.
    let warmed = scalar(&mut conn, "MATCH (n) RETURN count(labels(n)) AS n");
    assert_eq!(warmed, expected.nodes as i64, "{shape:?} warmed nodes");
    let nodes = scalar(&mut conn, "MATCH (n) RETURN count(n) AS n");
    let rels = scalar(&mut conn, "MATCH ()-[r]->() RETURN count(r) AS n");
    assert_eq!(nodes, expected.nodes as i64, "{shape:?} nodes");
    assert_eq!(
        rels, expected.relationships as i64,
        "{shape:?} relationships"
    );
    let (used, limit) = db.memory_usage().unwrap();
    assert_eq!(limit, None);
    used
}

fn load(shape: GraphShape) -> u64 {
    load_with(shape, IndexCounts::NONE, DEFAULT_NODE_ATTRIBUTES)
}

fn load_with(shape: GraphShape, indexes: IndexCounts, attributes: u32) -> u64 {
    let db = Database::open(":memory:").unwrap();
    apply(&db, shape, ShapeScale::Small, indexes, attributes);
    measure(&db, shape, indexes, attributes)
}

fn scalar(conn: &mut relate::Connection, cypher: &str) -> i64 {
    let result = conn.query(cypher).unwrap();
    result.rows[0][0].as_int().unwrap()
}

#[test]
fn small_shapes_pin_logical_bytes() {
    let base = load(GraphShape::Base);
    let flat = load(GraphShape::Flat);
    let fanout = load(GraphShape::Fanout);
    let ring = load(GraphShape::Ring);
    let dense = load(GraphShape::DenseRing);
    let mid = load(GraphShape::MidDegree);
    assert_eq!(
        (base, flat, fanout, ring, dense, mid),
        (BASE, FLAT, FANOUT, RING, DENSE_RING, MID_DEGREE),
        "measured base={base} flat={flat} fanout={fanout} ring={ring} dense={dense} mid={mid}"
    );
}

/// A second integer property is 12 payload bytes plus the property-vector
/// growth from `reserved(1)` to `reserved(2)`: 4 more key bytes and one
/// more value slot (`size_of::<Value>()`). The new key `a1` is interned once.
#[test]
fn a_second_attribute_grows_the_property_vectors() {
    let one = load(GraphShape::Ring);
    let two = load_with(GraphShape::Ring, IndexCounts::NONE, 2);
    assert_eq!(two - one, 64 * 1_000 + 66);
}

/// A range index on `id` adds one posting per node. Text and point add
/// their property and their postings. The combined pin is one of each.
#[test]
fn one_of_each_index_pins_flat_logical_bytes() {
    let range_only = load_with(
        GraphShape::Flat,
        IndexCounts {
            text: 0,
            point: 0,
            range: 1,
        },
        DEFAULT_NODE_ATTRIBUTES,
    );
    assert_eq!(range_only, FLAT_RANGE, "range-only flat={range_only}");
    let indexed = IndexCounts {
        text: 1,
        point: 1,
        range: 1,
    };
    let combined = load_with(GraphShape::Flat, indexed, DEFAULT_NODE_ATTRIBUTES);
    assert_eq!(
        (range_only, combined),
        (FLAT_RANGE, FLAT_INDEXED),
        "range-only={range_only} combined={combined}"
    );
}

// Disk tier (ADR-0251): WAL replay is the behavior under test, and the
// engine appends to the WAL only on a durable VFS — a shared MemoryVfs
// re-open finds nothing to replay — so this test keeps a real directory.
#[test]
fn ring_replay_reports_the_same_logical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ring");
    let path = path.to_str().unwrap();
    let used = {
        let db = Database::open(path).unwrap();
        apply(
            &db,
            GraphShape::Ring,
            ShapeScale::Small,
            IndexCounts::NONE,
            DEFAULT_NODE_ATTRIBUTES,
        );
        let used = measure(
            &db,
            GraphShape::Ring,
            IndexCounts::NONE,
            DEFAULT_NODE_ATTRIBUTES,
        );
        assert_eq!(used, RING);
        used
    };
    let db = Database::open(path).unwrap();
    assert_eq!(
        measure(
            &db,
            GraphShape::Ring,
            IndexCounts::NONE,
            DEFAULT_NODE_ATTRIBUTES
        ),
        used,
        "replayed file graph"
    );
}
