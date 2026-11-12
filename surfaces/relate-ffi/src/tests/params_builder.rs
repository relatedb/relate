//! The typed params builder (ADR-0502 §4): typed values bind from
//! components and round-trip through the walker, nested containers lower
//! without JSON, and every malformed put is a defined refusal.

use std::ffi::CString;
use std::os::raw::c_char;
use std::ptr;

use crate::abi::relate_status;
use crate::params::*;
use crate::result::*;
use crate::support::{cell, component, int_of, run_query, str_of, take_error_json, text, Session};
use crate::tabular::*;
use crate::value::relate_value_kind::*;
use crate::value::*;

/// A Rust string as an owned C string for one call.
fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// Assert a builder call succeeded.
fn ok(status: relate_status) {
    assert!(matches!(status, relate_status::RELATE_OK));
}

/// The acceptance round trip: a named-zone datetime (the zone id
/// survives), a sub-microsecond time, `duration('P1Y2M')` (months read
/// back), a 2D point and a FLOAT32 vector — each nested inside a list
/// parameter — bind through the builder and read back through the walker.
#[test]
fn ffi_params_typed_values_round_trip_inside_a_list() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let params = relate_params_new();
        let mut xs: *mut relate_params = ptr::null_mut();
        ok(relate_params_put_list(
            params,
            c("xs").as_ptr(),
            &mut xs,
            ptr::null_mut(),
        ));
        assert!(!xs.is_null());
        // A list takes NULL names: each put appends.
        let stockholm = c("Europe/Stockholm");
        ok(relate_params_put_datetime(
            xs,
            ptr::null(),
            2024,
            7,
            1,
            12,
            30,
            45,
            123_456_789,
            0,
            stockholm.as_ptr(),
            ptr::null_mut(),
        ));
        ok(relate_params_put_time(
            xs,
            ptr::null(),
            8,
            15,
            0,
            1,
            -5 * 3600,
            ptr::null(),
            ptr::null_mut(),
        ));
        ok(relate_params_put_duration(
            xs,
            ptr::null(),
            14,
            0,
            0,
            0,
            ptr::null_mut(),
        ));
        ok(relate_params_put_point(
            xs,
            ptr::null(),
            7203,
            1.5,
            -2.5,
            ptr::null(),
            ptr::null_mut(),
        ));
        let coords = [0.5f64, -1.25, 3.0];
        let float32 = c("FLOAT32");
        ok(relate_params_put_vector_float(
            xs,
            ptr::null(),
            float32.as_ptr(),
            coords.as_ptr(),
            coords.len(),
            ptr::null_mut(),
        ));

        let result = run_query(
            conn,
            "RETURN $xs AS xs, $xs[2] = duration('P1Y2M') AS same_duration, \
             $xs[0] = datetime('2024-07-01T12:30:45.123456789[Europe/Stockholm]') AS same_datetime",
            params,
        );
        let list = cell(result, 0, 0);
        assert_eq!(relate_value_list_len(list), 5);

        // DATETIME: the wall clock, the zone's summer offset, the zone id.
        let dt = relate_value_list_get(list, 0);
        assert_eq!(relate_value_kind(dt), RELATE_V_DATETIME as i32);
        assert_eq!(component(relate_value_year, dt), 2024);
        assert_eq!(component(relate_value_month, dt), 7);
        assert_eq!(component(relate_value_hour, dt), 12);
        assert_eq!(component(relate_value_nanosecond, dt), 123_456_789);
        assert_eq!(component(relate_value_offset_seconds, dt), 7200);
        assert_eq!(text(relate_value_zone(dt)), "Europe/Stockholm");

        // TIME: one nanosecond survives (sub-microsecond), offset kept.
        let t = relate_value_list_get(list, 1);
        assert_eq!(relate_value_kind(t), RELATE_V_TIME as i32);
        assert_eq!(component(relate_value_hour, t), 8);
        assert_eq!(component(relate_value_minute, t), 15);
        assert_eq!(component(relate_value_nanosecond, t), 1);
        assert_eq!(component(relate_value_offset_seconds, t), -5 * 3600);

        // DURATION: P1Y2M is 14 months, read back losslessly.
        let d = relate_value_list_get(list, 2);
        assert_eq!(relate_value_kind(d), RELATE_V_DURATION as i32);
        let (mut months, mut days, mut seconds, mut nanos) = (-1, -1, -1, -1);
        ok(relate_value_duration(
            d,
            &mut months,
            &mut days,
            &mut seconds,
            &mut nanos,
        ));
        assert_eq!((months, days, seconds, nanos), (14, 0, 0, 0));

        // POINT: cartesian 2D.
        let pt = relate_value_list_get(list, 3);
        assert_eq!(relate_value_kind(pt), RELATE_V_POINT as i32);
        let (mut srid, mut dim, mut x, mut y, mut z) = (0, 0, 0.0, 0.0, 99.0);
        ok(relate_value_point(
            pt, &mut srid, &mut dim, &mut x, &mut y, &mut z,
        ));
        assert_eq!((srid, dim, x, y, z), (7203, 2, 1.5, -2.5, 99.0));

        // VECTOR: FLOAT32, three coordinates.
        let v = relate_value_list_get(list, 4);
        assert_eq!(relate_value_kind(v), RELATE_V_VECTOR as i32);
        assert_eq!(text(relate_value_vector_dtype(v)), "FLOAT32");
        assert_eq!(relate_value_vector_dim(v), 3);
        for (i, expected) in coords.iter().enumerate() {
            let mut f = 0.0;
            ok(relate_value_vector_float(v, i as i64, &mut f));
            assert_eq!(f, *expected);
        }

        // The engine sees the same values its own constructors build.
        let mut same = 0;
        ok(relate_value_bool(cell(result, 0, 1), &mut same));
        assert_eq!(same, 1, "the bound duration equals duration('P1Y2M')");
        ok(relate_value_bool(cell(result, 0, 2), &mut same));
        assert_eq!(same, 1, "the bound datetime equals the zoned literal");

        relate_destroy_result(result);
        relate_params_free(params);
    }
}

/// Scalars, local temporals, integer vectors and nested maps bind by name
/// on the root; a string with an interior NUL crosses whole; the builder
/// is reusable across statements.
#[test]
fn ffi_params_scalars_maps_and_reuse() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let p = relate_params_new();
        let e = ptr::null_mut();
        ok(relate_params_put_null(p, c("n").as_ptr(), e));
        ok(relate_params_put_bool(p, c("b").as_ptr(), 1, e));
        ok(relate_params_put_int(p, c("i").as_ptr(), i64::MIN, e));
        ok(relate_params_put_float(p, c("f").as_ptr(), f64::NAN, e));
        let s = b"a\0b";
        ok(relate_params_put_string(
            p,
            c("s").as_ptr(),
            s.as_ptr().cast(),
            s.len(),
            e,
        ));
        ok(relate_params_put_date(p, c("d").as_ptr(), 1999, 12, 31, e));
        ok(relate_params_put_local_time(
            p,
            c("lt").as_ptr(),
            23,
            59,
            59,
            999_999_999,
            e,
        ));
        ok(relate_params_put_local_datetime(
            p,
            c("ldt").as_ptr(),
            2000,
            1,
            2,
            3,
            4,
            5,
            6,
            e,
        ));
        let ints = [-128i64, 0, 127];
        ok(relate_params_put_vector_int(
            p,
            c("v").as_ptr(),
            c("INTEGER8").as_ptr(),
            ints.as_ptr(),
            ints.len(),
            e,
        ));
        let mut m: *mut relate_params = ptr::null_mut();
        ok(relate_params_put_map(p, c("m").as_ptr(), &mut m, e));
        ok(relate_params_put_int(m, c("k").as_ptr(), 7, e));
        let mut inner: *mut relate_params = ptr::null_mut();
        ok(relate_params_put_list(m, c("l").as_ptr(), &mut inner, e));
        ok(relate_params_put_point(
            inner,
            ptr::null(),
            4979,
            1.0,
            2.0,
            &3.0,
            e,
        ));

        let result = run_query(
            conn,
            "RETURN $n IS NULL AS n, $b AS b, $i AS i, $f AS f, $s AS s, $d AS d, \
             $lt AS lt, $ldt AS ldt, $v AS v, $m AS m",
            p,
        );
        let mut b = 0;
        ok(relate_value_bool(cell(result, 0, 0), &mut b));
        assert_eq!(b, 1);
        ok(relate_value_bool(cell(result, 0, 1), &mut b));
        assert_eq!(b, 1);
        assert_eq!(int_of(cell(result, 0, 2)), i64::MIN);
        let mut nan = 0;
        ok(relate_value_is_nan(cell(result, 0, 3), &mut nan));
        assert_eq!(nan, 1);
        let mut len = 0;
        let sp = relate_value_string(cell(result, 0, 4), &mut len);
        assert_eq!(std::slice::from_raw_parts(sp.cast::<u8>(), len), s);
        let d = cell(result, 0, 5);
        assert_eq!(relate_value_kind(d), RELATE_V_DATE as i32);
        assert_eq!(component(relate_value_day, d), 31);
        let lt = cell(result, 0, 6);
        assert_eq!(relate_value_kind(lt), RELATE_V_LOCAL_TIME as i32);
        assert_eq!(component(relate_value_nanosecond, lt), 999_999_999);
        let ldt = cell(result, 0, 7);
        assert_eq!(relate_value_kind(ldt), RELATE_V_LOCAL_DATETIME as i32);
        assert_eq!(component(relate_value_nanosecond, ldt), 6);
        let v = cell(result, 0, 8);
        assert_eq!(text(relate_value_vector_dtype(v)), "INTEGER8");
        let mut n = 0;
        ok(relate_value_vector_int(v, 0, &mut n));
        assert_eq!(n, -128);
        let map = cell(result, 0, 9);
        assert_eq!(relate_value_kind(map), RELATE_V_MAP as i32);
        assert_eq!(int_of(relate_value_map_get(map, c("k").as_ptr())), 7);
        let l = relate_value_map_get(map, c("l").as_ptr());
        let (mut srid, mut dim, mut x, mut y, mut z) = (0, 0, 0.0, 0.0, 0.0);
        ok(relate_value_point(
            relate_value_list_get(l, 0),
            &mut srid,
            &mut dim,
            &mut x,
            &mut y,
            &mut z,
        ));
        assert_eq!((srid, dim, z), (4979, 3, 3.0));
        relate_destroy_result(result);

        // The builder is read, not consumed: extend it and run again.
        ok(relate_params_put_string(
            p,
            c("t").as_ptr(),
            c("x").as_ptr(),
            1,
            e,
        ));
        let again = run_query(conn, "RETURN $t AS t, $m.k AS k", p);
        assert_eq!(str_of(cell(again, 0, 0)), "x");
        assert_eq!(int_of(cell(again, 0, 1)), 7);
        relate_destroy_result(again);

        // A child handle is borrowed: freeing it is a no-op, the root
        // still lowers it.
        relate_params_free(m);
        let still = run_query(conn, "RETURN $m.k AS k", p);
        assert_eq!(int_of(cell(still, 0, 0)), 7);
        relate_destroy_result(still);

        // A `put_map` child is a parameter set in its own right (the
        // header and spec's root-or-child rule): bound directly, its own
        // entries are the statement's parameters.
        let child = run_query(conn, "RETURN $k AS k", m);
        assert_eq!(int_of(cell(child, 0, 0)), 7);
        relate_destroy_result(child);

        // NULL params run the statement with none.
        let none = run_query(conn, "RETURN 1 AS one", ptr::null());
        assert_eq!(int_of(cell(none, 0, 0)), 1);
        relate_destroy_result(none);

        relate_params_free(p);
        relate_params_free(ptr::null_mut());
    }
}

/// Run one putter expected to refuse: `RELATE_ERROR`, the
/// internal-stage error shape naming `reason`.
///
/// # Safety
/// `put` must call a putter with the given error out-param.
unsafe fn refused(reason: &str, put: impl FnOnce(*mut *mut c_char) -> relate_status) {
    let mut error_json: *mut c_char = ptr::null_mut();
    assert!(
        matches!(put(&mut error_json), relate_status::RELATE_ERROR),
        "{reason}: expected a refusal"
    );
    assert!(
        !error_json.is_null(),
        "{reason}: the shape rides error_json"
    );
    // SAFETY: the FFI filled the out-param.
    let shape = unsafe { take_error_json(error_json) };
    assert_eq!(shape["stage"], "internal");
    let message = shape["message"].as_str().unwrap();
    assert!(message.contains(reason), "{reason}: {message}");
}

/// The empty STRING binds (LABS-1198): a NULL buffer with no bytes is the
/// canonical C spelling of `""` — a marshalling layer may hand a
/// a zero-length buffer over as NULL — and it round-trips as a zero-length
/// STRING, distinct from null and from absent. A NULL buffer that claims
/// bytes stays the "null string value" refusal.
#[test]
fn ffi_params_empty_string_binds_as_zero_length_string() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let params = relate_params_new();
        // The koffi spelling: a zero-length buffer marshals as NULL.
        ok(relate_params_put_string(
            params,
            c("none").as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
        ));
        // A non-NULL zero-length buffer binds the same value.
        ok(relate_params_put_string(
            params,
            c("some").as_ptr(),
            "".as_ptr().cast(),
            0,
            ptr::null_mut(),
        ));
        // Bytes claimed at NULL is still a refusal.
        refused("null string value", |e| {
            relate_params_put_string(params, c("bad").as_ptr(), ptr::null(), 1, e)
        });

        let result = run_query(
            conn,
            "RETURN $none AS none, $some AS some, $none IS NULL AS is_null, \
             $none = '' AND $some = '' AS both_empty",
            params,
        );
        for col in 0..2 {
            let cell = cell(result, 0, col);
            // STRING kind, not NULL: the zero-length string is a value.
            assert_eq!(relate_value_kind(cell), RELATE_V_STRING as i32);
            assert_eq!(str_of(cell), "");
        }
        let mut b = 0;
        ok(relate_value_bool(cell(result, 0, 2), &mut b));
        assert_eq!(b, 0);
        ok(relate_value_bool(cell(result, 0, 3), &mut b));
        assert_eq!(b, 1);

        relate_destroy_result(result);
        relate_params_free(params);
    }
}

/// The malformed-put tests' shared setup: an in-memory connection, a
/// parameter builder `p` and a list child `xs` under it.
struct Fixture {
    _session: Session,
    conn: *mut relate_connection,
    p: *mut relate_params,
    list: *mut relate_params,
}

impl Fixture {
    /// # Safety
    /// The fixture owns its handles; dropping it frees every handle.
    unsafe fn new() -> Self {
        // SAFETY: opens and drives the FFI with out-params this fn owns.
        unsafe {
            let session = Session::open();
            let (_, conn) = session.handles();
            let p = relate_params_new();
            let mut list: *mut relate_params = ptr::null_mut();
            ok(relate_params_put_list(
                p,
                c("xs").as_ptr(),
                &mut list,
                ptr::null_mut(),
            ));
            Fixture {
                _session: session,
                conn,
                p,
                list,
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // SAFETY: the fixture uniquely owns each handle, freed once.
        unsafe {
            relate_params_free(self.p);
        }
    }
}

/// Malformed names, string payloads and builder-kind puts are defined
/// refusals on the error channel — never UB.
#[test]
fn ffi_params_malformed_names_and_strings_are_defined_refusals() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let f = Fixture::new();
        let (p, list) = (f.p, f.list);
        let name = c("x");
        let n = name.as_ptr();

        refused("null params builder", |e| {
            relate_params_put_int(ptr::null_mut(), n, 1, e)
        });
        refused("needs a non-NULL name", |e| {
            relate_params_put_int(p, ptr::null(), 1, e)
        });
        ok(relate_params_put_int(p, n, 1, ptr::null_mut()));
        refused("duplicate parameter name 'x'", |e| {
            relate_params_put_int(p, n, 2, e)
        });
        refused("takes a NULL name", |e| {
            relate_params_put_int(list, n, 1, e)
        });
        refused("null string value", |e| {
            relate_params_put_string(p, c("s").as_ptr(), ptr::null(), 1, e)
        });
        refused("invalid utf-8", |e| {
            relate_params_put_string(p, c("s").as_ptr(), b"\xff".as_ptr().cast(), 1, e)
        });
    }
}

/// Out-of-range and unresolvable temporal puts are defined refusals on
/// the error channel — never UB.
#[test]
fn ffi_params_malformed_temporal_puts_are_defined_refusals() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let f = Fixture::new();
        let p = f.p;
        let stockholm = c("Europe/Stockholm");
        let nowhere = c("Mars/Olympus");
        let offset_only = c("+01:00");

        refused("date components out of range", |e| {
            relate_params_put_date(p, c("d").as_ptr(), 2023, 2, 29, e)
        });
        refused("time components out of range", |e| {
            relate_params_put_local_time(p, c("t").as_ptr(), 24, 0, 0, 0, e)
        });
        refused("time components out of range", |e| {
            relate_params_put_local_time(p, c("t").as_ptr(), 0, 0, 0, 1_000_000_000, e)
        });
        refused("UTC offset out of range", |e| {
            relate_params_put_time(
                p,
                c("t").as_ptr(),
                0,
                0,
                0,
                0,
                18 * 3600 + 1,
                ptr::null(),
                e,
            )
        });
        refused("unknown time zone 'Mars/Olympus'", |e| {
            relate_params_put_datetime(
                p,
                c("dt").as_ptr(),
                2024,
                1,
                1,
                0,
                0,
                0,
                0,
                0,
                nowhere.as_ptr(),
                e,
            )
        });
        refused("not an IANA region id", |e| {
            relate_params_put_datetime(
                p,
                c("dt").as_ptr(),
                2024,
                1,
                1,
                0,
                0,
                0,
                0,
                0,
                offset_only.as_ptr(),
                e,
            )
        });
        // 2024-03-31 02:30 never occurs in Stockholm (spring forward).
        refused("DST gap", |e| {
            relate_params_put_datetime(
                p,
                c("dt").as_ptr(),
                2024,
                3,
                31,
                2,
                30,
                0,
                0,
                0,
                stockholm.as_ptr(),
                e,
            )
        });
    }
}

/// Invalid point systems, vector dtypes, coordinates and child out
/// pointers are defined refusals on the error channel — never UB.
#[test]
fn ffi_params_malformed_point_and_vector_puts_are_defined_refusals() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let f = Fixture::new();
        let p = f.p;
        let float32 = c("FLOAT32");
        let int8 = c("INTEGER8");
        let bogus = c("FLOAT128");

        refused("is not a point() coordinate system", |e| {
            relate_params_put_point(p, c("pt").as_ptr(), 7203, 1.0, 2.0, &3.0, e)
        });
        refused("is not a point() coordinate system", |e| {
            relate_params_put_point(p, c("pt").as_ptr(), 1234, 1.0, 2.0, ptr::null(), e)
        });
        let inexact = [0.1f64];
        refused("not exactly representable as FLOAT32", |e| {
            relate_params_put_vector_float(
                p,
                c("v").as_ptr(),
                float32.as_ptr(),
                inexact.as_ptr(),
                1,
                e,
            )
        });
        refused("takes integer coordinates", |e| {
            relate_params_put_vector_float(
                p,
                c("v").as_ptr(),
                int8.as_ptr(),
                inexact.as_ptr(),
                1,
                e,
            )
        });
        let wide = [128i64];
        refused("out of range for INTEGER8", |e| {
            relate_params_put_vector_int(p, c("v").as_ptr(), int8.as_ptr(), wide.as_ptr(), 1, e)
        });
        refused("takes floating coordinates", |e| {
            relate_params_put_vector_int(p, c("v").as_ptr(), float32.as_ptr(), wide.as_ptr(), 1, e)
        });
        refused("unknown vector dtype 'FLOAT128'", |e| {
            relate_params_put_vector_int(p, c("v").as_ptr(), bogus.as_ptr(), wide.as_ptr(), 1, e)
        });
        refused("null vector coordinates", |e| {
            relate_params_put_vector_int(p, c("v").as_ptr(), int8.as_ptr(), ptr::null(), 2, e)
        });
        refused("null child out pointer", |e| {
            relate_params_put_map(p, c("m").as_ptr(), ptr::null_mut(), e)
        });
    }
}

/// Nesting stops at the depth cap, a NULL `error_json` opts out of the
/// detail, and every refusal leaves the builder unchanged and usable.
#[test]
fn ffi_params_refusals_leave_the_builder_unchanged_and_usable() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let f = Fixture::new();
        let (p, list, conn) = (f.p, f.list, f.conn);
        let name = c("x");
        let n = name.as_ptr();
        ok(relate_params_put_int(p, n, 1, ptr::null_mut()));

        // Nesting stops at the engine's value-depth cap (ADR-0462).
        let mut at = list;
        for _ in 1..relate::MAX_VALUE_DEPTH {
            let mut next: *mut relate_params = ptr::null_mut();
            ok(relate_params_put_list(
                at,
                ptr::null(),
                &mut next,
                ptr::null_mut(),
            ));
            at = next;
        }
        let mut sentinel: *mut relate_params = ptr::null_mut();
        refused("value depth limit", |e| {
            relate_params_put_list(at, ptr::null(), &mut sentinel, e)
        });
        assert!(sentinel.is_null(), "a refused child is never handed out");

        // A NULL error_json opts out of the detail; the status still reports.
        assert!(matches!(
            relate_params_put_int(list, n, 1, ptr::null_mut()),
            relate_status::RELATE_ERROR
        ));

        // The refusals changed nothing: `x` is the one scalar bound, and
        // `xs` holds only the nested chain.
        let result = run_query(conn, "RETURN $x AS x", p);
        assert_eq!(int_of(cell(result, 0, 0)), 1);
        relate_destroy_result(result);
        let keys = run_query(conn, "RETURN size($xs) AS n", p);
        assert_eq!(int_of(cell(keys, 0, 0)), 1);
        relate_destroy_result(keys);

        // A list builder is not a parameter map: a status error, no result.
        // (A `put_map` child IS accepted — the spec lets a caller bind a
        // child map as its own parameter set; covered in the
        // scalars-maps-and-reuse test.)
        let q = c("RETURN 1");
        let mut result: *mut relate_query_result = ptr::null_mut();
        refused("params must be a map builder", |e| {
            relate_query_params(conn, q.as_ptr(), list, &mut result, e)
        });
        assert!(result.is_null());
    }
}

/// The builder's fixed caps (OPLANE_REQ-00340363/57, LABS-937): each
/// accepts its limit and refuses one past it — a runaway putter loop
/// fails with a typed refusal on the error channel instead of
/// accumulating without bound. Each section uses its own builder: the
/// caps are independent bounds, and the total budget in the last section
/// shows why.
#[test]
fn ffi_params_builder_caps_accept_the_limit_and_refuse_one_past() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        // Entries: the limit's worth of positional items appends; the next
        // is a defined refusal (100k x 16 B overhead stays far inside the
        // builder budget).
        let p = relate_params_new();
        let mut list: *mut relate_params = ptr::null_mut();
        ok(relate_params_put_list(
            p,
            c("xs").as_ptr(),
            &mut list,
            ptr::null_mut(),
        ));
        for i in 0..MAX_PARAM_ENTRIES as i64 {
            ok(relate_params_put_int(list, ptr::null(), i, ptr::null_mut()));
        }
        refused("entry limit", |e| {
            relate_params_put_int(list, ptr::null(), 0, e)
        });
        relate_params_free(p);

        // Names: a limit-length name binds; one byte longer refuses.
        let p = relate_params_new();
        let name_at = c(&"n".repeat(MAX_PARAM_NAME_BYTES));
        ok(relate_params_put_int(
            p,
            name_at.as_ptr(),
            1,
            ptr::null_mut(),
        ));
        let name_past = c(&"n".repeat(MAX_PARAM_NAME_BYTES + 1));
        refused("parameter name exceeds", |e| {
            relate_params_put_int(p, name_past.as_ptr(), 1, e)
        });
        relate_params_free(p);

        // Strings and the builder budget: one half-budget string binds
        // and spends it; the second refuses on the aggregate; a single
        // over-budget length refuses before its bytes are ever copied.
        // `put_string` takes buffer + len, so a byte vector is the buffer.
        let p = relate_params_new();
        let s = "s";
        let half = MAX_PARAM_TOTAL_BYTES / 2;
        let first = vec![b'a'; half];
        ok(relate_params_put_string(
            p,
            c(s).as_ptr(),
            first.as_ptr().cast(),
            first.len(),
            ptr::null_mut(),
        ));
        let second = vec![b'a'; half];
        refused("total byte budget", |e| {
            relate_params_put_string(p, c(s).as_ptr(), second.as_ptr().cast(), second.len(), e)
        });
        let past = vec![b'a'; MAX_PARAM_TOTAL_BYTES + 1];
        refused("total byte budget", |e| {
            relate_params_put_string(p, c(s).as_ptr(), past.as_ptr().cast(), past.len(), e)
        });
        relate_params_free(p);
    }
}
