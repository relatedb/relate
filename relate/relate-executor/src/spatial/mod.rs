//! Point construction and geospatial predicates.
//!
//! Every `point()` argument rejection *inside a map* — value-domain
//! (unknown srid/crs, dimension mismatch, missing coordinate keys,
//! out-of-range latitude, non-finite coordinates, `crs`/`srid` conflict)
//! and type (a string where a coordinate belongs, a non-string `crs`, a
//! non-integer `srid`) alike — carries Neo4j's `ArgumentError` class via
//! [`Error::argument_error`]. The non-map argument itself is the one
//! exception: Neo4j's compile-time signature check reports it as
//! `SyntaxError`. Live-verified 2026-11-06 on
//! `neo4j:2026.09.0-enterprise` (Cypher 25) through the driver's error
//! code: every in-map shape → `Neo.ClientError.Statement.ArgumentError`
//! (GQLSTATUS 22N20/22N21/22N23 for values, 22N27 → 22G03 for types),
//! `point(5)` / `point('foo')` → `Neo.ClientError.Statement.SyntaxError`.
//! The converted `SpatialFunctionsAcceptanceTest` corpus asserts the same
//! classes. The `InvalidArgumentValue:` / `InvalidArgumentType:` detail
//! tokens are kept verbatim — only the class changes (the ADR-0489
//! vocabulary).

use std::collections::BTreeMap;

use relate_core::{Error, ErrorKind, Result, Value};

/// `point()` argument handling (ADR-0167, LABS-169). Nodes and relationships
/// are resolved to their property maps by the caller (`bi_point`), so what
/// reaches this function is maps, null, and any other runtime value: the
/// planner's `point` shape rule rejects a statically-known non-map argument
/// (a literal; `function_meta.rs`), and this `_` arm is the runtime
/// backstop for the shapes only execution can see (a non-map parameter, a
/// projected value). Live neo4j:2026.09 reports every non-map spelling —
/// literal or parameter — as the SyntaxError class this arm carries.
pub(crate) fn point_construct(vals: &[Value]) -> Result<Value> {
    match vals.first() {
        Some(Value::Map(map)) => point_from_map(map),
        Some(Value::Null) => Ok(Value::Null),
        // Live neo4j:2026.09 (Cypher 25) rejects a non-map `point()`
        // argument at compile time with `Neo.ClientError.Statement.
        // SyntaxError` ("Type mismatch: expected Map, Node or
        // Relationship but was <type>"); the class is the SyntaxError
        // family, unlike every in-map rejection below.
        _ => Err(Error::syntax(
            "InvalidArgumentType: point() requires a map, node, or relationship",
        )),
    }
}

/// Coordinate family of a point: planar cartesian (`x`/`y`) or geographic
/// WGS 84 (`latitude`/`longitude`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    Cartesian,
    Geographic,
}

impl Family {
    /// The family's three coordinate keys, from the model's one axis
    /// table (both spellings of each of the three axes) — the family
    /// selection probes the leading two, and the coordinate readers
    /// below name their keys from the same source.
    fn keys(self) -> [&'static str; 3] {
        relate_model::point::point_axis_keys(self == Family::Geographic)
    }
}

/// The one family selection: cartesian iff both leading cartesian keys
/// are present, else geographic iff both leading geographic keys are,
/// else the error — every key set and the error text derive from the two
/// families' own key tables, so a renamed axis cannot leave the probe or
/// the message spelling the old vocabulary.
fn family_from_map(map: &BTreeMap<String, Value>) -> Result<Family> {
    let cartesian = Family::Cartesian.keys();
    let geographic = Family::Geographic.keys();
    for (family, keys) in [
        (Family::Cartesian, cartesian),
        (Family::Geographic, geographic),
    ] {
        if map.contains_key(keys[0]) && map.contains_key(keys[1]) {
            return Ok(family);
        }
    }
    // The geographic pair is written latitude-first, matching Neo4j's own
    // wording ("must contain 'latitude' and 'longitude'"), not the axis
    // table's x/y order the probe above used.
    Err(Error::argument_value_kind(
        ErrorKind::InvalidArgumentValue,
        format!(
            "point() requires either '{}' and '{}', or '{}' and '{}'",
            cartesian[0], cartesian[1], geographic[1], geographic[0]
        ),
    ))
}

/// The spatial reference identifier domain: exactly the four codes Neo4j's
/// `point()` accepts (ADR-0167).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Srid {
    Wgs84_2D,
    Wgs84_3D,
    Cartesian2D,
    Cartesian3D,
}

impl Srid {
    /// The whole four-code domain — the one table `from_code`, `from_name`
    /// and the two expected-list messages below derive from, so a new or
    /// renamed srid cannot leave a message advertising the old vocabulary.
    const ALL: [Srid; 4] = [
        Srid::Wgs84_2D,
        Srid::Wgs84_3D,
        Srid::Cartesian2D,
        Srid::Cartesian3D,
    ];

    /// The srid for a raw code; codes outside `point()`'s four have none
    /// (reachable only through the load surface's raw `SRID=`).
    fn from_code(code: i32) -> Option<Srid> {
        Self::ALL.into_iter().find(|srid| srid.code() == code)
    }

    /// Validate a raw code against the four-srid domain.
    fn from_i64(i: i64) -> Result<Srid> {
        i32::try_from(i)
            .ok()
            .and_then(Srid::from_code)
            .ok_or_else(|| {
                Error::argument_value_kind(
                    ErrorKind::InvalidArgumentValue,
                    format!(
                        "point() unknown srid {i}: expected one of {}",
                        Self::ALL
                            .iter()
                            .map(|srid| srid.code().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            })
    }

    fn code(self) -> i32 {
        match self {
            Srid::Wgs84_2D => 4326,
            Srid::Wgs84_3D => 4979,
            Srid::Cartesian2D => 7203,
            Srid::Cartesian3D => 9157,
        }
    }

    /// The CRS-name spelling of the same four-entry domain (`crs` is the
    /// name form of `srid`); the two matches are the one vocabulary the
    /// `ALL`-derived messages advertise.
    fn name(self) -> &'static str {
        match self {
            Srid::Wgs84_2D => "wgs-84",
            Srid::Wgs84_3D => "wgs-84-3d",
            Srid::Cartesian2D => "cartesian",
            Srid::Cartesian3D => "cartesian-3d",
        }
    }

    fn family(self) -> Family {
        match self {
            Srid::Wgs84_2D | Srid::Wgs84_3D => Family::Geographic,
            Srid::Cartesian2D | Srid::Cartesian3D => Family::Cartesian,
        }
    }

    fn is_3d(self) -> bool {
        matches!(self, Srid::Wgs84_3D | Srid::Cartesian3D)
    }

    /// The four CRS names Neo4j's `point()` accepts, case-insensitively:
    /// `crs` is the name spelling of the same four-code domain.
    fn from_name(name: &str) -> Result<Srid> {
        Self::ALL
            .into_iter()
            .find(|srid| srid.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                Error::argument_value_kind(
                    ErrorKind::InvalidArgumentValue,
                    format!(
                        "point() unknown crs '{name}': expected one of {}",
                        Self::ALL
                            .iter()
                            .map(|srid| format!("'{}'", srid.name()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            })
    }

    /// The srid Neo4j picks when the map has no `srid` key.
    fn default(family: Family, has_z: bool) -> Srid {
        match (family, has_z) {
            (Family::Geographic, false) => Srid::Wgs84_2D,
            (Family::Geographic, true) => Srid::Wgs84_3D,
            (Family::Cartesian, false) => Srid::Cartesian2D,
            (Family::Cartesian, true) => Srid::Cartesian3D,
        }
    }
}

/// The coordinate keys `point()` reads, derived from the shared axis
/// vocabulary beside `PointValue` (both spellings of each of the three
/// axes) — the one table the null short-circuit and the family selection
/// consult (LABS-752's one-table remedy; the readers below take their keys
/// from this set's members).
const COORDINATE_KEYS: [&str; 6] = {
    let axes = relate_model::point::POINT_AXES;
    let mut keys = [""; 6];
    let mut i = 0;
    while i < axes.len() {
        keys[i * 2] = axes[i].cartesian;
        keys[i * 2 + 1] = axes[i].geographic;
        i += 1;
    }
    keys
};

/// A null in any coordinate key makes the whole point null (Neo4j-verified:
/// "If any argument … is null, null will be returned"). Checked before the
/// family and type rules, so `point({x: 1, y: null})` is null, not a type
/// error — the same rule the node predicates already apply to null points.
fn null_coordinate(map: &BTreeMap<String, Value>) -> bool {
    COORDINATE_KEYS
        .iter()
        .any(|key| matches!(map.get(*key), Some(Value::Null)))
}

/// Neo4j-verified construction rules: cartesian iff both `x` and `y` are
/// present, else geographic iff both `latitude` and `longitude` are present,
/// else an error. `crs` names the CRS (the name spelling of `srid`); other
/// extra properties are ignored.
fn point_from_map(map: &BTreeMap<String, Value>) -> Result<Value> {
    if null_coordinate(map) {
        return Ok(Value::Null);
    }
    let family = family_from_map(map)?;
    let (cartesian, geographic) = (Family::Cartesian.keys(), Family::Geographic.keys());
    let (mut x, y, z) = match family {
        // `height` is ignored for cartesian points.
        Family::Cartesian => (
            required_coordinate(map, cartesian[0])?,
            required_coordinate(map, cartesian[1])?,
            coordinate(map, cartesian[2])?,
        ),
        // Geographic: longitude is x, latitude is y; `z` wins over `height`.
        // `height` is still type-checked eagerly when a valid `z` wins —
        // deliberate, Neo4j-verified (a non-numeric height errors, 22N27).
        Family::Geographic => (
            required_coordinate(map, geographic[0])?,
            required_coordinate(map, geographic[1])?,
            // The axis-2 cartesian spelling (`z`) is accepted for
            // geographic maps too and wins over the geographic one
            // (`height`) — both from the axis table, no third spelling.
            coordinate(map, cartesian[2])?.or(coordinate(map, geographic[2])?),
        ),
    };
    let srid = resolve_srid(map, family, z)?;
    if family == Family::Geographic {
        if !(-90.0..=90.0).contains(&y) {
            return Err(Error::argument_value_kind(
                ErrorKind::InvalidArgumentValue,
                "point() latitude must be in the range [-90, 90]",
            ));
        }
        x = wrap_longitude(x);
    }
    Ok(Value::Point(relate_core::PointValue {
        srid: srid.code(),
        x,
        y,
        z,
    }))
}

/// Resolve the map's CRS specification: `srid` is the code spelling and
/// `crs` the name spelling of the same four-code domain; both may be
/// present when they agree. Absent picks the family/dimension default; a
/// present value must match the coordinate dimension (3D srid iff `z` is
/// present), and geographic coordinates reject cartesian srids.
fn resolve_srid(map: &BTreeMap<String, Value>, family: Family, z: Option<f64>) -> Result<Srid> {
    // The named srid carries its own spelling, so the conflict error below
    // reads it from the pair instead of re-deriving it behind an invariant.
    let named: Option<(Srid, &str)> = match map.get("crs") {
        None => None,
        Some(Value::String(name)) => Some((Srid::from_name(name)?, &**name)),
        Some(_) => {
            return Err(Error::argument_value_kind(
                ErrorKind::InvalidArgumentType,
                "point() 'crs' must be a string",
            ))
        }
    };
    let srid = match map.get("srid") {
        None => match named {
            None => return Ok(Srid::default(family, z.is_some())),
            Some((srid, _)) => srid,
        },
        Some(Value::Int(i)) => {
            let srid = Srid::from_i64(*i)?;
            if let Some((named_srid, name)) = named {
                if named_srid != srid {
                    return Err(Error::argument_value_kind(
                        ErrorKind::InvalidArgumentValue,
                        format!("point() crs '{name}' conflicts with srid {}", srid.code()),
                    ));
                }
            }
            srid
        }
        Some(_) => {
            return Err(Error::argument_value_kind(
                ErrorKind::InvalidArgumentType,
                "point() 'srid' must be an integer",
            ))
        }
    };
    if srid.is_3d() != z.is_some() {
        return Err(Error::argument_value_kind(
            ErrorKind::InvalidArgumentValue,
            format!(
                "point() srid {} does not match the coordinate dimension",
                srid.code()
            ),
        ));
    }
    if family == Family::Geographic && srid.family() == Family::Cartesian {
        return Err(Error::argument_value_kind(
            ErrorKind::InvalidArgumentValue,
            "point() geographic coordinates require srid 4326 or 4979",
        ));
    }
    Ok(srid)
}

/// A coordinate key that may be absent; present values must be finite
/// numbers. Neo4j-verified (22N24): NaN and ±Infinity are rejected for
/// every coordinate key.
fn coordinate(map: &BTreeMap<String, Value>, key: &str) -> Result<Option<f64>> {
    let value = match map.get(key) {
        None => return Ok(None),
        Some(Value::Int(i)) => *i as f64,
        Some(Value::Float(f)) => *f,
        Some(_) => return Err(coordinate_type_error(key)),
    };
    if !value.is_finite() {
        return Err(Error::argument_value_kind(
            ErrorKind::InvalidArgumentValue,
            format!("point() coordinate '{key}' must be finite"),
        ));
    }
    Ok(Some(value))
}

/// The one construction of the pinned wrong-type wording for a `point()`
/// coordinate key — the reader's wrong-typed case, and
/// `required_coordinate`'s own fallback for a key the family selection
/// proved present (unreachable there; see that function). ArgumentError
/// like every other `point()` argument rejection (see the module doc).
fn coordinate_type_error(key: &str) -> Error {
    Error::argument_value_kind(
        ErrorKind::InvalidArgumentType,
        format!("point() coordinate '{key}' must be a float or integer"),
    )
}

/// A coordinate key guaranteed present by the family selection
/// (`family_from_map` returns a family only when both its leading keys
/// are in the map, and `null_coordinate` has already run); a present
/// value that is not a number is the one reachable error.
fn required_coordinate(map: &BTreeMap<String, Value>, key: &str) -> Result<f64> {
    coordinate(map, key)?.ok_or_else(|| coordinate_type_error(key))
}

/// Neo4j longitude wrap into [-180, 180], keeping both endpoints
/// (180.0 → 180.0, -180.0 → -180.0, 180.5 → -179.5, -200.0 → 160.0,
/// -540.0 → -180.0). Mirrors Neo4j's `PointValue` constructor: a
/// truncating remainder lands in (-360, 360) and one subtraction folds it
/// in, so exact negative multiples of 360 keep the negative endpoint.
fn wrap_longitude(longitude: f64) -> f64 {
    let mut x = longitude;
    if !(-180.0..=180.0).contains(&x) {
        x %= 360.0;
        if x > 180.0 {
            x -= 360.0;
        } else if x < -180.0 {
            x += 360.0;
        }
    }
    // Normalize -0.0 (from -360 % 360) to +0.0.
    if x == 0.0 {
        0.0
    } else {
        x
    }
}

/// A point's two horizontal coordinates, `[x, y]` (longitude, latitude).
fn xy(p: &relate_core::PointValue) -> [f64; 2] {
    [p.x, p.y]
}

/// A point's three coordinates, `[x, y, height]`. A 2D point has no height
/// axis, so its projected height is 0 — the neutral value for both the
/// planar norm and the geodesic's orthogonal height leg. The group's
/// dimension is validated up front, so a group never mixes 2D and 3D points.
fn xyz(p: &relate_core::PointValue) -> [f64; 3] {
    match p.z {
        Some(height) => [p.x, p.y, height],
        None => [p.x, p.y, 0.0],
    }
}

/// Decode and validate a point group in one pass: `Ok(None)` when the points
/// name different CRSes (Neo4j compares coordinate reference systems
/// exactly, so the predicates answer null), else the one family they then
/// dispatch on. Two rules decide the family and the dimension:
///
/// - a srid inside `point()`'s four pins both the family and the dimension,
///   so every point's height must be present iff that srid is 3D;
/// - a srid outside that domain (reachable only through the load surface's
///   raw `SRID=`) decodes to no CRS and is treated as planar cartesian by
///   every predicate, with the points merely having to agree with each other
///   on the dimension.
///
/// `point()` maintains these invariants, but the load surface takes the srid
/// from `SRID=` and the coordinate arity from the `POINT Z` marker without
/// cross-checking, so a disagreement is reachable and is surfaced rather
/// than silently answered as planar or with an unconstrained height axis.
/// The group's srid is `first.srid`, since a CRS mismatch answers `None`.
fn decode_family(
    first: &relate_core::PointValue,
    rest: &[&relate_core::PointValue],
) -> Result<Option<Family>> {
    if rest.iter().any(|p| p.srid != first.srid) {
        return Ok(None);
    }
    let (family, has_height) = match Srid::from_code(first.srid) {
        Some(srid) => (srid.family(), srid.is_3d()),
        None => (Family::Cartesian, first.z.is_some()),
    };
    if std::iter::once(first)
        .chain(rest.iter().copied())
        .any(|p| p.z.is_some() != has_height)
    {
        return Err(inconsistent_dimension(first.srid));
    }
    Ok(Some(family))
}

/// The one wording for a stored point whose height contradicts its srid.
fn inconsistent_dimension(srid: i32) -> Error {
    Error::execution(format!(
        "InvalidPoint: srid {srid} does not match the coordinate dimension"
    ))
}

/// Neo4j's overflow-safe Euclidean norm (`CRSCalculator.pythagoras`):
/// scale every axis by the largest difference before squaring, so
/// coordinates near `f64::MAX` cannot overflow to Infinity. The axis count
/// is part of the signature, so mismatched widths cannot be passed.
///
/// A NaN axis difference stays sticky: `f64::max` ignores NaN where Java's
/// `Math.max` propagates it, so a NaN coordinate yields NaN distance rather
/// than a fabricated zero. Hoisted out of the fold so the fold stays a plain
/// per-axis maximum.
fn pythagoras<const N: usize>(a: &[f64; N], b: &[f64; N]) -> f64 {
    if a.iter().zip(b).any(|(x, y)| (x - y).is_nan()) {
        return f64::NAN;
    }
    let max = a
        .iter()
        .zip(b)
        .fold(0.0f64, |acc, (x, y)| acc.max((x - y).abs()));
    if max == 0.0 || !max.is_finite() {
        return max;
    }
    let scaled_sum: f64 = a.iter().zip(b).map(|(x, y)| ((x - y) / max).powi(2)).sum();
    max * scaled_sum.sqrt()
}

/// Neo4j's haversine earth radius in meters (LABS-256). The distance
/// seek's envelope geometry consumes the same constant — one fact, never
/// re-derived.
pub(crate) const EARTH_RADIUS_METERS: f64 = 6_378_140.0;

/// Great-circle central angle in radians over two `[longitude, latitude]`
/// pairs in degrees, ignoring any height.
fn great_circle(a: &[f64; 2], b: &[f64; 2]) -> f64 {
    let lon1 = a[0].to_radians();
    let lat1 = a[1].to_radians();
    let lon2 = b[0].to_radians();
    let lat2 = b[1].to_radians();
    let dlon = lon2 - lon1;
    let dlat = lat2 - lat1;
    let alpha = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * alpha.sqrt().atan2((1.0 - alpha).sqrt())
}

/// Geodesic distance over the ellipsoid-free sphere, with the height axis
/// treated as orthogonal to the surface distance (Neo4j's
/// `GeographicCalculator.distance`). The average height is halved before
/// adding so two extreme heights cannot overflow; `f64::hypot` is the
/// overflow-safe, NaN-propagating primitive for the two legs.
fn geographic_distance(a: &relate_core::PointValue, b: &relate_core::PointValue) -> f64 {
    let (a3, b3) = (xyz(a), xyz(b));
    let avg_height = a3[2] / 2.0 + b3[2] / 2.0;
    let surface = (EARTH_RADIUS_METERS + avg_height) * great_circle(&xy(a), &xy(b));
    surface.hypot(a3[2] - b3[2])
}

/// The one null policy of the point predicates: a null among the leading
/// `arity` arguments yields null.
fn has_null_argument(vals: &[Value], arity: usize) -> bool {
    vals.iter().take(arity).any(Value::is_null)
}

/// Distance between two points, or `None` when their CRSes differ (Neo4j
/// compares coordinate reference systems exactly: a dimensionality or
/// CRS-family mismatch is null, not a number). The one distance core: the
/// `point.distance` builtin and the `SpatialSeek::Distance` index seek over
/// its comparison form both ride it.
fn distance_between(
    a: &relate_core::PointValue,
    b: &relate_core::PointValue,
) -> Result<Option<f64>> {
    let Some(family) = decode_family(a, &[b])? else {
        return Ok(None);
    };
    let distance = match family {
        Family::Cartesian => pythagoras(&xyz(a), &xyz(b)),
        Family::Geographic => geographic_distance(a, b),
    };
    Ok(Some(distance))
}

pub(crate) fn point_distance(vals: &[Value]) -> Result<Value> {
    if has_null_argument(vals, 2) {
        return Ok(Value::Null);
    }
    match (vals.first(), vals.get(1)) {
        (Some(Value::Point(a)), Some(Value::Point(b))) => match distance_between(a, b)? {
            Some(distance) => Ok(Value::Float(distance)),
            None => Ok(Value::Null),
        },
        _ => Err(Error::ty("distance() requires two points")),
    }
}

/// Inclusive per-axis containment: `min <= v <= max`.
fn within(v: f64, min: f64, max: f64) -> bool {
    min <= v && v <= max
}

/// The bbox longitude axis's wrap decision — the one owner of the
/// residual predicate's wrap arm and the seek envelope builder's split
/// (the proptest oracle holds the two against each other): an inverted
/// longitude wraps for the geographic family and admits nothing
/// otherwise.
pub(crate) fn bbox_longitude_wraps(srid: i32, lo_x: f64, hi_x: f64) -> bool {
    relate_model::point::is_geographic(srid) && lo_x > hi_x
}

/// Inclusive containment in the three-axis box `[x, y, height]`: `p` lies
/// between `lo` and `hi` on every axis. The longitude axis rides the one
/// wrap decision ([`bbox_longitude_wraps`]): a wrapping box covers the
/// ±180 band instead of the inverted range; a non-wrapping (or
/// non-geographic) box is the plain interval, which an inverted axis
/// empties. A 2D group's height axis is 0 on all three points, which
/// leaves it unconstrained.
fn bbox_contains(p: &[f64; 3], lo: &[f64; 3], hi: &[f64; 3], srid: i32) -> bool {
    let first = if bbox_longitude_wraps(srid, lo[0], hi[0]) {
        // Wrap: [lo[0], 180] ∪ [-180, hi[0]].
        p[0] >= lo[0] || p[0] <= hi[0]
    } else {
        within(p[0], lo[0], hi[0])
    };
    first && within(p[1], lo[1], hi[1]) && within(p[2], lo[2], hi[2])
}

pub(crate) fn point_within_bbox(vals: &[Value]) -> Result<Value> {
    if has_null_argument(vals, 3) {
        return Ok(Value::Null);
    }
    match (vals.first(), vals.get(1), vals.get(2)) {
        (Some(Value::Point(p)), Some(Value::Point(lo)), Some(Value::Point(hi))) => {
            match decode_family(p, &[lo, hi])? {
                None => Ok(Value::Null),
                // The family decode proves one CRS across the three
                // points; the wrap decision rides the same srid.
                Some(_) => Ok(Value::Bool(bbox_contains(
                    &xyz(p),
                    &xyz(lo),
                    &xyz(hi),
                    p.srid,
                ))),
            }
        }
        _ => Err(Error::ty("point.withinBBox() requires three points")),
    }
}

#[cfg(test)]
mod tests;
