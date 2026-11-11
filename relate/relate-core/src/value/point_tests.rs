//! Point-accessor tests: the axis vocabulary, the unknown-component
//! rejection, and live Neo4j's per-point availability rule — split from
//! `value/tests.rs` for the file-size rule (the `depth_tests.rs`
//! convention).

use super::*;

/// C7: the point accessor answers the same typed `no such component`
/// rejection as the temporal accessor (never a silently null read), and both
/// coordinate spellings of one axis read the same coordinate — the shared
/// axis vocabulary beside `PointValue`.
#[test]
fn point_component_reads_axis_aliases_and_rejects_unknown_keys() {
    let p = PointValue::wgs84_3d(-73.9857, 40.7484, 8.0);
    assert_eq!(point_component(&p, "x").unwrap(), Value::Float(-73.9857));
    assert_eq!(
        point_component(&p, "longitude").unwrap(),
        Value::Float(-73.9857)
    );
    assert_eq!(point_component(&p, "y").unwrap(), Value::Float(40.7484));
    assert_eq!(
        point_component(&p, "latitude").unwrap(),
        Value::Float(40.7484)
    );
    assert_eq!(point_component(&p, "z").unwrap(), Value::Float(8.0));
    assert_eq!(point_component(&p, "height").unwrap(), Value::Float(8.0));
    assert_eq!(
        point_component(&p, "crs").unwrap(),
        Value::String("wgs-84-3d".into())
    );
    assert_eq!(point_component(&p, "srid").unwrap(), Value::Int(4979));
    // 22G03: an unknown component is an invalid-argument rejection.
    assert_eq!(
        point_component(&p, "foo").unwrap_err(),
        Error::argument_error("no such component 'foo' on a point value")
    );
}

/// Live Neo4j 2026.10.1's availability rule (probed 2026-11-11): a
/// cartesian point exposes only the cartesian spellings, a geographic point
/// accepts both spellings of each axis, and the third axis exists only on a
/// 3D point. An unavailable spelling is an `ArgumentError` rejection, never
/// a null read.
#[test]
fn point_component_rejects_spellings_the_point_does_not_have() {
    let cart2d = PointValue::cartesian_2d(1.0, 2.0);
    // Geographic spellings on a cartesian point: the `ArgumentError` class
    // the doc names, not just any error.
    for key in ["longitude", "latitude", "height"] {
        assert!(
            matches!(point_component(&cart2d, key), Err(Error::ArgumentError(_))),
            "cartesian 2D / {key}"
        );
    }
    // The third axis on a 2D point.
    assert!(matches!(
        point_component(&cart2d, "z"),
        Err(Error::ArgumentError(_))
    ));
    assert_eq!(point_component(&cart2d, "x").unwrap(), Value::Float(1.0));
    assert_eq!(point_component(&cart2d, "y").unwrap(), Value::Float(2.0));

    let cart3d = PointValue::cartesian_3d(1.0, 2.0, 3.0);
    assert_eq!(point_component(&cart3d, "z").unwrap(), Value::Float(3.0));
    assert!(matches!(
        point_component(&cart3d, "latitude"),
        Err(Error::ArgumentError(_))
    ));

    // A geographic point accepts both spellings of a present axis.
    let geo2d = PointValue::wgs84_2d(1.0, 2.0);
    assert_eq!(point_component(&geo2d, "x").unwrap(), Value::Float(1.0));
    assert_eq!(
        point_component(&geo2d, "longitude").unwrap(),
        Value::Float(1.0)
    );
    assert_eq!(
        point_component(&geo2d, "latitude").unwrap(),
        Value::Float(2.0)
    );
    for key in ["z", "height"] {
        assert!(
            matches!(point_component(&geo2d, key), Err(Error::ArgumentError(_))),
            "geographic 2D / {key}"
        );
    }

    let geo3d = PointValue::wgs84_3d(1.0, 2.0, 3.0);
    assert_eq!(point_component(&geo3d, "z").unwrap(), Value::Float(3.0));
    assert_eq!(
        point_component(&geo3d, "height").unwrap(),
        Value::Float(3.0)
    );
}
