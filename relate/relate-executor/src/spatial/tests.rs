//! Behavior tests for `crate::spatial`: point construction, `point()`
//! SRID/CRS resolution, `point.distance`, and `point.withinBBox`.
//!
//! Ground truth: Neo4j's `PointValue`/`GeographicCalculator`/
//! `CRSCalculator` behavior (ADR-0167, LABS-169, LABS-256, LABS-711).

use relate_core::{Error, PointValue, Result, Value};

use crate::test_support::map;

use super::{point_construct, point_distance, point_within_bbox, pythagoras, wrap_longitude, Srid};

fn point(pairs: &[(&str, Value)]) -> Result<Value> {
    point_construct(&[Value::Map(map(pairs).into())])
}

fn srid_xyz(v: &Value) -> (i32, f64, f64, Option<f64>) {
    let Value::Point(p) = v else {
        panic!("expected point, got {v:?}")
    };
    (p.srid, p.x, p.y, p.z)
}

/// 2D WGS-84 point through the canonical constructor.
fn geo(longitude: f64, latitude: f64) -> Value {
    Value::Point(PointValue::wgs84_2d(longitude, latitude))
}

fn distance_of(a: Value, b: Value) -> Value {
    point_distance(&[a, b]).unwrap()
}

#[test]
fn geographic_distance_uses_neo4j_haversine_radius() {
    // LABS-256: the openstreetmap walkthrough pair — Delacorte Musical
    // Clock to Zoo School. Neo4j returns 21.493084025393262, which is
    // the haversine with a 6_378_140 m earth radius (not the IUGG mean
    // radius 6_371_008.8 m, which yields 21.469…).
    let clock = geo(-73.9711748, 40.7681147);
    let zoo_school = geo(-73.9709581, 40.768013);
    let Value::Float(d) = distance_of(clock, zoo_school) else {
        panic!("expected float distance")
    };
    // The store rounds coordinates to 7 decimals (~11 mm), so compare
    // against the Neo4j reference with slack for that quantization.
    assert!(
        (d - 21.493084025393262).abs() < 1e-3,
        "distance {d} diverges from the Neo4j reference 21.493084025393262"
    );
}

/// `Srid::ALL` is the one table the code lookups and both expected-list
/// messages derive from, and a fixed-size array cannot notice a new
/// variant. The match below must name every variant, so an addition fails
/// to compile here instead of silently shrinking the accepted domain.
#[test]
fn srid_all_lists_every_variant() {
    let named: Vec<Srid> = [
        Srid::Wgs84_2D,
        Srid::Wgs84_3D,
        Srid::Cartesian2D,
        Srid::Cartesian3D,
    ]
    .into_iter()
    .map(|srid| match srid {
        Srid::Wgs84_2D | Srid::Wgs84_3D | Srid::Cartesian2D | Srid::Cartesian3D => srid,
    })
    .collect();
    assert_eq!(named, Srid::ALL.to_vec());
}

#[test]
fn null_yields_null_and_scalars_are_syntax_errors() {
    assert_eq!(point_construct(&[Value::Null]).unwrap(), Value::Null);
    for v in [Value::String("foo".into()), Value::Int(42)] {
        let err = point_construct(&[v]).unwrap_err();
        // The detail token the TCK harness derives the kind from is kept;
        // the class is Neo4j's compile-time SyntaxError (see the module
        // doc): a non-map argument is the one point() rejection outside
        // the ArgumentError family.
        let rendered = err.to_string();
        assert!(
            rendered.contains("InvalidArgumentType: point() requires a map, node, or relationship"),
            "unexpected error: {rendered}"
        );
        assert!(matches!(err, Error::Syntax(_)), "{rendered}");
    }
}

#[test]
fn cartesian_requires_both_x_and_y() {
    let (srid, x, y, z) = srid_xyz(&point(&[("x", Value::Int(1)), ("y", Value::Int(2))]).unwrap());
    assert_eq!((srid, x, y, z), (7203, 1.0, 2.0, None));
    // z upgrades to 3D cartesian.
    let (srid, .., z) = srid_xyz(
        &point(&[
            ("x", Value::Float(1.0)),
            ("y", Value::Float(2.0)),
            ("z", Value::Float(3.0)),
        ])
        .unwrap(),
    );
    assert_eq!((srid, z), (9157, Some(3.0)));
    // height is ignored for cartesian points.
    let (srid, .., z) = srid_xyz(
        &point(&[
            ("x", Value::Int(1)),
            ("y", Value::Int(2)),
            ("height", Value::Int(100)),
        ])
        .unwrap(),
    );
    assert_eq!((srid, z), (7203, None));
    // Partial key sets are errors.
    assert!(point(&[("x", Value::Int(1))]).is_err());
    assert!(point(&[("y", Value::Int(1))]).is_err());
    assert!(point(&[]).is_err());
}

#[test]
fn geographic_requires_both_latitude_and_longitude() {
    let (srid, x, y, z) = srid_xyz(
        &point(&[
            ("latitude", Value::Float(59.3293)),
            ("longitude", Value::Float(18.0686)),
        ])
        .unwrap(),
    );
    // longitude is x, latitude is y.
    assert_eq!((srid, x, y, z), (4326, 18.0686, 59.3293, None));
    // height gives 3D geographic.
    let (srid, .., z) = srid_xyz(
        &point(&[
            ("latitude", Value::Int(59)),
            ("longitude", Value::Int(18)),
            ("height", Value::Int(100)),
        ])
        .unwrap(),
    );
    assert_eq!((srid, z), (4979, Some(100.0)));
    // z wins over height when both are present.
    let (srid, .., z) = srid_xyz(
        &point(&[
            ("latitude", Value::Int(59)),
            ("longitude", Value::Int(18)),
            ("z", Value::Int(3)),
            ("height", Value::Int(100)),
        ])
        .unwrap(),
    );
    assert_eq!((srid, z), (4979, Some(3.0)));
    assert!(point(&[("latitude", Value::Int(59))]).is_err());
}

#[test]
fn cartesian_keys_win_over_geographic_keys() {
    // {x, y, latitude, longitude} → cartesian.
    let (srid, x, y, _) = srid_xyz(
        &point(&[
            ("x", Value::Int(1)),
            ("y", Value::Int(2)),
            ("latitude", Value::Int(59)),
            ("longitude", Value::Int(18)),
        ])
        .unwrap(),
    );
    assert_eq!((srid, x, y), (7203, 1.0, 2.0));
    // {x, latitude, longitude} → geographic (no y, so not cartesian).
    let (srid, x, y, _) = srid_xyz(
        &point(&[
            ("x", Value::Int(1)),
            ("latitude", Value::Int(59)),
            ("longitude", Value::Int(18)),
        ])
        .unwrap(),
    );
    assert_eq!((srid, x, y), (4326, 18.0, 59.0));
}

#[test]
fn extra_properties_are_ignored() {
    let (srid, x, y, _) = srid_xyz(
        &point(&[
            ("latitude", Value::Float(59.3293)),
            ("longitude", Value::Float(18.0686)),
            ("address", Value::String("5 Corrie Road".into())),
            ("postcode", Value::String("AB1 2CD".into())),
        ])
        .unwrap(),
    );
    assert_eq!((srid, x, y), (4326, 18.0686, 59.3293));
}

#[test]
fn non_numeric_coordinates_are_errors() {
    assert!(point(&[("x", Value::String("1".into())), ("y", Value::Int(2))]).is_err());
    // A null coordinate is NOT a type error: the whole point is null
    // (Neo4j: "If any argument … is null, null will be returned") — pinned
    // separately by `a_null_coordinate_makes_the_point_null`.
    assert!(point(&[
        ("latitude", Value::Int(59)),
        ("longitude", Value::Int(18)),
        ("height", Value::String("100".into())),
    ])
    .is_err());
}

#[test]
fn non_finite_coordinates_are_errors() {
    // Neo4j-verified (22N24): NaN and ±Infinity in any coordinate error.
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        // Cartesian family: x, y, z.
        assert!(point(&[("x", Value::Float(bad)), ("y", Value::Int(1))]).is_err());
        assert!(point(&[("x", Value::Int(1)), ("y", Value::Float(bad))]).is_err());
        assert!(point(&[
            ("x", Value::Int(1)),
            ("y", Value::Int(2)),
            ("z", Value::Float(bad)),
        ])
        .is_err());
        // Geographic family: longitude, latitude, z, height.
        assert!(point(&[
            ("latitude", Value::Int(59)),
            ("longitude", Value::Float(bad)),
        ])
        .is_err());
        assert!(point(&[
            ("latitude", Value::Float(bad)),
            ("longitude", Value::Int(18)),
        ])
        .is_err());
        assert!(point(&[
            ("latitude", Value::Int(59)),
            ("longitude", Value::Int(18)),
            ("z", Value::Float(bad)),
        ])
        .is_err());
        assert!(point(&[
            ("latitude", Value::Int(59)),
            ("longitude", Value::Int(18)),
            ("height", Value::Float(bad)),
        ])
        .is_err());
    }
    // The error is an InvalidArgumentValue naming the coordinate key.
    let err = point(&[("x", Value::Float(f64::NAN)), ("y", Value::Int(1))])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("InvalidArgumentValue: point() coordinate 'x' must be finite"),
        "unexpected error: {err}"
    );
}

#[test]
fn height_is_type_checked_even_when_z_wins() {
    // Neo4j-verified (22N27): height is validated eagerly, so a
    // non-numeric height errors even though a valid z takes precedence.
    let err = point(&[
        ("latitude", Value::Float(59.0)),
        ("longitude", Value::Float(18.0)),
        ("z", Value::Float(3.0)),
        ("height", Value::String("abc".into())),
    ])
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("InvalidArgumentType: point() coordinate 'height' must be a float or integer"),
        "unexpected error: {err}"
    );
}

#[test]
fn srid_must_be_a_known_integer() {
    // Float and string srids are errors (previously silently ignored).
    assert!(point(&[
        ("x", Value::Int(1)),
        ("y", Value::Int(2)),
        ("srid", Value::Float(4326.0)),
    ])
    .is_err());
    assert!(point(&[
        ("x", Value::Int(1)),
        ("y", Value::Int(2)),
        ("srid", Value::String("7203".into())),
    ])
    .is_err());
    // Unknown code.
    assert!(point(&[
        ("x", Value::Int(1)),
        ("y", Value::Int(2)),
        ("srid", Value::Int(9999)),
    ])
    .is_err());
}

#[test]
fn srid_must_match_dimension_and_family() {
    // 2D srid with z present.
    assert!(point(&[
        ("x", Value::Int(1)),
        ("y", Value::Int(2)),
        ("z", Value::Int(3)),
        ("srid", Value::Int(7203)),
    ])
    .is_err());
    // 3D srid without z.
    assert!(point(&[
        ("x", Value::Int(1)),
        ("y", Value::Int(2)),
        ("srid", Value::Int(9157)),
    ])
    .is_err());
    // Geographic keys with a cartesian srid.
    assert!(point(&[
        ("latitude", Value::Int(59)),
        ("longitude", Value::Int(18)),
        ("srid", Value::Int(7203)),
    ])
    .is_err());
    // 3D geographic srid without z/height.
    assert!(point(&[
        ("latitude", Value::Int(59)),
        ("longitude", Value::Int(18)),
        ("srid", Value::Int(4979)),
    ])
    .is_err());
    // Cartesian keys accept any of the four srids, dimension-checked.
    let (srid, x, y, z) = srid_xyz(
        &point(&[
            ("x", Value::Int(1)),
            ("y", Value::Int(2)),
            ("srid", Value::Int(4326)),
        ])
        .unwrap(),
    );
    assert_eq!((srid, x, y, z), (4326, 1.0, 2.0, None));
    let (srid, ..) = srid_xyz(
        &point(&[
            ("x", Value::Int(1)),
            ("y", Value::Int(2)),
            ("z", Value::Int(3)),
            ("srid", Value::Int(9157)),
        ])
        .unwrap(),
    );
    assert_eq!(srid, 9157);
    // Geographic keys with an explicit geographic srid.
    let (srid, ..) = srid_xyz(
        &point(&[
            ("latitude", Value::Int(59)),
            ("longitude", Value::Int(18)),
            ("srid", Value::Int(4326)),
        ])
        .unwrap(),
    );
    assert_eq!(srid, 4326);
}

#[test]
fn negative_multiple_of_360_wraps_to_the_negative_endpoint() {
    // Spatial [3] @1.12: Neo4j keeps -540 at -180, not +180.
    assert_eq!(wrap_longitude(-540.0), -180.0);
    assert_eq!(wrap_longitude(540.0), 180.0);
    assert_eq!(wrap_longitude(-180.0), -180.0);
    assert_eq!(wrap_longitude(180.0), 180.0);
    // -360 % 360 is -0.0 in Rust; Neo4j normalizes it to +0.0.
    assert!(wrap_longitude(-360.0).is_sign_positive());
    // Extreme longitudes stay finite and in range.
    for lon in [1e30, -1e30, 1e300, -1e300, f64::MAX, -f64::MAX] {
        let x = wrap_longitude(lon);
        assert!(
            x.is_finite() && (-180.0..=180.0).contains(&x),
            "longitude {lon} wrapped to {x}"
        );
    }
}

#[test]
fn cartesian_distance_survives_coordinates_that_overflow_when_squared() {
    // Spatial [6] @1.6: squaring 2e154 overflows; Neo4j scales first.
    let Value::Float(d) = distance_of(
        Value::Point(PointValue::cartesian_2d(1.0e154, 0.0)),
        Value::Point(PointValue::cartesian_2d(-1.0e154, 0.0)),
    ) else {
        panic!("expected float distance")
    };
    assert_eq!(d, 2.0e154);
    // The plain cases keep their exact answers.
    let Value::Float(d) = distance_of(
        Value::Point(PointValue::cartesian_2d(0.0, 0.0)),
        Value::Point(PointValue::cartesian_2d(3.0, 4.0)),
    ) else {
        panic!("expected float distance")
    };
    assert_eq!(d, 5.0);
    let Value::Float(d) = distance_of(
        Value::Point(PointValue::cartesian_3d(0.0, 0.0, 0.0)),
        Value::Point(PointValue::cartesian_3d(0.0, 0.0, 2.0)),
    ) else {
        panic!("expected float distance")
    };
    assert_eq!(d, 2.0);
}

#[test]
fn distance_is_null_for_dimension_and_crs_mismatches() {
    // Spatial [8] @1.4: 2D vs 3D.
    assert_eq!(
        distance_of(
            Value::Point(PointValue::cartesian_2d(1.0, 2.0)),
            Value::Point(PointValue::cartesian_3d(1.0, 2.0, 3.0))
        ),
        Value::Null
    );
    // Spatial [8] @1.5: WGS-84 vs cartesian.
    assert_eq!(
        distance_of(
            geo(1.0, 2.0),
            Value::Point(PointValue::cartesian_2d(1.0, 2.0))
        ),
        Value::Null
    );
    // Same CRS still measures.
    assert_eq!(
        distance_of(
            Value::Point(PointValue::cartesian_2d(1.0, 2.0)),
            Value::Point(PointValue::cartesian_2d(1.0, 2.0))
        ),
        Value::Float(0.0)
    );
}

#[test]
fn geographic_distance_handles_extreme_heights() {
    // Spatial [7] @1.4: the height axis is orthogonal to the surface
    // distance and must not overflow.
    let Value::Float(d) = distance_of(
        Value::Point(PointValue::wgs84_3d(12.0, 55.0, 1.0e308)),
        Value::Point(PointValue::wgs84_3d(12.0, 55.0, 0.0)),
    ) else {
        panic!("expected float distance")
    };
    assert_eq!(d, 1.0e308);
    // Two equal extreme heights are zero apart, not NaN/Infinity.
    let Value::Float(d) = distance_of(
        Value::Point(PointValue::wgs84_3d(0.0, 0.0, 1.0e308)),
        Value::Point(PointValue::wgs84_3d(0.0, 0.0, 1.0e308)),
    ) else {
        panic!("expected float distance")
    };
    assert_eq!(d, 0.0);
}

#[test]
fn bbox_wraps_across_the_antimeridian() {
    // Spatial [9] @1.5: 179 → -179 wraps across ±180.
    let lo = geo(179.0, 55.66);
    let hi = geo(-179.0, 55.70);
    assert_eq!(
        point_within_bbox(&[geo(180.0, 55.66), lo.clone(), hi.clone()]).unwrap(),
        Value::Bool(true)
    );
    // A point on the non-wrapping side of the box is outside.
    assert_eq!(
        point_within_bbox(&[geo(0.0, 55.68), lo, hi]).unwrap(),
        Value::Bool(false)
    );
    // A non-wrapping box still behaves as before.
    assert_eq!(
        point_within_bbox(&[
            Value::Point(PointValue::cartesian_2d(5.0, 5.0)),
            Value::Point(PointValue::cartesian_2d(0.0, 0.0)),
            Value::Point(PointValue::cartesian_2d(10.0, 10.0))
        ])
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn bbox_is_null_for_dimension_and_crs_mismatches() {
    // Spatial [10] @1.4: 2D point against a 3D box.
    assert_eq!(
        point_within_bbox(&[
            Value::Point(PointValue::cartesian_2d(1.0, 2.0)),
            Value::Point(PointValue::cartesian_3d(0.0, 0.0, 0.0)),
            Value::Point(PointValue::cartesian_3d(10.0, 10.0, 10.0))
        ])
        .unwrap(),
        Value::Null
    );
    // WGS-84 vs cartesian.
    assert_eq!(
        point_within_bbox(&[
            geo(1.0, 2.0),
            Value::Point(PointValue::cartesian_2d(0.0, 0.0)),
            Value::Point(PointValue::cartesian_2d(10.0, 10.0))
        ])
        .unwrap(),
        Value::Null
    );
    // Null arguments stay null.
    assert_eq!(
        point_within_bbox(&[
            Value::Null,
            Value::Point(PointValue::cartesian_2d(0.0, 0.0)),
            Value::Point(PointValue::cartesian_2d(10.0, 10.0))
        ])
        .unwrap(),
        Value::Null
    );
}

#[test]
fn pythagoras_propagates_nan_axis_differences() {
    // `f64::max` ignores NaN; Java's `Math.max` propagates it, so a NaN
    // axis difference must yield NaN instead of a fabricated zero.
    assert!(pythagoras(&[f64::NAN, 0.0], &[0.0, 0.0]).is_nan());
    assert!(pythagoras(&[1.0, f64::NAN], &[1.0, 0.0]).is_nan());
    assert!(pythagoras(&[f64::NAN, f64::NAN], &[0.0, 0.0]).is_nan());
}

#[test]
fn inconsistent_stored_dimension_is_an_error() {
    // The load surface takes the srid from `SRID=` and the coordinate arity
    // from the `POINT Z` marker without cross-checking, so a stored point
    // can claim 3D with no height. `point()` never produces this, and the
    // predicates surface it instead of answering as planar or with an
    // unconstrained height axis.
    let flat_3d = Value::Point(PointValue {
        srid: 9157,
        x: 1.0,
        y: 2.0,
        z: None,
    });
    let origin = Value::Point(PointValue::cartesian_3d(0.0, 0.0, 0.0));
    let corner = Value::Point(PointValue::cartesian_3d(10.0, 10.0, 10.0));
    let err = point_distance(&[flat_3d.clone(), origin.clone()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("InvalidPoint"), "unexpected error: {err}");
    let err = point_within_bbox(&[flat_3d, origin, corner])
        .unwrap_err()
        .to_string();
    assert!(err.contains("InvalidPoint"), "unexpected error: {err}");
    // A 2D srid carrying a height is the same violation.
    let tall_2d = Value::Point(PointValue {
        srid: 7203,
        x: 1.0,
        y: 2.0,
        z: Some(3.0),
    });
    assert!(point_distance(&[tall_2d, Value::Point(PointValue::cartesian_2d(0.0, 0.0))]).is_err());
}

/// Neo4j: a null in any coordinate key makes the whole point null ("If any
/// argument … is null, null will be returned") — not a type error.
#[test]
fn a_null_coordinate_makes_the_point_null() {
    use crate::test_support::value_map;
    assert_eq!(
        point_construct(&[value_map(&[("x", Value::Int(1)), ("y", Value::Null)])]).unwrap(),
        Value::Null
    );
    assert_eq!(
        point_construct(&[value_map(&[
            ("longitude", Value::Float(-5.0)),
            ("latitude", Value::Float(45.0)),
            ("height", Value::Null),
        ])])
        .unwrap(),
        Value::Null
    );
}

/// A non-point argument is not a fault: live answers null for it, as it does
/// for a null argument and for a pair whose coordinate-reference families
/// disagree (live 2026.10.1: an integer, a string, a list, a bare map and a
/// cartesian/geographic pair each return null).
#[test]
fn a_non_point_argument_yields_null_not_an_error() {
    let cartesian = |x: f64, y: f64| {
        point(&[
            ("x", Value::Float(x)),
            ("y", Value::Float(y)),
            ("crs", Value::String("cartesian".into())),
        ])
        .unwrap()
    };
    let origin = cartesian(0.0, 0.0);
    for other in [
        Value::Int(5),
        Value::String("nope".into()),
        Value::List(vec![Value::Int(1), Value::Int(2)].into()),
        geo(0.0, 0.0),
    ] {
        assert_eq!(
            distance_of(origin.clone(), other.clone()),
            Value::Null,
            "{other:?}"
        );
        assert_eq!(distance_of(other, origin.clone()), Value::Null);
    }
    // The distance itself is untouched.
    assert_eq!(distance_of(origin, cartesian(3.0, 4.0)), Value::Float(5.0));
}
