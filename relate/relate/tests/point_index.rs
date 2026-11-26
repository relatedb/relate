//! POINT index contract through the public Rust API (ADR-0549): indexed
//! bbox and radius seeks answer through the R-tree postings, and a
//! file-backed reopen keeps them correct (postings are derived state,
//! rebuilt from the replayed graph at open).

mod common;

use common::{connection, first_column_ints, list, map, params, run, scalar_one};
use relate::{Connection, Database, Value};
use relate_test_support::uri;

/// Haversine ground truth's radius, the one the engine's residual uses
/// (`relate_executor::spatial::EARTH_RADIUS_METERS` is crate-private, so
/// this test crate carries the canonical copy once).
const EARTH_RADIUS_METERS: f64 = 6_378_140.0;

fn near_point_queries(c: &mut Connection) {
    let rows = c
        .query(
            "MATCH (n:P) WHERE point.withinBBox(n.loc, point({x: 0, y: 0}), point({x: 2, y: 2})) \
             RETURN n.id AS id",
        )
        .unwrap();
    assert_eq!(rows.rows, vec![vec![Value::String("near".into())]]);
    let rows = c
        .query(
            "MATCH (n:P) WHERE point.distance(n.loc, point({x: 0, y: 0})) < 2.0 \
             RETURN n.id AS id",
        )
        .unwrap();
    assert_eq!(rows.rows, vec![vec![Value::String("near".into())]]);
}

#[test]
fn point_index_answers_bbox_and_radius() {
    let mut c = Database::open(":memory:").unwrap().connect().unwrap();
    c.query(
        "CREATE (:P {id: 'near', loc: point({x: 1.0, y: 1.0})}), \
                (:P {id: 'far', loc: point({x: 9.0, y: 9.0})})",
    )
    .unwrap();
    c.query("CREATE POINT INDEX locIndex FOR (n:P) ON (n.loc)")
        .unwrap();
    near_point_queries(&mut c);
}

#[test]
fn point_index_seek_correct_after_file_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(uri(dir.path())).unwrap();
        let mut c = db.connect().unwrap();
        c.query(
            "CREATE (:P {id: 'near', loc: point({x: 1.0, y: 1.0})}), \
                    (:P {id: 'far', loc: point({x: 50.0, y: 50.0})})",
        )
        .unwrap();
        c.query("CREATE POINT INDEX locIndex FOR (n:P) ON (n.loc)")
            .unwrap();
    }
    let db = Database::open(uri(dir.path())).unwrap();
    let mut c = db.connect().unwrap();
    near_point_queries(&mut c);
}

fn ids(c: &mut Connection, statement: &str) -> Vec<i64> {
    first_column_ints(&run(c, statement))
}

/// The single-row integer `count(…)` of `statement`, which must succeed.
fn count(c: &mut Connection, statement: &str) -> i64 {
    scalar_one(c, statement).as_int().expect("count")
}

/// The corpus distance predicate: `point.distance(n.geo, origin) [op] radius`.
fn distance_seek(origin: &str, radius: f64, op: &str) -> String {
    format!("point.distance(n.geo, {origin}) {op} {radius}")
}

/// [`distance_seek`] with `+ 0`: the arithmetic defeats the POINT-index
/// seek, so the general path answers — the control beside an indexed run.
fn distance_scan(origin: &str, radius: f64, op: &str) -> String {
    format!("point.distance(n.geo, {origin}) + 0 {op} {radius}")
}

fn create_corpus_graph(c: &mut Connection) {
    run(
        c,
        "CREATE (:A {id: 1, geo: point({longitude: 0, latitude: 89})}), \
                (:A {id: 2, geo: point({longitude: 0, latitude: 70})}), \
                (:A {id: 3, geo: point({longitude: 0, latitude: 0})})",
    );
}

/// The corpus table, haversine ground truth (README of finding 41). The
/// 11,021 km distance of the 89°N point from (0°,-10°) is past a quarter
/// circumference; a POINT index used to answer only 2 and 3 there.
#[test]
fn wgs84_distance_seek_past_a_quarter_circumference_matches_the_scan() {
    let mut c = connection();
    create_corpus_graph(&mut c);
    let cases: &[(&str, f64, &str, &[i64])] = &[
        (
            "point({longitude: 0, latitude: -10})",
            12_000_000.0,
            "<",
            &[1, 2, 3],
        ),
        (
            "point({longitude: 0, latitude: -10})",
            12_000_000.0,
            "<=",
            &[1, 2, 3],
        ),
        (
            "point({longitude: 0, latitude: -60})",
            19_000_000.0,
            "<",
            &[1, 2, 3],
        ),
        // Below a quarter circumference the band still prunes correctly.
        (
            "point({longitude: 0, latitude: -10})",
            10_000_000.0,
            "<",
            &[2, 3],
        ),
        (
            "point({longitude: 0, latitude: 10})",
            12_000_000.0,
            "<",
            &[1, 2, 3],
        ),
    ];
    let statement =
        |predicate: String| format!("MATCH (n:A) WHERE {predicate} RETURN n.id AS id ORDER BY id");
    for (origin, radius, op, want) in cases {
        let stmt = statement(distance_scan(origin, *radius, op));
        assert_eq!(ids(&mut c, &stmt), *want, "scan: {stmt}");
    }
    run(&mut c, "CREATE POINT INDEX geo41 FOR (n:A) ON (n.geo)");
    for (origin, radius, op, want) in cases {
        let indexed = statement(distance_seek(origin, *radius, op));
        assert_eq!(ids(&mut c, &indexed), *want, "seek: {indexed}");
        let control = statement(distance_scan(origin, *radius, op));
        assert_eq!(ids(&mut c, &control), *want, "control (no seek): {control}");
    }
}

/// Haversine ground truth over the same radius constants the engine's
/// residual uses — the oracle for this class is the formula, not live
/// Neo4j (which disagrees with its own scans past a quarter
/// circumference).
fn haversine_m(a_lon: f64, a_lat: f64, b_lon: f64, b_lat: f64) -> f64 {
    let (lat1, lat2) = (a_lat.to_radians(), b_lat.to_radians());
    let alpha = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * ((b_lon - a_lon).to_radians() / 2.0).sin().powi(2);
    EARTH_RADIUS_METERS * 2.0 * alpha.sqrt().atan2((1.0 - alpha).sqrt())
}

/// The corpus's 400-point scan shape: two fifths of the points sit near a
/// pole, and radii reach half a circumference. The indexed seek must
/// answer exactly the haversine count.
#[test]
fn wgs84_distance_seek_matches_haversine_up_to_half_a_circumference() {
    use relate_test_support::rng::Rng;
    let mut rng = Rng(0x5eed_0041);
    let mut pts = Vec::new();
    for i in 0..400 {
        let lon = (rng.unit() * 2.0 - 1.0) * 180.0;
        let lat = if i % 5 < 2 {
            let edge = rng.unit() * 5.0;
            if i % 2 == 0 {
                90.0 - edge
            } else {
                -90.0 + edge
            }
        } else {
            (rng.unit() * 2.0 - 1.0) * 90.0
        };
        pts.push((lon, lat));
    }
    let mut c = connection();
    let points = list(
        pts.iter()
            .map(|(lon, lat)| {
                map(&[
                    ("longitude", Value::Float(*lon)),
                    ("latitude", Value::Float(*lat)),
                ])
            })
            .collect(),
    );
    c.query_with_params(
        "UNWIND $points AS p \
         CREATE (:G {geo: point({longitude: p.longitude, latitude: p.latitude})})",
        params(&[("points", points)]),
    )
    .unwrap();
    run(&mut c, "CREATE POINT INDEX g41 FOR (n:G) ON (n.geo)");
    let half = std::f64::consts::PI * EARTH_RADIUS_METERS;
    for _ in 0..20 {
        let (olon, olat) = (
            (rng.unit() * 2.0 - 1.0) * 180.0,
            (rng.unit() * 2.0 - 1.0) * 90.0,
        );
        let radius = rng.unit() * half;
        let want = pts
            .iter()
            .filter(|(lon, lat)| haversine_m(olon, olat, *lon, *lat) < radius)
            .count() as i64;
        let origin = format!("point({{longitude: {olon:.9}, latitude: {olat:.9}}})");
        let seek = format!(
            "MATCH (n:G) WHERE {} RETURN count(*) AS c",
            distance_seek(&origin, radius, "<")
        );
        assert_eq!(count(&mut c, &seek), want, "seek: {seek}");
        let scan = format!(
            "MATCH (n:G) WHERE {} RETURN count(*) AS c",
            distance_scan(&origin, radius, "<")
        );
        assert_eq!(count(&mut c, &scan), want, "scan control: {scan}");
    }
}
