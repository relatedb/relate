//! The typed value walker (ADR-0502 §1): kinds at every depth, component
//! accessors by kind, result-owned handles, and the defined out-of-kind
//! refusal.

use std::ffi::CString;
use std::ptr;

use crate::abi::relate_status;
use crate::result::*;
use crate::support::{cell, component, int_of, run, str_of, text, Session};
use crate::tabular::*;
use crate::value::relate_value_kind::*;
use crate::value::*;

/// The kind of `v`, asserted to be an enumerator.
///
/// # Safety
/// `v` must be null or a live handle.
unsafe fn kind(v: *const relate_value) -> i32 {
    unsafe { relate_value_kind(v) }
}

/// A FLOAT handle's value, asserted in-kind.
///
/// # Safety
/// `v` must be a live handle.
unsafe fn float_of(v: *const relate_value) -> f64 {
    let mut out = 0.0;
    assert!(matches!(
        unsafe { relate_value_float(v, &mut out) },
        relate_status::RELATE_OK
    ));
    out
}

/// The value under `key` of a map-shaped handle, asserted present.
///
/// # Safety
/// `v` must be a live handle.
unsafe fn get(v: *const relate_value, key: &str) -> *const relate_value {
    let k = CString::new(key).unwrap();
    let child = unsafe { relate_value_map_get(v, k.as_ptr()) };
    assert!(!child.is_null(), "key {key} is present");
    child
}

/// Every kind enumerator, in ABI order.
const ALL_KINDS: [relate_value_kind; 18] = [
    RELATE_V_NULL,
    RELATE_V_BOOL,
    RELATE_V_INT,
    RELATE_V_FLOAT,
    RELATE_V_STRING,
    RELATE_V_NODE,
    RELATE_V_RELATIONSHIP,
    RELATE_V_PATH,
    RELATE_V_LIST,
    RELATE_V_MAP,
    RELATE_V_POINT,
    RELATE_V_DATE,
    RELATE_V_TIME,
    RELATE_V_LOCAL_TIME,
    RELATE_V_DATETIME,
    RELATE_V_LOCAL_DATETIME,
    RELATE_V_DURATION,
    RELATE_V_VECTOR,
];

/// The header's `enum relate_value_kind` and the Rust enum agree name
/// for name and value for value — the C mirror cannot drift.
#[test]
fn header_kind_enum_mirrors_the_rust_enum() {
    let header = include_str!("../../include/relate.h");
    let body = header
        .split("enum relate_value_kind {")
        .nth(1)
        .and_then(|rest| rest.split("};").next())
        .expect("the header declares enum relate_value_kind");
    let declared: Vec<(String, i32)> = body
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (name, value) = entry.split_once('=').expect("an explicit value");
            (name.trim().to_string(), value.trim().parse().unwrap())
        })
        .collect();
    let rust: Vec<(String, i32)> = ALL_KINDS
        .iter()
        .map(|k| (format!("{k:?}"), *k as i32))
        .collect();
    assert_eq!(declared, rust);
}

/// ADR-0502 §1 acceptance: every value keeps its kind and components at
/// every depth — a date inside a node property, `duration('P1Y')` inside a
/// list (months, not a string), a path inside a list, a user map shaped
/// like a node staying a MAP, a named-zone datetime keeping its zone id,
/// NaN nested in a map.
#[test]
fn walker_round_trips_kinds_at_every_depth() {
    // SAFETY: test drives the FFI with handles it owns; every value handle
    // is read before its result is destroyed.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let created = run(
            conn,
            "CREATE (:Person {name: 'Ada', born: date('1815-12-10')})\
             -[:KNOWS {since: 1833}]->(:Person {name: 'Babbage'})",
        );
        relate_destroy_result(created);
        let result = run(
            conn,
            "MATCH p = (a:Person {name: 'Ada'})-[:KNOWS]->(:Person) \
             RETURN a, [duration('P1Y'), 1] AS durations, [p] AS paths, \
             {id: 7, labels: ['Person'], properties: {name: 'x'}} AS fake, \
             datetime('2024-03-14T12:30:45.123456789[Europe/Stockholm]') AS zoned, \
             {nan: 0.0 / 0.0, inf: -1.0 / 0.0, nested: {deep: [0.0 / 0.0]}} AS floats",
        );

        // A date inside a node property.
        let a = cell(result, 0, 0);
        assert_eq!(kind(a), RELATE_V_NODE as i32);
        assert_eq!(
            text(relate_value_element_id(a)),
            "4:rl-id:n0",
            "the engine's elementId() spelling"
        );
        assert_eq!(relate_value_list_len(a), 1);
        assert_eq!(str_of(relate_value_list_get(a, 0)), "Person");
        assert_eq!(relate_value_map_len(a), 2);
        let born = get(a, "born");
        assert_eq!(kind(born), RELATE_V_DATE as i32);
        assert_eq!(component(relate_value_year, born), 1815);
        assert_eq!(component(relate_value_month, born), 12);
        assert_eq!(component(relate_value_day, born), 10);
        assert_eq!(str_of(get(a, "name")), "Ada");

        // duration('P1Y') inside a list: months readable, never a string.
        let durations = cell(result, 0, 1);
        assert_eq!(kind(durations), RELATE_V_LIST as i32);
        assert_eq!(relate_value_list_len(durations), 2);
        let year = relate_value_list_get(durations, 0);
        assert_eq!(kind(year), RELATE_V_DURATION as i32);
        assert!(relate_value_string(year, ptr::null_mut()).is_null());
        let (mut months, mut days, mut seconds, mut nanos) = (-1, -1, -1, -1);
        assert!(matches!(
            relate_value_duration(year, &mut months, &mut days, &mut seconds, &mut nanos),
            relate_status::RELATE_OK
        ));
        assert_eq!((months, days, seconds, nanos), (12, 0, 0, 0));
        assert_eq!(int_of(relate_value_list_get(durations, 1)), 1);

        // A path inside a list: alternating node, relationship, node.
        let paths = cell(result, 0, 2);
        assert_eq!(kind(paths), RELATE_V_LIST as i32);
        let path = relate_value_list_get(paths, 0);
        assert_eq!(kind(path), RELATE_V_PATH as i32);
        assert_eq!(relate_value_list_len(path), 3);
        let (start, rel, end) = (
            relate_value_list_get(path, 0),
            relate_value_list_get(path, 1),
            relate_value_list_get(path, 2),
        );
        assert!(relate_value_list_get(path, 3).is_null());
        assert_eq!(kind(start), RELATE_V_NODE as i32);
        assert_eq!(kind(rel), RELATE_V_RELATIONSHIP as i32);
        assert_eq!(kind(end), RELATE_V_NODE as i32);
        assert_eq!(text(relate_value_element_id(start)), "4:rl-id:n0");
        assert_eq!(text(relate_value_element_id(end)), "4:rl-id:n1");
        assert_eq!(text(relate_value_element_id(rel)), "5:rl-id:r0");
        assert_eq!(text(relate_value_start_element_id(rel)), "4:rl-id:n0");
        assert_eq!(text(relate_value_end_element_id(rel)), "4:rl-id:n1");
        assert_eq!(str_of(rel), "KNOWS");
        assert_eq!(int_of(get(rel, "since")), 1833);
        assert_eq!(str_of(get(end, "name")), "Babbage");
        // The nested path's date property is still a DATE.
        assert_eq!(kind(get(start, "born")), RELATE_V_DATE as i32);

        // A user map shaped like a node stays a MAP.
        let fake = cell(result, 0, 3);
        assert_eq!(kind(fake), RELATE_V_MAP as i32);
        assert!(relate_value_element_id(fake).is_null());
        assert_eq!(relate_value_list_len(fake), -1);
        assert_eq!(relate_value_map_len(fake), 3);
        let keys: Vec<String> = (0..3)
            .map(|i| text(relate_value_map_key(fake, i, ptr::null_mut())))
            .collect();
        assert_eq!(keys, ["id", "labels", "properties"]);
        assert_eq!(kind(get(fake, "labels")), RELATE_V_LIST as i32);
        assert_eq!(kind(get(fake, "properties")), RELATE_V_MAP as i32);

        // A named-zone datetime keeps its zone id beside its components.
        let zoned = cell(result, 0, 4);
        assert_eq!(kind(zoned), RELATE_V_DATETIME as i32);
        assert_eq!(text(relate_value_zone(zoned)), "Europe/Stockholm");
        assert_eq!(component(relate_value_offset_seconds, zoned), 3600);
        assert_eq!(component(relate_value_year, zoned), 2024);
        assert_eq!(component(relate_value_month, zoned), 3);
        assert_eq!(component(relate_value_day, zoned), 14);
        assert_eq!(component(relate_value_hour, zoned), 12);
        assert_eq!(component(relate_value_minute, zoned), 30);
        assert_eq!(component(relate_value_second, zoned), 45);
        assert_eq!(component(relate_value_nanosecond, zoned), 123_456_789);

        // NaN nested in a map — and in a list in a map.
        let floats = cell(result, 0, 5);
        assert_eq!(kind(floats), RELATE_V_MAP as i32);
        let (mut is_nan, mut is_inf) = (9u8, 9u8);
        let nan = get(floats, "nan");
        assert_eq!(kind(nan), RELATE_V_FLOAT as i32);
        assert!(matches!(
            relate_value_is_nan(nan, &mut is_nan),
            relate_status::RELATE_OK
        ));
        assert_eq!(is_nan, 1);
        assert!(float_of(nan).is_nan());
        let inf = get(floats, "inf");
        assert!(matches!(
            relate_value_is_inf(inf, &mut is_inf),
            relate_status::RELATE_OK
        ));
        assert_eq!(is_inf, 1);
        assert_eq!(float_of(inf), f64::NEG_INFINITY);
        assert!(matches!(
            relate_value_is_nan(inf, &mut is_nan),
            relate_status::RELATE_OK
        ));
        assert_eq!(is_nan, 0);
        let deep = relate_value_list_get(get(get(floats, "nested"), "deep"), 0);
        assert!(float_of(deep).is_nan());

        relate_destroy_result(result);
    }
}

/// The remaining temporal kinds: TIME (offset, zone ""), LOCAL_TIME and
/// LOCAL_DATETIME (no offset, no zone), an offset-only DATETIME (zone "").
#[test]
fn walker_reads_every_temporal_kind_as_components() {
    // SAFETY: test drives the FFI with handles it owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let result = run(
            conn,
            "RETURN time('10:11:12.5+01:00') AS t, localtime('03:04:05') AS lt, \
             localdatetime('2024-01-02T03:04:05.000000006') AS ldt, \
             datetime('2024-03-14T12:00:00+02:00') AS offset_only, \
             duration({days: 2, seconds: 3, nanoseconds: 4}) AS d",
        );
        let t = cell(result, 0, 0);
        assert_eq!(kind(t), RELATE_V_TIME as i32);
        assert_eq!(component(relate_value_hour, t), 10);
        assert_eq!(component(relate_value_nanosecond, t), 500_000_000);
        assert_eq!(component(relate_value_offset_seconds, t), 3600);
        assert_eq!(text(relate_value_zone(t)), "");
        let mut out = -7;
        assert!(matches!(
            relate_value_year(t, &mut out),
            relate_status::RELATE_ERROR
        ));
        assert_eq!(out, -7, "a refusal leaves the out-param untouched");

        let lt = cell(result, 0, 1);
        assert_eq!(kind(lt), RELATE_V_LOCAL_TIME as i32);
        assert_eq!(component(relate_value_second, lt), 5);
        assert!(matches!(
            relate_value_offset_seconds(lt, &mut out),
            relate_status::RELATE_ERROR
        ));
        assert!(relate_value_zone(lt).is_null());

        let ldt = cell(result, 0, 2);
        assert_eq!(kind(ldt), RELATE_V_LOCAL_DATETIME as i32);
        assert_eq!(component(relate_value_day, ldt), 2);
        assert_eq!(component(relate_value_nanosecond, ldt), 6);
        assert!(relate_value_zone(ldt).is_null());

        let offset_only = cell(result, 0, 3);
        assert_eq!(kind(offset_only), RELATE_V_DATETIME as i32);
        assert_eq!(text(relate_value_zone(offset_only)), "");
        assert_eq!(component(relate_value_offset_seconds, offset_only), 7200);
        assert_eq!(component(relate_value_hour, offset_only), 12);

        let d = cell(result, 0, 4);
        let (mut months, mut days, mut seconds, mut nanos) = (-1, -1, -1, -1);
        assert!(matches!(
            relate_value_duration(d, &mut months, &mut days, &mut seconds, &mut nanos),
            relate_status::RELATE_OK
        ));
        assert_eq!((months, days, seconds, nanos), (0, 2, 3, 4));

        relate_destroy_result(result);
    }
}

/// Scalars, points and vectors: each accessor serves its kinds and refuses
/// the rest — strings cross whole (interior NUL included), points report
/// srid and dimension, vectors their dtype and per-dtype elements.
#[test]
fn walker_reads_scalars_points_and_vectors() {
    // SAFETY: test drives the FFI with handles it owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let result = run(
            conn,
            "RETURN true AS b, -5 AS i, 'a\\u0000b' AS s, \
             point({x: 1.5, y: 2.5}) AS p2, point({x: 1, y: 2, z: 3}) AS p3, \
             vector([1, 2, 3], 3, INTEGER8) AS vi, vector([1.5, 2.5], 2, FLOAT32) AS vf",
        );

        let mut flag = 9u8;
        let b = cell(result, 0, 0);
        assert!(matches!(
            relate_value_bool(b, &mut flag),
            relate_status::RELATE_OK
        ));
        assert_eq!(flag, 1);
        assert_eq!(int_of(cell(result, 0, 1)), -5);

        let s = cell(result, 0, 2);
        let mut len = 0usize;
        let p = relate_value_string(s, &mut len);
        assert_eq!(len, 3, "an interior NUL crosses whole");
        assert_eq!(
            std::slice::from_raw_parts(p.cast::<u8>(), len + 1),
            b"a\0b\0"
        );

        let (mut srid, mut dim, mut x, mut y, mut z) = (0, 0, 0.0, 0.0, -1.0);
        let p2 = cell(result, 0, 3);
        assert_eq!(kind(p2), RELATE_V_POINT as i32);
        assert!(matches!(
            relate_value_point(p2, &mut srid, &mut dim, &mut x, &mut y, &mut z),
            relate_status::RELATE_OK
        ));
        assert_eq!((srid, dim, x, y, z), (7203, 2, 1.5, 2.5, -1.0));
        let p3 = cell(result, 0, 4);
        assert!(matches!(
            relate_value_point(p3, &mut srid, &mut dim, &mut x, &mut y, &mut z),
            relate_status::RELATE_OK
        ));
        assert_eq!((srid, dim, x, y, z), (9157, 3, 1.0, 2.0, 3.0));

        let vi = cell(result, 0, 5);
        assert_eq!(kind(vi), RELATE_V_VECTOR as i32);
        assert_eq!(text(relate_value_vector_dtype(vi)), "INTEGER8");
        assert_eq!(relate_value_vector_dim(vi), 3);
        let (mut n, mut f) = (0i64, 0.0f64);
        assert!(matches!(
            relate_value_vector_int(vi, 2, &mut n),
            relate_status::RELATE_OK
        ));
        assert_eq!(n, 3);
        assert!(matches!(
            relate_value_vector_float(vi, 0, &mut f),
            relate_status::RELATE_ERROR
        ));
        let vf = cell(result, 0, 6);
        assert_eq!(text(relate_value_vector_dtype(vf)), "FLOAT32");
        assert!(matches!(
            relate_value_vector_float(vf, 1, &mut f),
            relate_status::RELATE_OK
        ));
        assert_eq!(f, 2.5);
        assert!(matches!(
            relate_value_vector_float(vf, 2, &mut f),
            relate_status::RELATE_ERROR
        ));
        assert!(matches!(
            relate_value_vector_int(vf, 0, &mut n),
            relate_status::RELATE_ERROR
        ));
        // A vector is not a list.
        assert_eq!(relate_value_list_len(vf), -1);

        relate_destroy_result(result);
    }
}

/// The legacy numeric entity ids (ADR-0506): `relate_value_id` equals
/// the INTEGER `id()` returns in the same row, the relationship endpoints
/// equal `id(startNode(r))` / `id(endNode(r))`, at the top level and
/// nested inside a LIST and a PATH — and the endpoint accessors refuse a
/// NODE, the wrong kind for them.
#[test]
fn walker_reads_legacy_numeric_entity_ids() {
    // SAFETY: test drives the FFI with handles it owns; every value handle
    // is read before its result is destroyed.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let created = run(
            conn,
            "CREATE (:Person {name: 'Ada'})-[:KNOWS {since: 1833}]->(:Person {name: 'Babbage'})",
        );
        relate_destroy_result(created);
        let result = run(
            conn,
            "MATCH p = (a:Person {name: 'Ada'})-[k:KNOWS]->(b:Person) \
             RETURN a, k, id(a) AS na, id(k) AS nk, id(startNode(k)) AS ns, \
             id(endNode(k)) AS ne, [a, k] AS pair, p",
        );

        let (na, nk, ns, ne) = (
            int_of(cell(result, 0, 2)),
            int_of(cell(result, 0, 3)),
            int_of(cell(result, 0, 4)),
            int_of(cell(result, 0, 5)),
        );

        // Top level: the node, the relationship and its endpoints.
        let a = cell(result, 0, 0);
        let k = cell(result, 0, 1);
        assert_eq!(component(relate_value_id, a), na, "id(a)");
        assert_eq!(component(relate_value_id, k), nk, "id(k)");
        assert_eq!(component(relate_value_start_id, k), ns, "id(startNode(k))");
        assert_eq!(component(relate_value_end_id, k), ne, "id(endNode(k))");
        // The endpoints are RELATIONSHIP-only: a NODE is the wrong kind.
        let mut out = 42i64;
        assert!(matches!(
            relate_value_start_id(a, &mut out),
            relate_status::RELATE_ERROR
        ));
        assert!(matches!(
            relate_value_end_id(a, &mut out),
            relate_status::RELATE_ERROR
        ));
        assert_eq!(out, 42, "a refusal leaves the out-param untouched");

        // Nested in a LIST: node and relationship keep their ids.
        let pair = cell(result, 0, 6);
        assert_eq!(kind(pair), RELATE_V_LIST as i32);
        let (item_a, item_k) = (
            relate_value_list_get(pair, 0),
            relate_value_list_get(pair, 1),
        );
        assert_eq!(component(relate_value_id, item_a), na);
        assert_eq!(component(relate_value_id, item_k), nk);
        assert_eq!(component(relate_value_start_id, item_k), ns);
        assert_eq!(component(relate_value_end_id, item_k), ne);

        // Nested in a PATH: node, relationship, node — each keeps its ids.
        let p = cell(result, 0, 7);
        assert_eq!(kind(p), RELATE_V_PATH as i32);
        let (start, rel, end) = (
            relate_value_list_get(p, 0),
            relate_value_list_get(p, 1),
            relate_value_list_get(p, 2),
        );
        assert_eq!(component(relate_value_id, start), na);
        assert_eq!(component(relate_value_id, rel), nk);
        assert_eq!(component(relate_value_start_id, rel), ns);
        assert_eq!(component(relate_value_end_id, rel), ne);
        assert_eq!(component(relate_value_id, end), ne);

        relate_destroy_result(result);
    }
}

/// Out-of-kind access is a defined refusal (ADR-0502 §1): NULL for pointer
/// returns, -1 for counts, `RELATE_ERROR` with the out-param untouched
/// for scalars — on a wrong kind, a NULL handle, a NULL out-param, or an
/// out-of-range index. A Cypher null is a NULL-kind handle, never a NULL
/// handle, so the refusal stays distinguishable from the value.
#[test]
fn walker_out_of_kind_access_is_a_defined_refusal() {
    // SAFETY: test drives the FFI with handles it owns, and NULL handles
    // the contract defines.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let result = run(conn, "RETURN null AS n, 'text' AS s, {k: null} AS m");

        // Out-of-range cells, both ends, and a NULL result.
        for (row, col) in [(-1, 0), (0, -1), (1, 0), (0, 3)] {
            assert!(relate_value_at(result, row, col).is_null());
        }
        assert!(relate_value_at(ptr::null_mut(), 0, 0).is_null());

        // A Cypher null is a handle of kind NULL.
        let null = cell(result, 0, 0);
        assert_eq!(kind(null), RELATE_V_NULL as i32);
        let m = cell(result, 0, 2);
        assert_eq!(kind(get(m, "k")), RELATE_V_NULL as i32);
        let absent = CString::new("missing").unwrap();
        assert!(relate_value_map_get(m, absent.as_ptr()).is_null());
        assert!(relate_value_map_get(m, ptr::null()).is_null());
        assert!(relate_value_map_key(m, 1, ptr::null_mut()).is_null());
        assert!(relate_value_map_key(m, -1, ptr::null_mut()).is_null());

        // Every accessor refuses a wrong kind and a NULL handle alike.
        let s = cell(result, 0, 1);
        for v in [s, ptr::null()] {
            let mut out = 42i64;
            let mut flag = 7u8;
            let mut f = 4.5f64;
            assert!(matches!(
                relate_value_int(v, &mut out),
                relate_status::RELATE_ERROR
            ));
            assert!(matches!(
                relate_value_bool(v, &mut flag),
                relate_status::RELATE_ERROR
            ));
            assert!(matches!(
                relate_value_float(v, &mut f),
                relate_status::RELATE_ERROR
            ));
            assert!(matches!(
                relate_value_is_nan(v, &mut flag),
                relate_status::RELATE_ERROR
            ));
            assert!(matches!(
                relate_value_month(v, &mut out),
                relate_status::RELATE_ERROR
            ));
            assert!(matches!(
                relate_value_id(v, &mut out),
                relate_status::RELATE_ERROR
            ));
            assert!(matches!(
                relate_value_start_id(v, &mut out),
                relate_status::RELATE_ERROR
            ));
            assert!(matches!(
                relate_value_end_id(v, &mut out),
                relate_status::RELATE_ERROR
            ));
            assert_eq!((out, flag, f), (42, 7, 4.5), "out-params untouched");
            assert!(relate_value_element_id(v).is_null());
            assert!(relate_value_start_element_id(v).is_null());
            assert!(relate_value_zone(v).is_null());
            assert!(relate_value_vector_dtype(v).is_null());
            assert!(relate_value_list_get(v, 0).is_null());
            assert_eq!(relate_value_list_len(v), -1);
            assert_eq!(relate_value_map_len(v), -1);
            assert_eq!(relate_value_vector_dim(v), -1);
            let (mut a, mut b, mut c, mut d) = (1, 2, 3, 4);
            assert!(matches!(
                relate_value_duration(v, &mut a, &mut b, &mut c, &mut d),
                relate_status::RELATE_ERROR
            ));
            assert_eq!((a, b, c, d), (1, 2, 3, 4));
        }
        assert_eq!(kind(ptr::null()), -1);
        assert!(relate_value_string(ptr::null(), ptr::null_mut()).is_null());
        assert!(relate_value_string(null, ptr::null_mut()).is_null());
        // In kind, but a NULL out-param: refused, not dereferenced.
        assert!(matches!(
            relate_value_int(cell(result, 0, 0), ptr::null_mut()),
            relate_status::RELATE_ERROR
        ));
        assert_eq!(str_of(s), "text");

        relate_destroy_result(result);
    }
}

/// Handles are owned by the result: a child is minted once (the same
/// handle on every descent), and every handle stays readable across later
/// statements on the connection until its own result is destroyed. An
/// error result has no cells.
#[test]
fn walker_handles_are_result_owned_and_stable() {
    // SAFETY: test drives the FFI with handles it owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let result = run(conn, "RETURN [[1, 2], 'x'] AS l");
        let l = cell(result, 0, 0);
        let first = relate_value_list_get(l, 0);
        assert_eq!(first, relate_value_list_get(l, 0));
        assert_eq!(l, cell(result, 0, 0));

        let other = run(conn, "RETURN 'later' AS s");
        assert_eq!(int_of(relate_value_list_get(first, 1)), 2);
        assert_eq!(str_of(relate_value_list_get(l, 1)), "x");
        relate_destroy_result(other);
        relate_destroy_result(result);

        let bad = CString::new("RETURN nope(").unwrap();
        let mut failed: *mut relate_query_result = ptr::null_mut();
        assert!(matches!(
            relate_query_params(
                conn,
                bad.as_ptr(),
                ptr::null(),
                &mut failed,
                ptr::null_mut()
            ),
            relate_status::RELATE_OK
        ));
        assert_eq!(relate_result_is_error(failed), 1);
        assert!(relate_value_at(failed, 0, 0).is_null());
        relate_destroy_result(failed);
    }
}
