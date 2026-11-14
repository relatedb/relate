//! Deterministic graph shapes for logical-memory pins and the RSS loadtest.
//!
//! [`statements`] emits a fixed list for the `Small` pins. The loadtest uses
//! [`FillCursor`], which emits the same batch shapes and keeps going until
//! the caller stops at the memory limit. The same Cypher runs in-process
//! (`memory_usage`) and inside a separate `relate-bolt` process. Counts,
//! string lengths, and index DDL are fixed so a given
//! `(shape, scale, indexes, attributes)` always charges the same logical bytes.
//!
//! This module owns the shape types, their CLI spellings, and the Cypher
//! statement generators; the closed-form logical-byte arithmetic that turns a
//! target into node counts lives in the sibling [`crate::shape_size`], whose
//! [`census`] is re-exported below.

pub use crate::shape_size::census;

mod fill;

/// Re-exported for the loadtest's fill loop (`crate::memory_shape`).
pub(crate) use fill::FillCursor;

use crate::shape_size::{
    dense_counts, fanout_counts, flat_nodes, integer_key, mid_degree_counts, point_key, ring_nodes,
    target_bytes, text_key, INDEXED_STRING,
};

/// One synthetic graph. `Base` is empty. `Ring` is a directed cycle.
/// `Fanout` is hubs and leaves. `Flat` is nodes with no relationships.
/// `DenseRing` is a directed cycle where each node also skips ahead.
/// `MidDegree` is the degree-32–512 ladder: every node's out-degree sits
/// above the adjacency seal floor (`COMPACTION_DELTA_FLOOR` = 32 hops,
/// ADR-0551), so it is the one shape whose adjacency chunks actually seal
/// in the RSS rig — the degrees the sealed-delta tier was built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphShape {
    Base,
    Ring,
    Fanout,
    Flat,
    DenseRing,
    MidDegree,
}

/// `Small` is the `cargo test` pin. Every filled shape has 1,000 nodes.
/// `Large` is the closed-form census for a logical size. The RSS loadtest
/// does not stop at that count; it writes until the database hits the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShapeScale {
    Small,
    Large { logical_bytes: u64 },
}

/// How many attributes of each index type a node carries.
///
/// Text and point counts add that many properties on top of the integer
/// `attributes`. Range counts index the first integer properties and must
/// not exceed `attributes`. Zero means that index type is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct IndexCounts {
    pub text: u32,
    pub point: u32,
    pub range: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeCensus {
    pub nodes: u64,
    pub relationships: u64,
}

/// Rows per statement batch for every filled shape.
const BATCH: u32 = 2_000;

/// `(start, last)` inclusive id windows of at most [`BATCH`].
fn batch_ranges(total: u32) -> impl Iterator<Item = (u32, u32)> {
    let mut start = 0u32;
    std::iter::from_fn(move || {
        if start >= total {
            return None;
        }
        let end = (start + BATCH).min(total);
        let last = end - 1;
        let window = (start, last);
        start = end;
        Some(window)
    })
}
/// Integer properties on every node. The first is `id`. Default is one.
pub const DEFAULT_NODE_ATTRIBUTES: u32 = 1;

impl GraphShape {
    /// The one spelling table: `(CLI kebab-case, memory-shape report)`. Both
    /// the CLI spelling and the report spelling are derived from it, so a
    /// shape cannot be added to one and forgotten in the other.
    const fn spellings(self) -> (&'static str, &'static str) {
        match self {
            Self::Base => ("base", "base"),
            Self::Ring => ("ring", "ring"),
            Self::Fanout => ("fanout", "fanout"),
            Self::Flat => ("flat", "flat"),
            Self::DenseRing => ("dense-ring", "dense ring"),
            Self::MidDegree => ("mid-degree", "mid degree"),
        }
    }

    /// Memory-shape report spelling (`dense ring` with a space).
    pub fn as_str(self) -> &'static str {
        self.spellings().1
    }
}

impl std::fmt::Display for GraphShape {
    /// Kebab-case CLI spelling (`dense-ring`); the canonical pair of
    /// [`FromStr`](std::str::FromStr) and the form used in `shape-<name>`
    /// dataset names.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.spellings().0)
    }
}

impl std::str::FromStr for GraphShape {
    type Err = String;

    /// Parses the kebab-case CLI spellings [`Display`](std::fmt::Display)
    /// emits, minus `base` (not a loadable shape on the CLI). The accepted
    /// set is [`loadable`] and the names come from the one spelling table,
    /// so a shape cannot be added to one and forgotten here.
    fn from_str(raw: &str) -> Result<Self, String> {
        if let Some(shape) = loadable().find(|shape| shape.spellings().0 == raw) {
            return Ok(shape);
        }
        let accepted: Vec<&str> = loadable().map(|shape| shape.spellings().0).collect();
        Err(format!(
            "unknown shape {raw:?}; expected {}",
            accepted.join(", ")
        ))
    }
}

impl IndexCounts {
    pub const NONE: Self = Self {
        text: 0,
        point: 0,
        range: 0,
    };

    /// Compare key. `none` when every count is zero, otherwise
    /// `text2+point1+range1` with zero counts omitted, in that order.
    pub fn token(self) -> String {
        if self == Self::NONE {
            return "none".into();
        }
        let mut parts = Vec::new();
        if self.text > 0 {
            parts.push(format!("text{}", self.text));
        }
        if self.point > 0 {
            parts.push(format!("point{}", self.point));
        }
        if self.range > 0 {
            parts.push(format!("range{}", self.range));
        }
        parts.join("+")
    }
}

impl ShapeScale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Large { .. } => "large",
        }
    }
}

/// Every shape, in declaration order — the one inventory. The CLI parser's
/// accepted set, the RSS matrix and the spelling roundtrip test are
/// projections of it, so a new shape cannot be added to one and forgotten
/// in another.
const ALL: [GraphShape; 6] = [
    GraphShape::Base,
    GraphShape::Ring,
    GraphShape::Fanout,
    GraphShape::Flat,
    GraphShape::DenseRing,
    GraphShape::MidDegree,
];

/// The CLI-loadable shapes: [`ALL`] minus `base`. The one home of the
/// exclusion — the parser accepts exactly these and the spelling roundtrip
/// test pins exactly these.
fn loadable() -> impl Iterator<Item = GraphShape> {
    ALL.into_iter().filter(|shape| *shape != GraphShape::Base)
}

/// Shapes whose process RSS the loadtest records, in report order.
/// `base` is the empty server subtracted from the others. Index counts are
/// run-wide and are not part of this list.
pub fn rss_matrix() -> [GraphShape; 6] {
    [
        GraphShape::Base,
        GraphShape::Flat,
        GraphShape::Fanout,
        GraphShape::Ring,
        GraphShape::DenseRing,
        GraphShape::MidDegree,
    ]
}

/// Cypher that builds `shape` at `scale` after optional index DDL.
/// Statements are independent autocommit queries. Writes return nothing.
/// `base` emits nothing, including no index DDL, so its RSS stays the floor.
pub fn statements(
    shape: GraphShape,
    scale: ShapeScale,
    indexes: IndexCounts,
    attributes: u32,
) -> Result<Vec<String>, String> {
    validate(attributes, indexes, scale)?;
    let mut out = index_statements(shape, indexes);
    match shape {
        GraphShape::Base => {}
        GraphShape::Ring => out.extend(ring_statements(
            ring_nodes(scale, attributes, indexes)?,
            attributes,
            indexes,
        )),
        GraphShape::Fanout => {
            let (hubs, leaves) = fanout_counts(scale, attributes, indexes)?;
            out.extend(fanout_statements(hubs, leaves, attributes, indexes));
        }
        GraphShape::Flat => out.extend(flat_statements(
            flat_nodes(scale, attributes, indexes)?,
            attributes,
            indexes,
        )),
        GraphShape::DenseRing => {
            let (nodes, degree) = dense_counts(scale, attributes, indexes)?;
            out.extend(dense_statements(nodes, degree, attributes, indexes));
        }
        GraphShape::MidDegree => {
            let nodes = mid_degree_counts(scale, attributes, indexes)?;
            out.extend(middegree_statements(nodes, attributes, indexes));
        }
    }
    Ok(out)
}

pub(crate) fn validate(
    attributes: u32,
    indexes: IndexCounts,
    scale: ShapeScale,
) -> Result<(), String> {
    if attributes == 0 {
        return Err("attributes per node must be at least 1".into());
    }
    if indexes.range > attributes {
        return Err(format!(
            "range indexes ({}) exceed attributes per node ({})",
            indexes.range, attributes
        ));
    }
    if let ShapeScale::Large { logical_bytes } = scale {
        target_bytes(logical_bytes)?;
    }
    Ok(())
}

/// Default loadtest target: 32 MiB of logical graph memory per filled shape.
pub const DEFAULT_LOGICAL_TARGET_BYTES: u64 = 32 * 1024 * 1024;

fn labels(shape: GraphShape) -> &'static [&'static str] {
    match shape {
        GraphShape::Base => &[],
        GraphShape::Flat => &["Flat"],
        GraphShape::Ring => &["Ring"],
        GraphShape::DenseRing => &["DenseRing"],
        GraphShape::Fanout => &["Hub", "Leaf"],
        GraphShape::MidDegree => &["MidDegree"],
    }
}

type IndexDdl = (u32, &'static str, fn(u32) -> String);

fn index_statements(shape: GraphShape, indexes: IndexCounts) -> Vec<String> {
    let kinds: [IndexDdl; 3] = [
        (indexes.text, "TEXT", text_key),
        (indexes.point, "POINT", point_key),
        (indexes.range, "RANGE", integer_key),
    ];
    let mut out = Vec::new();
    for label in labels(shape) {
        for (count, keyword, key_of) in kinds {
            for i in 0..count {
                let key = key_of(i);
                out.push(format!(
                    "CREATE {keyword} INDEX {label}_{key} FOR (n:{label}) ON (n.{key})"
                ));
            }
        }
    }
    out
}

/// `id` plus `a1`..`a{attributes-1}`, then the indexed text and point values.
fn node_props(value: &str, attributes: u32, indexes: IndexCounts) -> String {
    let mut parts = Vec::new();
    for n in 0..attributes {
        let key = integer_key(n);
        parts.push(format!("{key}: {value}"));
    }
    for i in 0..indexes.text {
        parts.push(format!("{key}: '{INDEXED_STRING}'", key = text_key(i)));
    }
    for i in 0..indexes.point {
        parts.push(format!(
            "{key}: point({{srid: 7203, x: 1.0, y: 2.0}})",
            key = point_key(i)
        ));
    }
    parts.join(", ")
}

fn ring_statements(nodes: u32, attributes: u32, indexes: IndexCounts) -> Vec<String> {
    let props = node_props("i", attributes, indexes);
    let mut out = Vec::new();
    for (start, last) in batch_ranges(nodes) {
        out.push(format!(
            "UNWIND range({start}, {last}) AS i CREATE (:Ring {{{props}}})"
        ));
        if last > start {
            out.push(format!(
                "MATCH (c:Ring) WHERE c.id >= {start} AND c.id <= {last} \
                 WITH c ORDER BY c.id \
                 WITH collect(c) AS nodes \
                 UNWIND range(0, size(nodes) - 2) AS i \
                 WITH nodes[i] AS a, nodes[i + 1] AS b \
                 CREATE (a)-[:NEXT]->(b)"
            ));
        }
        if start > 0 {
            let prev = start - 1;
            out.push(format!(
                "MATCH (a:Ring {{id: {prev}}}), (b:Ring {{id: {start}}}) \
                 CREATE (a)-[:NEXT]->(b)"
            ));
        }
    }
    if nodes >= 2 {
        let last = nodes - 1;
        out.push(format!(
            "MATCH (a:Ring {{id: {last}}}), (b:Ring {{id: 0}}) CREATE (a)-[:NEXT]->(b)"
        ));
    }
    out
}

fn fanout_statements(hubs: u32, leaves: u32, attributes: u32, indexes: IndexCounts) -> Vec<String> {
    let hub_props = node_props("h", attributes, indexes);
    let mut out = Vec::new();
    for (start, last) in batch_ranges(hubs) {
        out.push(format!(
            "UNWIND range({start}, {last}) AS h CREATE (:Hub {{{hub_props}}})"
        ));
    }
    // One hub per statement. Crossing every hub with one leaf window made
    // each statement `hubs × batch` rows, which the other shapes never do.
    for h in 0..hubs {
        let leaf_props = node_props(&format!("{h} * {leaves} + l"), attributes, indexes);
        for (start, last) in batch_ranges(leaves) {
            out.push(format!(
                "MATCH (hub:Hub {{id: {h}}}) \
                 UNWIND range({start}, {last}) AS l \
                 CREATE (hub)-[:LEAF]->(:Leaf {{{leaf_props}}})"
            ));
        }
    }
    out
}

fn flat_statements(nodes: u32, attributes: u32, indexes: IndexCounts) -> Vec<String> {
    let props = node_props("i", attributes, indexes);
    batch_ranges(nodes)
        .map(|(start, last)| {
            format!("UNWIND range({start}, {last}) AS i CREATE (:Flat {{{props}}})")
        })
        .collect()
}

fn dense_statements(nodes: u32, degree: u32, attributes: u32, indexes: IndexCounts) -> Vec<String> {
    let props = node_props("i", attributes, indexes);
    let mut out: Vec<String> = batch_ranges(nodes)
        .map(|(start, last)| {
            format!("UNWIND range({start}, {last}) AS i CREATE (:DenseRing {{{props}}})")
        })
        .collect();
    // Source windows of `BATCH / degree` keep each statement at or under
    // BATCH relationship rows, the same cap as a ring batch.
    let width = if degree == 0 {
        nodes
    } else {
        (BATCH / degree).max(1)
    };
    let mut start = 0u32;
    while start < nodes {
        let last = (start + width).min(nodes) - 1;
        out.push(format!(
            "UNWIND range({start}, {last}) AS i \
             UNWIND range(1, {degree}) AS d \
             WITH i, (i + d) % {nodes} AS j \
             MATCH (a:DenseRing {{id: i}}), (b:DenseRing {{id: j}}) \
             CREATE (a)-[:LINK]->(b)"
        ));
        start = last + 1;
    }
    out
}

/// The mid-degree ladder: the out-degree of mid-degree node `i` is
/// [`MID_DEGREE_LADDER`]`[i % MID_DEGREE_GROUP]`. Every rung is a power of
/// two, so the logical model's `reserved()` capacity charge is exact — no
/// padding term — and every rung is at least the adjacency seal floor
/// ([`MID_DEGREE_FLOOR`], 32 hops, ADR-0551), so a mid-degree out-chunk
/// always seals at a checkpoint. The pattern repeats per group instead of
/// concentrating the hubs in one global head: the loadtest fills to a
/// memory limit, not a known node count, and a head-only ladder would make
/// the measured degree mix depend on how far the fill got.
const MID_DEGREE_LADDER: [u32; 16] = [
    512, 256, 128, 128, 64, 64, 64, 64, 32, 32, 32, 32, 32, 32, 32, 32,
];

/// Every fact derived from the ladder is computed from the table here —
/// its only home. A rung edit moves the group size, the average, the floor
/// and every hop total with it.
const fn ladder_sum(ladder: &[u32]) -> u32 {
    let mut sum = 0u32;
    let mut i = 0;
    while i < ladder.len() {
        sum += ladder[i];
        i += 1;
    }
    sum
}

const fn ladder_min(ladder: &[u32]) -> u32 {
    let mut min = ladder[0];
    let mut i = 1;
    while i < ladder.len() {
        if ladder[i] < min {
            min = ladder[i];
        }
        i += 1;
    }
    min
}

/// One ladder group's node count.
pub(crate) const MID_DEGREE_GROUP: u32 = MID_DEGREE_LADDER.len() as u32;
/// One whole group's hop count — every group's relationship rows stay under
/// the 2,000-row batch cap.
const MID_DEGREE_GROUP_HOPS: u32 = ladder_sum(&MID_DEGREE_LADDER);
/// The exact average out-degree: whole groups carry
/// [`MID_DEGREE_GROUP_HOPS`] over [`MID_DEGREE_GROUP`] nodes.
pub(crate) const MID_DEGREE_AVG: u64 = MID_DEGREE_GROUP_HOPS as u64 / MID_DEGREE_GROUP as u64;
/// The ladder's floor rung — the seal floor the shape exists to cross.
const MID_DEGREE_FLOOR: u32 = ladder_min(&MID_DEGREE_LADDER);

/// The mid-degree relationships over `nodes` nodes: whole groups at
/// [`MID_DEGREE_GROUP_HOPS`] plus the head slice of a trailing partial
/// group — the one derivation the census, the fill tick and the test
/// comments all share.
pub(crate) fn mid_degree_hops(nodes: u32) -> u64 {
    let whole = u64::from(nodes / MID_DEGREE_GROUP) * u64::from(MID_DEGREE_GROUP_HOPS);
    let head: u64 = MID_DEGREE_LADDER[..(nodes % MID_DEGREE_GROUP) as usize]
        .iter()
        .map(|d| u64::from(*d))
        .sum();
    whole + head
}

fn middegree_statements(nodes: u32, attributes: u32, indexes: IndexCounts) -> Vec<String> {
    // Dense's two-phase pin: every node exists before any link runs, so a
    // group's forward references — `(i + d) % nodes` for d up to 512 —
    // always MATCH. One create batch (dense's own window shape), then one
    // link statement per ladder group.
    let props = node_props("i", attributes, indexes);
    let mut out: Vec<String> = batch_ranges(nodes)
        .map(|(start, last)| {
            format!("UNWIND range({start}, {last}) AS i CREATE (:MidDegree {{{props}}})")
        })
        .collect();
    out.extend(
        (0..nodes)
            .step_by(MID_DEGREE_LADDER.len())
            .map(|start| middegree_group_link(start, nodes)),
    );
    out
}

/// Link one ladder group: every node at its [`MID_DEGREE_LADDER`] degree,
/// against the caller's modulus total (the pin's full node count, the
/// fill's nodes committed so far — dense's own pin/fill split; the fill's
/// early groups wrap onto themselves, their rung multiples of the group
/// size becoming self-loops). The period, the arms and the ELSE floor all
/// come from the table — nothing about the ladder is re-typed here.
fn middegree_group_link(start: u32, total: u32) -> String {
    let last = (start + (MID_DEGREE_GROUP - 1)).min(total - 1);
    let arms: String = MID_DEGREE_LADDER
        .iter()
        .enumerate()
        .filter(|(_, degree)| **degree > MID_DEGREE_FLOOR)
        .map(|(i, degree)| format!("WHEN {i} THEN {degree}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "UNWIND range({start}, {last}) AS i \
         UNWIND range(1, CASE i % {MID_DEGREE_GROUP} {arms} ELSE {MID_DEGREE_FLOOR} END) AS d \
         WITH i, (i + d) % {total} AS j \
         MATCH (a:MidDegree {{id: i}}), (b:MidDegree {{id: j}}) \
         CREATE (a)-[:LINK]->(b)"
    )
}

#[cfg(test)]
mod tests {
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
}
