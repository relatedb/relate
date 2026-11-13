//! Fuzz the typed params builder (ADR-0502 §4, LABS-937 /
//! OPLANE_REQ-00340357): the fuzz bytes drive an arbitrary sequence of
//! putters — scalar, temporal, spatial, vector, nested container — and
//! the builder is freed at the end. Every C input is built here from the
//! fuzz data (NUL-terminated names, buffers whose `len` matches), so the
//! trusted-length contract is never violated by the harness itself; the
//! target hunts for panics, poisons and refusals that are not defined
//! refusals.
#![no_main]

use std::ffi::CString;
use std::os::raw::c_char;

use relate_ffi_fuzz::*;
use libfuzzer_sys::fuzz_target;

/// A NUL-free, NUL-terminated name for one put: the byte chunk with
/// interior NULs stripped, or a short fallback (a name must be non-NULL
/// for map puts; an empty name is a legal, distinct key).
fn name(chunk: &[u8]) -> CString {
    let cleaned: Vec<u8> = chunk.iter().copied().filter(|b| *b != 0).collect();
    CString::new(cleaned).unwrap()
}


fuzz_target!(|data: &[u8]| {
    // SAFETY: every handle, string and out-param below is created and
    // owned by this harness, and every buffer's length matches the `len`
    // passed with it — the ABI's trusted-length contract holds by
    // construction here.
    unsafe {
        let root = relate_params_new();
        let mut list: *mut relate_params = std::ptr::null_mut();
        let mut map: *mut relate_params = std::ptr::null_mut();
        let mut error_json: *mut c_char = std::ptr::null_mut();
        let float64 = CString::new("FLOAT64").unwrap();

        // One op per byte: the byte's low bits pick the putter, the
        // remaining bytes (rotating) supply the name and the values.
        for (i, op) in data.iter().enumerate() {
            let rest = &data[i + 1..];
            let take = |n: usize| -> Vec<u8> {
                let start = i % data.len();
                (0..n).map(|k| data[(start + k) % data.len()]).collect()
            };
            let n = name(&take(7));
            let np = n.as_ptr();
            match op % 12 {
                0 => relate_params_put_int(root, np, i as i64, &mut error_json),
                1 => relate_params_put_float(root, np, i as f64, &mut error_json),
                2 => {
                    // A string whose `len` matches its buffer exactly.
                    let bytes = &rest[..rest.len().min(64)];
                    relate_params_put_string(
                        root,
                        np,
                        bytes.as_ptr().cast(),
                        bytes.len(),
                        &mut error_json,
                    )
                }
                3 => relate_params_put_bool(root, np, (i % 2) as u8, &mut error_json),
                4 => relate_params_put_null(root, np, &mut error_json),
                5 => relate_params_put_date(
                    root,
                    np,
                    2024,
                    11,
                    (i % 28) as i64 + 1,
                    &mut error_json,
                ),
                6 => relate_params_put_local_time(
                    root,
                    np,
                    (i % 24) as i64,
                    0,
                    0,
                    0,
                    &mut error_json,
                ),
                7 => relate_params_put_duration(
                    root,
                    np,
                    (i % 10) as i64,
                    0,
                    0,
                    0,
                    &mut error_json,
                ),
                8 => {
                    // A float vector whose dim matches the coordinate
                    // buffer (the trusted-length contract).
                    let dim = (i % 8) as usize;
                    let coords: Vec<f64> = (0..dim).map(|k| k as f64 + (i % 7) as f64).collect();
                    relate_params_put_vector_float(
                        root,
                        np,
                        float64.as_ptr(),
                        coords.as_ptr(),
                        dim,
                        &mut error_json,
                    )
                }
                9 => relate_params_put_list(root, np, &mut list, &mut error_json),
                10 => relate_params_put_map(root, np, &mut map, &mut error_json),
                _ => {
                    // Puts into the child containers minted above — a
                    // NULL-name list put or a named map put, whichever
                    // the byte's bit picks (a never-minted child is the
                    // NULL builder refusal, itself a defined refusal).
                    let child = if (op >> 7) & 1 == 0 { list } else { map };
                    if (op >> 6) & 1 == 0 {
                        relate_params_put_int(child, std::ptr::null(), i as i64, &mut error_json)
                    } else {
                        relate_params_put_float(child, np, i as f64, &mut error_json)
                    }
                }
            };
            // The error channel is caller-owned: take it back every
            // time, whether or not the call refused.
            reclaim(&mut error_json);
        }

        relate_params_free(root);
    }
});
