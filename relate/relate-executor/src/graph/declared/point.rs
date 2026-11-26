//! POINT index: an R\*-tree (rstar) over the posted points. The postings
//! are a superset — the residual `point.distance` / `point.withinBBox`
//! re-check owns z, SRID, wrap, and bound semantics.

use std::collections::{HashMap, HashSet};

use relate_core::{NodeId, Value};
use relate_model::point::{
    is_geographic, wrap_longitude, LATITUDE_MAX as LAT_MAX, LATITUDE_MIN as LAT_MIN,
    LONGITUDE_MAX as LON_MAX, LONGITUDE_MIN as LON_MIN, LONGITUDE_TURN as LON_TURN,
};
use rstar::{RTree, RTreeObject, AABB};

use super::order::SpatialSeek;
use super::DrainState;
use crate::spatial::{bbox_longitude_wraps, EARTH_RADIUS_METERS};

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
    /// Sticky: a posted geographic latitude sat outside ±90 (a
    /// verbatim-storing path's, LABS-1236). The residual's
    /// great-circle term multiplies the two latitudes' cosines, which
    /// flips sign past a pole, so its distance can undercut the
    /// band's |Δlat| bound — no bounded envelope is sound then, and
    /// distance seeks fall back to the full posting set.
    has_out_of_domain_latitude: bool,
}

impl Default for PointIndex {
    fn default() -> Self {
        Self {
            pts: HashMap::new(),
            tree: RTree::new(),
            dirty: HashSet::new(),
            bulk: true,
            has_negative_height: false,
            has_out_of_domain_latitude: false,
        }
    }
}

impl PointIndex {
    /// Distinct posted points — the research-gated census harness's count
    /// (ADR-0476 quarantine; `pts` stays private).
    #[cfg(feature = "research")]
    pub(crate) fn pt_count(&self) -> usize {
        self.pts.len()
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
    if is_geographic(srid) && !latitude_in_domain(y) {
        st.has_out_of_domain_latitude = true;
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
    for envelope in envelopes {
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

/// Longitude envelopes covering the unwrapped `[lo, hi]` band (`lo <= hi`,
/// ends within one [`LON_TURN`] of the stored `[LON_MIN, LON_MAX]` domain):
/// a band a turn or wider is the full range, otherwise each of the band's
/// up-to-three spellings is clipped to the domain. A band reaching the edge
/// therefore yields both stored spellings of the edge coordinate, ±180,
/// which the residual accepts separately.
fn lon_envelopes(lo: f64, hi: f64, miny: f64, maxy: f64) -> Vec<AABB<[f64; 2]>> {
    if hi - lo >= LON_TURN {
        return vec![AABB::from_corners([LON_MIN, miny], [LON_MAX, maxy])];
    }
    let mut out = Vec::new();
    for shift in [-LON_TURN, 0.0, LON_TURN] {
        let (lo, hi) = ((lo + shift).max(LON_MIN), (hi + shift).min(LON_MAX));
        if lo <= hi {
            out.push(AABB::from_corners([lo, miny], [hi, maxy]));
        }
    }
    out
}

/// The stored geographic domain's tests — the one spelling of the
/// domain both seek arms read. The box arm declines a coordinate
/// outside it (a verbatim-storing path's, per LABS-1236 — a Bolt point
/// parameter, ADR-0177 §2, or the C ABI); the distance arm lifts the
/// origin's longitude into it first and reads the test on the lifted
/// value, where only a non-finite coordinate fails. Either way a
/// coordinate not in the domain — or not finite at all — takes the
/// decline: no envelope, the full posting set, the residual the
/// ground truth.
fn longitude_in_domain(x: f64) -> bool {
    x.is_finite() && (LON_MIN..=LON_MAX).contains(&x)
}

/// The latitude half of [`longitude_in_domain`]'s policy.
fn latitude_in_domain(y: f64) -> bool {
    y.is_finite() && (LAT_MIN..=LAT_MAX).contains(&y)
}

/// The candidate envelopes for one seek — a thin dispatcher; empty means
/// no usable envelope, and the caller falls back to the full posting set.
/// The per-arm geometry lives in [`bbox_envelopes`] and
/// [`distance_envelopes`] so each band policy has one home.
fn seek_envelopes(st: &PointIndex, seek: &SpatialSeek) -> Vec<AABB<[f64; 2]>> {
    match seek {
        SpatialSeek::BBox { lo, hi } => bbox_envelopes(lo, hi),
        SpatialSeek::Distance {
            origin,
            max,
            inclusive: _,
        } => distance_envelopes(
            origin,
            *max,
            st.has_negative_height,
            st.has_out_of_domain_latitude,
        ),
    }
}

/// The box arm's envelopes. A box wrapping the antimeridian splits across
/// ±180 ([`LON_MIN`], [`LON_MAX`] — the domain `point()` enforces and the
/// Query API typed decoders wrap into); a Cartesian box is the raw
/// interval (any units); a geographic one rides [`lon_envelopes`]. The
/// clip is sound for those producers; a point a verbatim-storing path
/// accepted without that rule (a Bolt point parameter, ADR-0177 §2, or
/// the C ABI) can sit outside the band, and this clip can then miss a
/// value the residual accepts — the residual stays the ground truth
/// (LABS-1236).
fn bbox_envelopes(lo: &Value, hi: &Value) -> Vec<AABB<[f64; 2]>> {
    let (Value::Point(lo), Value::Point(hi)) = (lo, hi) else {
        return Vec::new();
    };
    // `AABB::from_corners` normalizes both axes, so an inverted corner
    // pair needs no sort of its own.
    if !is_geographic(lo.srid) {
        return vec![AABB::from_corners([lo.x, lo.y], [hi.x, hi.y])];
    }
    // A corner outside the stored domain (a verbatim-storing path's,
    // per LABS-1236) takes one decision for both arms below: no
    // envelope, the full posting set, and the residual stays the
    // ground truth.
    if !longitude_in_domain(lo.x) || !longitude_in_domain(hi.x) {
        return Vec::new();
    }
    // The wrap decision is the residual predicate's own
    // ([`bbox_longitude_wraps`]). A wrapping box answers from its exact
    // corners, never a band one full turn on: the shifted spelling
    // (`hi.x + LON_TURN`, clipped back by [`lon_envelopes`]'s −turn
    // shift) lands a float rounding *below* `hi.x`, so a point stored
    // exactly at the corner longitude is accepted by the residual and
    // dropped by the envelope.
    if bbox_longitude_wraps(lo.srid, lo.x, hi.x) {
        return vec![
            AABB::from_corners([lo.x, lo.y], [LON_MAX, hi.y]),
            AABB::from_corners([LON_MIN, lo.y], [hi.x, hi.y]),
        ];
    }
    lon_envelopes(lo.x, hi.x, lo.y, hi.y)
}

/// The distance arm's envelopes. A Cartesian seek uses the flat metre box,
/// the Pythagorean metric's exact bound. A geographic seek uses the
/// spherical disk's exact great-circle bounding box — the residual's own
/// surface metric (heights only add distance when they are non-negative; a
/// negative height anywhere makes no bounded envelope sound, and the seek
/// falls back to the full posting set). `EPS` pads the radius for float
/// rounding in the over-size direction; the residual keeps the bound
/// exact.
fn distance_envelopes(
    origin: &Value,
    max: f64,
    negative_height: bool,
    out_of_domain_latitude: bool,
) -> Vec<AABB<[f64; 2]>> {
    const EPS: f64 = 1.01;
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
    // One malformed-input policy for the arm, stated on the raw
    // operands: a non-finite origin coordinate (which no band is
    // built off) or an origin latitude outside ±90 (which flips the
    // pole math's cosine — latitude has no stored spelling to lift
    // to) declines, exactly as a non-finite radius does below. The
    // residual's great-circle difference is exactly periodic in
    // longitude (its `dlon` term is squared through a half-turn
    // sine), so a finite origin longitude outside the stored domain
    // (LABS-1236) is instead lifted to its stored spelling — every
    // longitude the band then answers is one the residual accepts
    // for the raw origin too.
    if !origin.x.is_finite() || !latitude_in_domain(origin.y) {
        return Vec::new();
    }
    // Heights: the radial multiplier is `R + avg_height`, so a
    // negative height anywhere (posted point or origin) can shrink
    // the great-circle term below the surface bound.
    if negative_height || origin.z.is_some_and(|z| z < 0.0) || out_of_domain_latitude {
        return Vec::new();
    }
    // The disk's angular radius, padded. At or past a quarter
    // circumference the cap holds every longitude and spans more
    // than a hemisphere, so no band is worth pruning — and a band
    // built from a CLAMPED radius silently drops the far
    // hemisphere (an 89°N point is not a candidate for an origin
    // at 0°, -10°). Return no envelope: the seek falls back to
    // the full posting set, the whole-globe case the clamp
    // reached for, while the residual stays the ground truth. A
    // NaN (or infinite) radius gets no envelope either.
    let delta = max.abs() / EARTH_RADIUS_METERS * EPS;
    if !delta.is_finite() || delta >= std::f64::consts::FRAC_PI_2 {
        return Vec::new();
    }
    let origin_x = wrap_longitude(origin.x);
    let lat = origin.y.to_radians();
    // The latitude band is deliberately unclamped: the sticky
    // out-of-domain flag above guarantees every posted latitude sits
    // in ±90, so the band — which may reach past a pole — covers
    // every posted latitude a residual match can carry, and a pole
    // clamp would only drop candidates near it. Only the longitude
    // axis is clipped, by [`lon_envelopes`].
    let miny = (lat - delta).to_degrees();
    let maxy = (lat + delta).to_degrees();
    // The disk's maximal longitude deviation (Jan Matuschek's
    // bounding-box derivation): past the pole-reaching radius the
    // deviation is every longitude.
    let lon_delta = if delta.sin() >= lat.cos().abs() {
        LON_MAX
    } else {
        (delta.sin() / lat.cos().abs()).asin().to_degrees()
    };
    lon_envelopes(origin_x - lon_delta, origin_x + lon_delta, miny, maxy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_test_support::values::point;

    fn seek_bbox(srid: i32, minx: f64, miny: f64, maxx: f64, maxy: f64) -> SpatialSeek {
        SpatialSeek::BBox {
            lo: point(srid, minx, miny),
            hi: point(srid, maxx, maxy),
        }
    }

    fn seek_distance(srid: i32, x: f64, y: f64, max: f64) -> SpatialSeek {
        SpatialSeek::Distance {
            origin: point(srid, x, y),
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
        assert_superset(&st, &seek_bbox(7203, 0.05, 0.05, 0.25, 0.25));
        // A sealed tree keeps taking incremental inserts and removals.
        point_insert(&mut st, 9_999, 4326, 0.10, 0.10, None);
        assert!(spatial_ids(&st, &seek_bbox(7203, 0.05, 0.05, 0.25, 0.25)).contains(&9_999));
        point_remove_node(&mut st, 9_999);
        assert!(!spatial_ids(&st, &seek_bbox(7203, 0.05, 0.05, 0.25, 0.25)).contains(&9_999));
        // The seal is idempotent.
        st.seal();
        assert_superset(&st, &seek_bbox(7203, 0.05, 0.05, 0.25, 0.25));
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
        let seek = seek_bbox(7203, 0.002, 0.002, 0.004, 0.004);
        assert_superset(&st, &seek);
        assert_superset(&st, &seek_distance(7203, 0.003, 0.003, 150.0));
        for i in (0..10_000).filter(|i| i % 10 != 0) {
            point_remove_node(&mut st, i);
        }
        assert_superset(&st, &seek);
        assert_superset(&st, &seek_distance(7203, 0.003, 0.003, 150.0));
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
            spatial_ids(&st, &seek_bbox(7203, -10.0, -10.0, 10.0, 10.0)),
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
        let seek = seek_bbox(4326, 179.0, -10.0, -179.0, 10.0);
        assert_superset(&st, &seek);
        assert_eq!(
            spatial_ids(&st, &seek),
            HashSet::from([1, 2]),
            "the wrap band is answered, not its complement"
        );
    }

    #[test]
    fn bbox_wrap_keeps_a_point_stored_exactly_at_the_corner_longitude() {
        // A wrapping box's band was once built one full turn on and
        // clipped back, and the round trip landed a float rounding
        // below `hi.x` — a point stored at exactly the corner longitude
        // was accepted by the residual and never an envelope candidate.
        // The exact-corner envelopes answer it.
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4326, 0.7, 10.0, None);
        point_insert(&mut st, 2, 4326, 179.5, 10.0, None);
        point_insert(&mut st, 3, 4326, -179.5, 10.0, None);
        st.seal();
        let seek = seek_bbox(4326, 170.0, -10.0, 0.7, 10.0);
        assert_superset(&st, &seek);
        assert_eq!(spatial_ids(&st, &seek), HashSet::from([1, 2, 3]));
    }

    #[test]
    fn a_posted_latitude_outside_the_domain_takes_the_decline() {
        // A verbatim-storing path can post a latitude outside ±90
        // (LABS-1236), where the residual's great-circle term flips
        // its cosine and can undercut the band's |Δlat| bound — a
        // band is not a superset for it (the rust review's (80°, 95°)
        // case). The sticky flag declines the whole seek — the full
        // posting set answers and the residual stays the ground
        // truth, the negative-height arm's own decision.
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4326, 0.0, 95.0, None);
        point_insert(&mut st, 2, 4326, 0.0, 10.0, None);
        st.seal();
        assert!(st.has_out_of_domain_latitude);
        let seek = seek_distance(4326, 0.0, 10.0, 9_600_000.0);
        assert_superset(&st, &seek);
        assert_eq!(
            spatial_ids(&st, &seek),
            HashSet::from([1, 2]),
            "the seek declines: every posting answers"
        );
        // The flipped cosine is why no band is sound: (80°, 95°) sits
        // 9,006,237 m from (0°, 10°) — inside a 9,020,000 m radius —
        // although its |Δlat| alone puts it past any band a sound
        // origin could build.
        assert!(spatial_holds(
            &point(4326, 80.0, 95.0),
            &seek_distance(4326, 0.0, 10.0, 9_020_000.0)
        ));
    }

    #[test]
    fn a_non_finite_origin_takes_the_decline_not_a_full_range_band() {
        // One malformed-input policy for the whole arm: a non-finite
        // origin coordinate declines (full posting set, residual the
        // ground truth) exactly as a non-finite radius does, never a
        // band whose NaN edges clip to the whole longitude range.
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4326, 10.0, 10.0, None);
        point_insert(&mut st, 2, 4326, 20.0, 20.0, None);
        st.seal();
        let seek = seek_distance(4326, f64::NAN, 10.0, 100_000.0);
        assert_eq!(
            spatial_ids(&st, &seek),
            HashSet::from([1, 2]),
            "the seek declines: every posting answers, the residual rejects"
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
    fn distance_past_a_quarter_circumference_keeps_the_far_hemisphere() {
        // The corpus graph: the 89°N point is 11,021 km from an origin at
        // 0°,-10°. A radius past a quarter circumference (10,018 km) must
        // still answer it — a band computed from the clamped angular
        // radius ends at origin + 90° = 80°N and drops it.
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4326, 0.0, 89.0, None);
        point_insert(&mut st, 2, 4326, 0.0, 70.0, None);
        point_insert(&mut st, 3, 4326, 0.0, 0.0, None);
        st.seal();
        assert_superset(&st, &seek_distance(4326, 0.0, -10.0, 12_000_000.0));
        assert_superset(&st, &seek_distance(4326, 0.0, -60.0, 19_000_000.0));
        // Below the quarter circumference the band still prunes exactly:
        // 9,000 km from 0°,-10° keeps the 70°N and 0°N points only.
        assert_eq!(
            spatial_ids(&st, &seek_distance(4326, 0.0, -10.0, 9_000_000.0)),
            HashSet::from([2, 3])
        );
    }

    #[test]
    fn bbox_wrap_ending_at_the_antimeridian_keeps_the_180_meridian() {
        // A wrap band `lo.x .. -180` includes longitude -180 itself; the
        // ±180 band's two stored spellings are distinct coordinates, and
        // the residual accepts both.
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4326, -180.0, 10.0, None);
        point_insert(&mut st, 2, 4326, 180.0, 10.0, None);
        point_insert(&mut st, 3, 4326, 0.0, 10.0, None);
        st.seal();
        let seek = seek_bbox(4326, 179.0, -10.0, -180.0, 10.0);
        assert_superset(&st, &seek);
        assert_eq!(spatial_ids(&st, &seek), HashSet::from([1, 2]));
    }

    #[test]
    fn bbox_wrap_with_corners_outside_the_domain_keeps_the_residual_matches() {
        // A verbatim-storing path can post both corners below -180
        // (LABS-1236). The residual's wrap test then accepts the whole
        // domain (`p >= -200` is every stored longitude), so the seek
        // declines the envelope and the full posting set answers — a band
        // lifted from the raw corners would clip the middle longitudes.
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4326, -170.0, 10.0, None);
        point_insert(&mut st, 2, 4326, 140.0, 10.0, None);
        point_insert(&mut st, 3, 4326, 0.0, 10.0, None);
        st.seal();
        let seek = seek_bbox(4326, -200.0, -10.0, -250.0, 10.0);
        assert_superset(&st, &seek);
        assert_eq!(spatial_ids(&st, &seek), HashSet::from([1, 2, 3]));
    }

    #[test]
    fn distance_with_an_origin_outside_the_domain_answers_at_its_stored_spelling() {
        // A verbatim-storing path can hand the seek an origin past a
        // full turn outside the stored domain (LABS-1236): 540.5° is
        // the residual's exact equal of -179.5° (their longitude
        // difference is a whole turn, invisible to the great-circle
        // difference's squared half-turn sine), so the band is built
        // at the lifted spelling instead of declining — a band clipped
        // around the raw longitude would answer only one spelling and
        // drop the matches.
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4326, -179.5, 10.0, None);
        point_insert(&mut st, 2, 4326, 0.0, 10.0, None);
        st.seal();
        let seek = seek_distance(4326, 540.5, 10.0, 100_000.0);
        assert_superset(&st, &seek);
        // The lifted band prunes exactly: the stored spelling's point
        // is a candidate (distance zero), the half-turn point is not
        // (~17,800 km).
        assert_eq!(
            spatial_ids(&st, &seek),
            HashSet::from([1]),
            "the origin's stored spelling is answered, the far point is not"
        );
        assert!(spatial_holds(&point(4326, -179.5, 10.0), &seek));
        assert!(!spatial_holds(&point(4326, 0.0, 10.0), &seek));
    }

    #[test]
    fn distance_with_a_latitude_outside_the_domain_takes_the_decline() {
        // Latitude has no stored spelling to lift to — an origin
        // outside ±90 breaks the band's pole math, so the seek declines
        // (no envelope, the full posting set), the same decision the
        // box arm takes for a domain breach.
        let mut st = PointIndex::default();
        point_insert(&mut st, 1, 4326, -179.5, 10.0, None);
        point_insert(&mut st, 2, 4326, 0.0, 10.0, None);
        st.seal();
        assert_eq!(
            spatial_ids(&st, &seek_distance(4326, 0.0, 95.0, 100_000.0)),
            HashSet::from([1, 2])
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
                seek_bbox(7203, -2.0, -2.0, 2.0, 2.0),
                seek_bbox(7203, 0.0, 0.0, 0.5, 0.5),
                seek_distance(7203, 0.0, 0.0, 500.0),
                seek_distance(7203, 0.5, 0.5, 50.0),
            ] {
                assert_superset(&st, &seek);
            }
        }

        /// A geographic distance seek is a superset of its residual's
        /// matches at every radius up to half a circumference — the
        /// corpus's 400-point scan shape, with the poles always posted.
        #[test]
        fn geographic_distance_seek_superset_up_to_half_a_circumference(
            pts in proptest::collection::vec((-180.0f64..180.0, -90.0f64..90.0), 1..64),
            ox in -180.0f64..180.0,
            oy in -90.0f64..90.0,
            frac in 0.0f64..1.0,
        ) {
            let mut st = PointIndex::default();
            point_insert(&mut st, 0, 4326, 0.0, 90.0, None);
            point_insert(&mut st, 1, 4326, 0.0, -90.0, None);
            for (i, (x, y)) in pts.iter().enumerate() {
                point_insert(&mut st, i as i64 + 2, 4326, *x, *y, None);
            }
            st.seal();
            let max = frac * std::f64::consts::PI * EARTH_RADIUS_METERS;
            assert_superset(&st, &seek_distance(4326, ox, oy, max));
        }
    }
}
