//! POINT index: an R\*-tree (rstar) over the posted points. The postings
//! are a superset — the residual `point.distance` / `point.withinBBox`
//! re-check owns z, SRID, wrap, and bound semantics.

use std::collections::{HashMap, HashSet};

use relate_core::{NodeId, Value};
use relate_model::point::is_geographic;
use rstar::{RTree, RTreeObject, AABB};

use super::order::SpatialSeek;
use super::DrainState;
use crate::spatial::{bbox_longitude_wraps, EARTH_RADIUS_METERS};

/// The stored geographic longitude domain, as `point()` validates it.
const LON_MIN: f64 = -180.0;
const LON_MAX: f64 = 180.0;

#[derive(Debug)]
pub(crate) struct PointIndex {
    /// Every posted point by node: removal needs the coordinates to unpost
    /// from the tree, the seal reads them, and the residual's non-point
    /// arm falls back to the full posting set.
    pts: HashMap<NodeId, (i32, f64, f64)>,
    tree: RTree<PointLeaf>,
    dirty: HashSet<NodeId>,
    /// True until the pair's seal ([`DrainState::seal`]). Bulk insert
    /// records into `pts` only; after seal, each edit maintains the tree
    /// incrementally.
    bulk: bool,
    /// Sticky: a posted point carried a negative height. Heights enter the
    /// residual's distance with a +height radial multiplier, so a negative
    /// height can shrink the great-circle term — no bounded envelope is
    /// sound then, and distance seeks fall back to the full posting set.
    has_negative_height: bool,
}

impl Default for PointIndex {
    fn default() -> Self {
        Self {
            pts: HashMap::new(),
            tree: RTree::new(),
            dirty: HashSet::new(),
            bulk: true,
            has_negative_height: false,
        }
    }
}

/// One posted point: the tree payload (the position) plus the node id it
/// posts for. `PartialEq` keys rstar's `remove` on the exact (position, id)
/// pair — two nodes sharing coordinates stay distinct leaves.
#[derive(Clone, Debug, PartialEq)]
struct PointLeaf {
    pos: [f64; 2],
    id: NodeId,
}

impl RTreeObject for PointLeaf {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_point(self.pos)
    }
}

/// The one leaf constructor. A non-finite coordinate has no meaningful
/// envelope (NaN comparisons would corrupt the tree's invariants), and such
/// a point can never satisfy a spatial predicate — distance to it is NaN
/// and bbox containment is false — so it posts to `pts` (stays removable)
/// but never to the tree.
fn leaf_of(id: NodeId, x: f64, y: f64) -> Option<PointLeaf> {
    (x.is_finite() && y.is_finite()).then_some(PointLeaf { pos: [x, y], id })
}

impl DrainState for PointIndex {
    fn dirty(&self) -> &HashSet<NodeId> {
        &self.dirty
    }
    fn dirty_mut(&mut self) -> &mut HashSet<NodeId> {
        &mut self.dirty
    }
    /// Bulk-load the tree from the posted points and leave bulk mode —
    /// the lazy string postings' seal precedent (ADR-0063): a packed tree
    /// is both faster to build and better shaped than one grown by
    /// sequential inserts. Idempotent.
    fn seal(&mut self) {
        let leaves: Vec<PointLeaf> = self
            .pts
            .iter()
            .filter_map(|(id, (_, x, y))| leaf_of(*id, *x, *y))
            .collect();
        self.tree = RTree::bulk_load(leaves);
        self.bulk = false;
    }
    fn insert(&mut self, id: NodeId, value: &Value) {
        if let Value::Point(p) = value {
            point_insert(self, id, p.srid, p.x, p.y, p.z);
        }
    }
    fn remove_node(&mut self, id: NodeId) {
        point_remove_node(self, id);
    }
}

fn point_remove_node(st: &mut PointIndex, id: NodeId) {
    if let Some((_, x, y)) = st.pts.remove(&id) {
        // Bulk mode's tree is not yet built — `pts` is the whole truth
        // until the seal.
        if !st.bulk {
            if let Some(leaf) = leaf_of(id, x, y) {
                st.tree.remove(&leaf);
            }
        }
    }
}

pub(crate) fn point_insert(
    st: &mut PointIndex,
    id: NodeId,
    srid: i32,
    x: f64,
    y: f64,
    z: Option<f64>,
) {
    point_remove_node(st, id);
    st.pts.insert(id, (srid, x, y));
    if z.is_some_and(|z| z < 0.0) {
        st.has_negative_height = true;
    }
    if !st.bulk {
        if let Some(leaf) = leaf_of(id, x, y) {
            st.tree.insert(leaf);
        }
    }
}

pub(crate) fn spatial_ids(st: &PointIndex, seek: &SpatialSeek) -> HashSet<NodeId> {
    // One query site: the seek's envelopes (one or two — a geographic box
    // can wrap the antimeridian, which one AABB cannot express). An empty
    // set — a non-Point operand, or heights that make no bounded envelope
    // sound — falls back to the full posting set; the residual re-check
    // filters it.
    let envelopes = seek_envelopes(st, seek);
    if envelopes.is_empty() {
        return st.pts.keys().copied().collect();
    }
    let mut out = HashSet::new();
    for envelope in &envelopes {
        out.extend(
            st.tree
                .locate_in_envelope_intersecting(envelope)
                .map(|leaf| leaf.id),
        );
    }
    out
}

pub(crate) fn spatial_holds(v: &Value, seek: &SpatialSeek) -> bool {
    match seek {
        SpatialSeek::BBox { lo, hi } => {
            match crate::spatial::point_within_bbox(&[v.clone(), lo.clone(), hi.clone()]) {
                Ok(Value::Bool(b)) => b,
                Ok(_) => false,
                Err(_) => true,
            }
        }
        SpatialSeek::Distance {
            origin,
            max,
            inclusive,
        } => match crate::spatial::point_distance(&[v.clone(), origin.clone()]) {
            Ok(Value::Float(d)) => {
                if *inclusive {
                    d <= *max
                } else {
                    d < *max
                }
            }
            Ok(_) => false,
            Err(_) => true,
        },
    }
}

/// Longitude envelopes covering the `[lo, hi]` band on the stored
/// `[LON_MIN, LON_MAX]` domain: a band crossing the domain edge splits in
/// two, a band wider than the domain is the full range.
fn lon_envelopes(lo: f64, hi: f64, miny: f64, maxy: f64) -> Vec<AABB<[f64; 2]>> {
    if hi - lo >= LON_MAX - LON_MIN {
        return vec![AABB::from_corners([LON_MIN, miny], [LON_MAX, maxy])];
    }
    if lo >= LON_MIN && hi <= LON_MAX {
        return vec![AABB::from_corners([lo, miny], [hi, maxy])];
    }
    if hi > LON_MAX {
        return vec![
            AABB::from_corners([lo, miny], [LON_MAX, maxy]),
            AABB::from_corners([LON_MIN, miny], [hi - (LON_MAX - LON_MIN), maxy]),
        ];
    }
    vec![
        AABB::from_corners([LON_MIN, miny], [hi, maxy]),
        AABB::from_corners([lo + (LON_MAX - LON_MIN), miny], [LON_MAX, maxy]),
    ]
}

/// The candidate envelopes for one seek. A geographic distance seek uses
/// the spherical disk's exact great-circle bounding box — the residual's
/// own surface metric (heights only add distance when they are
/// non-negative; a negative height anywhere makes no bounded envelope
/// sound, and the seek falls back to the full posting set). A Cartesian
/// seek uses the flat metre box, the Pythagorean metric's exact bound. A
/// box wrapping the antimeridian splits across ±180 ([`LON_MIN`],
/// [`LON_MAX`] — the domain `point()` validates). `EPS` pads the radius
/// for float rounding in the over-size direction; the residual keeps the
/// bound exact. Empty means no usable envelope, and the caller falls back
/// to the full posting set.
fn seek_envelopes(st: &PointIndex, seek: &SpatialSeek) -> Vec<AABB<[f64; 2]>> {
    const EPS: f64 = 1.01;
    match seek {
        SpatialSeek::BBox { lo, hi } => {
            let (Value::Point(lo), Value::Point(hi)) = (lo, hi) else {
                return Vec::new();
            };
            let (miny, maxy) = if lo.y <= hi.y {
                (lo.y, hi.y)
            } else {
                (hi.y, lo.y)
            };
            // The wrap decision is the residual predicate's own
            // ([`bbox_longitude_wraps`]): a wrapping geographic box is the
            // ±180 band, split in two; a Cartesian box is the raw interval
            // (any units); a non-wrapping inverted axis admits nothing,
            // and the empty return falls back to the full posting set.
            if bbox_longitude_wraps(lo.srid, lo.x, hi.x) {
                let mut out = vec![AABB::from_corners([lo.x, miny], [LON_MAX, maxy])];
                out.push(AABB::from_corners([LON_MIN, miny], [hi.x, maxy]));
                return out;
            }
            if !is_geographic(lo.srid) {
                return vec![AABB::from_corners([lo.x, miny], [hi.x, maxy])];
            }
            lon_envelopes(lo.x, hi.x, miny, maxy)
        }
        SpatialSeek::Distance {
            origin,
            max,
            inclusive: _,
        } => {
            let Value::Point(origin) = origin else {
                return Vec::new();
            };
            if !is_geographic(origin.srid) {
                let (dx, dy) = (max.abs() * EPS, max.abs() * EPS);
                return vec![AABB::from_corners(
                    [origin.x - dx, origin.y - dy],
                    [origin.x + dx, origin.y + dy],
                )];
            }
            // Heights: the radial multiplier is `R + avg_height`, so a
            // negative height anywhere (posted point or origin) can shrink
            // the great-circle term below the surface bound.
            if st.has_negative_height || origin.z.is_some_and(|z| z < 0.0) {
                return Vec::new();
            }
            // The disk's angular radius, padded and clamped at a
            // hemisphere — at or past it the box is the whole globe.
            let delta = (max.abs() / EARTH_RADIUS_METERS * EPS).min(std::f64::consts::FRAC_PI_2);
            let lat = origin.y.to_radians();
            let miny = (lat - delta).to_degrees().max(-90.0);
            let maxy = (lat + delta).to_degrees().min(90.0);
            // The disk's maximal longitude deviation (Jan Matuschek's
            // bounding-box derivation): past the pole-reaching radius the
            // deviation is every longitude.
            let lon_delta = if delta.sin() >= lat.cos().abs() {
                180.0
            } else {
                (delta.sin() / lat.cos().abs()).asin().to_degrees()
            };
            lon_envelopes(origin.x - lon_delta, origin.x + lon_delta, miny, maxy)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_test_support::values::point;

    fn seek_bbox(minx: f64, miny: f64, maxx: f64, maxy: f64) -> SpatialSeek {
        SpatialSeek::BBox {
            lo: point(7203, minx, miny),
            hi: point(7203, maxx, maxy),
        }
    }

    fn seek_distance(x: f64, y: f64, max: f64) -> SpatialSeek {
        SpatialSeek::Distance {
            origin: point(7203, x, y),
            max,
            inclusive: true,
        }
    }

    /// Every candidate is currently posted, and every posted point that
    /// satisfies the residual predicate is a candidate: the superset
    /// contract the seek path relies on.
    fn assert_superset(st: &PointIndex, seek: &SpatialSeek) {
        let candidates = spatial_ids(st, seek);
        for id in &candidates {
            assert!(st.pts.contains_key(id), "ghost candidate {id}");
        }
        for (id, (srid, x, y)) in &st.pts {
            if spatial_holds(&point(*srid, *x, *y), seek) {
                assert!(candidates.contains(id), "missed posted match {id}");
            }
        }
    }

    #[test]
    fn bulk_mode_appends_then_the_seal_bulk_loads() {
        let mut st = PointIndex::default();
        for i in 0..2_000 {
            point_insert(
                &mut st,
                i,
                4326,
                (i % 50) as f64 * 0.01,
                (i / 50) as f64 * 0.01,
                None,
            );
        }
        // Bulk mode left the tree unbuilt; the seal is the one build step.
        assert_eq!(st.tree.size(), 0);
        st.seal();
        assert_eq!(st.tree.size(), 2_000);
        assert_superset(&st, &seek_bbox(0.05, 0.05, 0.25, 0.25));
        // A sealed tree keeps taking incremental inserts and removals.
        point_insert(&mut st, 9_999, 4326, 0.10, 0.10, None);
        assert!(spatial_ids(&st, &seek_bbox(0.05, 0.05, 0.25, 0.25)).contains(&9_999));
        point_remove_node(&mut st, 9_999);
        assert!(!spatial_ids(&st, &seek_bbox(0.05, 0.05, 0.25, 0.25)).contains(&9_999));
        // The seal is idempotent.
        st.seal();
        assert_superset(&st, &seek_bbox(0.05, 0.05, 0.25, 0.25));
    }

    #[test]
    fn splits_and_underflow_keep_the_superset() {
        // 10k points in a space far below one degree — the tree splits
        // many levels deep; removing 90% exercises underflow merges.
        let mut st = PointIndex::default();
        for i in 0..10_000 {
            let (x, y) = ((i % 100) as f64 / 10_000.0, (i / 100) as f64 / 10_000.0);
            point_insert(&mut st, i, 7203, x, y, None);
        }
        st.seal();
        let seek = seek_bbox(0.002, 0.002, 0.004, 0.004);
        assert_superset(&st, &seek);
        assert_superset(&st, &seek_distance(0.003, 0.003, 150.0));
        for i in (0..10_000).filter(|i| i % 10 != 0) {
            point_remove_node(&mut st, i);
        }
        assert_superset(&st, &seek);
        assert_superset(&st, &seek_distance(0.003, 0.003, 150.0));
        assert_eq!(st.pts.len(), 1_000);
        assert_eq!(st.tree.size(), 1_000);
    }

    #[test]
    fn non_finite_coordinates_stay_removable_but_never_seekable() {
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 7203, f64::NAN, 1.0, None);
        point_insert(&mut st, 2, 7203, f64::INFINITY, 2.0, None);
        point_insert(&mut st, 3, 7203, 1.0, 1.0, None);
        assert_eq!(st.pts.len(), 3);
        st.seal();
        assert_eq!(st.tree.size(), 1);
        assert_eq!(
            spatial_ids(&st, &seek_bbox(-10.0, -10.0, 10.0, 10.0)),
            HashSet::from([3])
        );
        point_remove_node(&mut st, 1);
        point_remove_node(&mut st, 2);
        assert_eq!(st.pts.len(), 1);
        assert_eq!(st.tree.size(), 1);
    }

    #[test]
    fn bbox_wrap_answers_across_the_antimeridian() {
        let mut st = PointIndex::default();
        // Both sides of the line, plus one far inside the complementary
        // rectangle a swapped normalization would answer instead.
        point_insert(&mut st, 1, 4326, 179.5, 10.0, None);
        point_insert(&mut st, 2, 4326, -179.5, 10.0, None);
        point_insert(&mut st, 3, 4326, 0.0, 10.0, None);
        st.seal();
        // lo.x > hi.x wraps: the two ±180-side points match, the equator
        // point does not.
        let seek = SpatialSeek::BBox {
            lo: point(4326, 179.0, -10.0),
            hi: point(4326, -179.0, 10.0),
        };
        assert_superset(&st, &seek);
        assert_eq!(
            spatial_ids(&st, &seek),
            HashSet::from([1, 2]),
            "the wrap band is answered, not its complement"
        );
    }

    #[test]
    fn geographic_distance_envelope_bounds_the_great_circle() {
        let mut st = PointIndex::default();
        // 60° out in longitude at latitude 60° is ~1655 km by great
        // circle — a flat metre-to-degree box (54.5° wide there) drops
        // it; the spherical bound (~64.8°) keeps it.
        point_insert(&mut st, 1, 4326, 60.0, 60.0, None);
        point_insert(&mut st, 2, 4326, -0.5, 60.5, None);
        point_insert(&mut st, 3, 4326, 0.5, 60.5, None);
        point_insert(&mut st, 4, 4326, 100.0, 60.0, None);
        st.seal();
        let seek = SpatialSeek::Distance {
            origin: point(4326, 0.0, 60.0),
            max: 3_000_000.0,
            inclusive: true,
        };
        assert_superset(&st, &seek);
        let matches = spatial_ids(&st, &seek);
        assert!(matches.contains(&1), "the 60°-out point is a match");
        assert!(matches.contains(&2) && matches.contains(&3));
        assert!(
            !matches.contains(&4),
            "100° out in longitude at 60° is ~5000 km, past the bound"
        );
    }

    #[test]
    fn distance_wrap_answers_across_the_antimeridian() {
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4326, 179.9, 10.0, None);
        point_insert(&mut st, 2, 4326, -179.9, 10.0, None);
        st.seal();
        // 30 km from the line's west side reaches the east side.
        let seek = SpatialSeek::Distance {
            origin: point(4326, -179.9, 10.0),
            max: 30_000.0,
            inclusive: true,
        };
        assert_superset(&st, &seek);
        assert_eq!(spatial_ids(&st, &seek), HashSet::from([1, 2]));
    }

    #[test]
    fn negative_heights_fall_back_to_the_full_posting_set() {
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4979, 10.0, 10.0, None);
        point_insert(&mut st, 2, 4979, 10.5, 10.5, None);
        st.seal();
        // A non-negative height keeps the spherical envelope.
        let seek = SpatialSeek::Distance {
            origin: point(4979, 10.0, 10.0),
            max: 100_000.0,
            inclusive: true,
        };
        assert!(!st.has_negative_height);
        assert_eq!(spatial_ids(&st, &seek), HashSet::from([1, 2]));
        // A negative height makes no bounded envelope sound: the seek
        // falls back to every posting.
        point_insert(&mut st, 3, 4979, 20.0, 20.0, Some(-1.0));
        assert!(st.has_negative_height);
        let far = SpatialSeek::Distance {
            origin: point(4979, 10.0, 10.0),
            max: 1.0,
            inclusive: true,
        };
        assert_superset(&st, &far);
        assert_eq!(spatial_ids(&st, &far).len(), 3, "full posting fallback");
    }

    proptest::proptest! {
        /// Random insert/remove churn, then bbox and distance seeks on both
        /// SRID families: candidates must be exactly the posted ids that
        /// reach the tree (no ghosts), and a superset of the residual's
        /// matches.
        #[test]
        fn spatial_ids_superset_under_churn(
            ops in proptest::collection::vec(
                (
                    0..64i64,
                    proptest::prop_oneof![
                        // Clustered: half the inserts land in a tight box.
                        (0.0f64..1.0, 0.0f64..1.0),
                        (-5.0f64..5.0, -5.0f64..5.0),
                    ],
                    proptest::prop_oneof![
                        proptest::prelude::Just(7203i32),
                        proptest::prelude::Just(4326)
                    ],
                    proptest::prelude::any::<bool>(),
                ),
                0..200,
            ),
        ) {
            let mut st = PointIndex::default();
            for (id, (x, y), srid, remove) in ops {
                if remove {
                    point_remove_node(&mut st, id);
                } else {
                    point_insert(&mut st, id, srid, x, y, None);
                }
            }
            st.seal();
            assert_eq!(st.tree.size(), st.pts.len());
            for seek in [
                seek_bbox(-2.0, -2.0, 2.0, 2.0),
                seek_bbox(0.0, 0.0, 0.5, 0.5),
                seek_distance(0.0, 0.0, 500.0),
                seek_distance(0.5, 0.5, 50.0),
            ] {
                assert_superset(&st, &seek);
            }
        }
    }
}
