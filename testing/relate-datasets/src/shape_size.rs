//! Closed-form logical-byte sizing for the synthetic shapes: the node,
//! relationship, and index charge constants and the per-shape node-count
//! solvers behind [`crate::shapes::census`].
//!
//! [`crate::shapes`] keeps the shape types and the statement generators;
//! this module owns the arithmetic that turns a logical-byte target into a
//! node count, so a given `(shape, scale, indexes, attributes)` always
//! charges the same logical bytes as the statements build.

use crate::shapes::{
    mid_degree_hops, validate, GraphShape, IndexCounts, ShapeCensus, ShapeScale, MID_DEGREE_AVG,
    MID_DEGREE_GROUP,
};

/// Nodes in every filled `Small` pin. `Base` stays empty.
pub(crate) const SMALL_NODES: u32 = 1_000;

pub(crate) const INDEXED_STRING: &str = "abcdefghijklmnop";
/// `4 + logical_bytes` for the 16-character indexed string (header 16 + length).
const STRING_RECORD: u64 = 36;
/// `4 + logical_bytes` for a point value (32).
const POINT_RECORD: u64 = 36;
/// `size_of::<Value>()`. The property-value vector charges this, not the
/// payload inside the value. `Node`, `Relationship`, and `Path` sit behind
/// `Arc`, so the slot is 48 rather than the 80 of the inline payloads.
const VALUE_SLOT: u64 = 48;
/// `size_of` of a node version record (`xmin`, `xmax`, live node).
const NODE_STRIDE: u64 = 88;
/// `size_of` of a relationship version record. The record holds a `Value`,
/// so it shrank with that enum when entity payloads moved behind `Arc`.
const REL_STRIDE: u64 = 144;
/// `size_of::<AdjHop>()`.
const HOP_STRIDE: u64 = 24;
/// `size_of<(u32, Vec<AdjHop>)>()`. The inner hop-vector header is inside it.
const CHUNK_STRIDE: u64 = 32;
const MAP_ENTRY: u64 = 24;
const VEC_HEADER: u64 = 24;

/// Property-key spelling for the `i`-th indexed text attribute.
pub(crate) fn text_key(i: u32) -> String {
    format!("s{i}")
}

/// Property-key spelling for the `i`-th indexed point attribute.
pub(crate) fn point_key(i: u32) -> String {
    format!("loc{i}")
}

/// Property-key spelling for the `i`-th integer attribute (`id` first).
pub(crate) fn integer_key(i: u32) -> String {
    if i == 0 {
        "id".into()
    } else {
        format!("a{i}")
    }
}

pub fn census(
    shape: GraphShape,
    scale: ShapeScale,
    indexes: IndexCounts,
    attributes: u32,
) -> Result<ShapeCensus, String> {
    validate(attributes, indexes, scale)?;
    Ok(match shape {
        GraphShape::Base => ShapeCensus {
            nodes: 0,
            relationships: 0,
        },
        GraphShape::Ring => {
            let nodes = ring_nodes(scale, attributes, indexes)?;
            ShapeCensus {
                nodes: u64::from(nodes),
                relationships: u64::from(nodes),
            }
        }
        GraphShape::Fanout => {
            let (hubs, leaves) = fanout_counts(scale, attributes, indexes)?;
            let leaf_nodes = u64::from(hubs) * u64::from(leaves);
            ShapeCensus {
                nodes: u64::from(hubs) + leaf_nodes,
                relationships: leaf_nodes,
            }
        }
        GraphShape::Flat => ShapeCensus {
            nodes: u64::from(flat_nodes(scale, attributes, indexes)?),
            relationships: 0,
        },
        GraphShape::DenseRing => {
            let (nodes, degree) = dense_counts(scale, attributes, indexes)?;
            ShapeCensus {
                nodes: u64::from(nodes),
                relationships: u64::from(nodes) * u64::from(degree),
            }
        }
        GraphShape::MidDegree => {
            let nodes = mid_degree_counts(scale, attributes, indexes)?;
            ShapeCensus {
                nodes: u64::from(nodes),
                relationships: mid_degree_hops(nodes),
            }
        }
    })
}

/// `target + per/2` divided by `per`, so the closed-form charge lands nearest
/// the requested logical size.
fn nearest(target: u64, per: u64) -> u64 {
    (target.saturating_add(per / 2)) / per
}

fn fit_u32(count: u64, what: &str) -> Result<u32, String> {
    u32::try_from(count).map_err(|_| format!("{what} count {count} does not fit in u32"))
}

pub(crate) fn target_bytes(logical_bytes: u64) -> Result<u64, String> {
    if logical_bytes == 0 {
        Err("logical target must be greater than 0".into())
    } else {
        Ok(logical_bytes)
    }
}

fn reserved(len: u64) -> u64 {
    if len == 0 {
        0
    } else {
        len.next_power_of_two()
    }
}

fn vec_heap(len: u64, elem: u64) -> u64 {
    reserved(len) * elem
}

/// Dashmap slot, version-list header, and one version record.
fn version_chain(stride: u64) -> u64 {
    MAP_ENTRY + VEC_HEADER + stride
}

fn prop_heap(count: u32) -> u64 {
    let n = u64::from(count);
    vec_heap(n, 4) + vec_heap(n, VALUE_SLOT)
}

/// Payload of one labeled node with `attributes` integer properties and no
/// index charge. Label length is the posting key. Each integer property is
/// 12 payload bytes. Containers are added in [`node_bytes`].
fn integer_payload(label_len: u64, attributes: u32) -> u64 {
    68 + label_len + 12 * u64::from(attributes)
}

/// Property bytes plus TEXT, POINT, and RANGE index charge for one label.
fn index_extra(label_len: u64, indexes: IndexCounts) -> u64 {
    let mut extra = 0u64;
    for i in 0..indexes.text {
        let key_len = text_key(i).len() as u64;
        extra += STRING_RECORD + text_index_bytes(label_len, key_len, INDEXED_STRING.len() as u64);
    }
    for i in 0..indexes.point {
        let key_len = point_key(i).len() as u64;
        extra += POINT_RECORD + point_index_bytes(label_len, key_len);
    }
    for i in 0..indexes.range {
        let key_len = integer_key(i).len() as u64;
        extra += range_index_bytes(label_len, key_len);
    }
    extra
}

fn node_bytes(label_len: u64, attributes: u32, indexes: IndexCounts) -> u64 {
    let props = attributes + indexes.text + indexes.point;
    integer_payload(label_len, attributes)
        + index_extra(label_len, indexes)
        + vec_heap(1, 4)
        + prop_heap(props)
        + version_chain(NODE_STRIDE)
}

/// One relationship with no properties, including its version chain.
/// Adjacency is [`endpoint_adj`], charged on the endpoint, not here.
fn rel_record_bytes() -> u64 {
    44 + version_chain(REL_STRIDE)
}

/// One typed adjacency entry's fixed frame — the map slot, the version-list
/// header, and the chunk — stated once so every hop-charge spelling shares
/// it.
fn endpoint_adj_frame() -> u64 {
    MAP_ENTRY + VEC_HEADER + vec_heap(1, CHUNK_STRIDE)
}

/// One typed adjacency entry of `degree` hops.
fn endpoint_adj(degree: u64) -> u64 {
    if degree == 0 {
        0
    } else {
        endpoint_adj_frame() + vec_heap(degree, HOP_STRIDE)
    }
}

pub(crate) fn ring_nodes(
    scale: ShapeScale,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<u32, String> {
    match scale {
        ShapeScale::Small => Ok(SMALL_NODES),
        // `:Ring` is 4 bytes. One NEXT per node, including the wrap to id 0.
        ShapeScale::Large { logical_bytes } => {
            let per = node_bytes(4, attributes, indexes)
                + rel_record_bytes()
                + endpoint_adj(1)
                + endpoint_adj(1);
            let n = nearest(target_bytes(logical_bytes)?, per).max(2);
            fit_u32(n, "ring nodes")
        }
    }
}

pub(crate) fn fanout_counts(
    scale: ShapeScale,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<(u32, u32), String> {
    match scale {
        // 4 hubs, and 249 leaves each: 4 + 4×249 = 1,000 nodes, 996 links.
        ShapeScale::Small => Ok((4, (SMALL_NODES - 4) / 4)),
        // 16 hubs plus equal leaves. `:Hub` is 3 bytes, `:Leaf` is 4.
        ShapeScale::Large { logical_bytes } => {
            const HUBS: u64 = 16;
            let hub = node_bytes(3, attributes, indexes);
            let leaf_and_rel =
                node_bytes(4, attributes, indexes) + rel_record_bytes() + endpoint_adj(1);
            let target = target_bytes(logical_bytes)?;
            let mut leaves_per = 1u64;
            let mut best = u64::MAX;
            let upper = (target / leaf_and_rel).saturating_add(HUBS).max(1);
            for n in 1..=upper {
                let total = HUBS * (hub + endpoint_adj(n)) + HUBS * n * leaf_and_rel;
                let dist = total.abs_diff(target);
                if dist < best {
                    best = dist;
                    leaves_per = n;
                }
            }
            Ok((
                fit_u32(HUBS, "fanout hubs")?,
                fit_u32(leaves_per, "fanout leaves")?,
            ))
        }
    }
}

pub(crate) fn flat_nodes(
    scale: ShapeScale,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<u32, String> {
    match scale {
        ShapeScale::Small => Ok(SMALL_NODES),
        // `:Flat` is 4 bytes.
        ShapeScale::Large { logical_bytes } => fit_u32(
            nearest(
                target_bytes(logical_bytes)?,
                node_bytes(4, attributes, indexes),
            )
            .max(1),
            "flat nodes",
        ),
    }
}

pub(crate) fn dense_counts(
    scale: ShapeScale,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<(u32, u32), String> {
    match scale {
        // Degree stays 8, so 1,000 nodes have 8,000 links.
        ShapeScale::Small => Ok((SMALL_NODES, 8)),
        // `:DenseRing` is 9 bytes. Degree stays 20.
        ShapeScale::Large { logical_bytes } => {
            const DEGREE: u64 = 20;
            let per = node_bytes(9, attributes, indexes)
                + DEGREE * rel_record_bytes()
                + endpoint_adj(DEGREE)
                + endpoint_adj(DEGREE);
            let nodes = nearest(target_bytes(logical_bytes)?, per).max(1);
            Ok((
                fit_u32(nodes, "dense ring nodes")?,
                fit_u32(DEGREE, "dense ring degree")?,
            ))
        }
    }
}

/// One mid-degree *out* endpoint's adjacency charge. Every ladder rung is a
/// power of two, so `reserved(rung) == rung` and the average hop charge is
/// exactly [`MID_DEGREE_AVG`] hops — no capacity padding term.
fn mid_degree_out_adj() -> u64 {
    endpoint_adj_frame() + MID_DEGREE_AVG * HOP_STRIDE
}

/// One mid-degree *in* endpoint's adjacency charge. The in-degree is uniform
/// and exact — each rung divides the group, so every node receives exactly
/// [`MID_DEGREE_AVG`] hops — but 96 is not a power of two, so that chunk
/// reserves the next one and pays the ordinary [`endpoint_adj`] padding. The
/// out side is the one that escapes `reserved()`.
fn mid_degree_in_adj() -> u64 {
    endpoint_adj(MID_DEGREE_AVG)
}

pub(crate) fn mid_degree_counts(
    scale: ShapeScale,
    attributes: u32,
    indexes: IndexCounts,
) -> Result<u32, String> {
    match scale {
        // 1,000 nodes like every filled pin; the trailing partial group
        // carries the ladder's head, so the pin's relationship count is
        // whatever `mid_degree_hops` sums from the ladder — never a
        // hand-typed constant.
        ShapeScale::Small => Ok(SMALL_NODES),
        // `:MidDegree` is 9 bytes. Whole groups only: the group unit keeps
        // the average-degree charge exact at any size.
        ShapeScale::Large { logical_bytes } => {
            let group = u64::from(MID_DEGREE_GROUP);
            let per = node_bytes(9, attributes, indexes)
                + MID_DEGREE_AVG * rel_record_bytes()
                + mid_degree_out_adj()
                + mid_degree_in_adj();
            let groups = nearest(target_bytes(logical_bytes)?, group * per).max(1);
            fit_u32(group * groups, "mid degree nodes")
        }
    }
}

/// TEXT grams for a string of `chars` on a label/key of those lengths.
/// Widths are 1..=3. This is the live `memory_usage` term.
fn text_index_bytes(label_len: u64, key_len: u64, chars: u64) -> u64 {
    let positions: u64 = (1..=3).filter(|w| chars >= *w).map(|w| chars - w + 1).sum();
    let per = 24 + label_len + key_len + 3 + 8;
    positions * per
}

fn point_index_bytes(label_len: u64, key_len: u64) -> u64 {
    24 + label_len + key_len + 24 + 8
}

/// One range posting of an integer property. The group key is 24 bytes.
fn range_index_bytes(label_len: u64, key_len: u64) -> u64 {
    24 + label_len + key_len + 24 + 8
}
