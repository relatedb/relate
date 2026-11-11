use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// Test-only allocation guard. In the dedicated subprocess it aborts BEFORE
// a speculative allocation could exhaust the test machine. Counting all
// requests (including reallocations) is stricter than tracking peak heap.
struct DecodeAllocationGuard;
static COUNT_DECODE_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static REQUESTED_DECODE_BYTES: AtomicUsize = AtomicUsize::new(0);
// What an over-budget decode reports through `decode`.
const OVER_BUDGET: &str = "packstream decode budget exceeded";
const MAX_REQUESTED_DECODE_BYTES: usize = 128 * 1024 * 1024;

#[global_allocator]
static ALLOCATOR: DecodeAllocationGuard = DecodeAllocationGuard;

fn check_allocation(size: usize) {
    if COUNT_DECODE_ALLOCATIONS.load(Ordering::Relaxed) {
        let previously_requested = REQUESTED_DECODE_BYTES.fetch_add(size, Ordering::Relaxed);
        if size > MAX_REQUESTED_DECODE_BYTES
            || previously_requested > MAX_REQUESTED_DECODE_BYTES - size
        {
            std::process::abort();
        }
    }
}

// SAFETY: the hook uses only atomics and aborts (never allocates or
// unwinds); each method preserves GlobalAlloc's pointer/layout contract
// by forwarding the exact arguments and result to System.
unsafe impl GlobalAlloc for DecodeAllocationGuard {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        check_allocation(layout.size());
        // SAFETY: the caller guarantees a valid layout; forward it unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        check_allocation(layout.size());
        // SAFETY: the caller guarantees a valid layout; forward it unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        check_allocation(new_size);
        // SAFETY: the caller guarantees ptr/layout and the requested new size;
        // forward all three unchanged to the allocator that owns the block.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller guarantees ptr/layout; System allocated it.
        unsafe { System.dealloc(ptr, layout) }
    }
}

// One dispatch helper owns the env-var check, the child-body early return,
// and the self re-exec, so each allocator-guard test carries only its child
// body. A separate process makes the test safe even if the old decoder is
// reintroduced: the guard aborts before its large allocations occur.
// Unlike RLIMIT_AS, this works on macOS and Windows as well as Linux.
fn isolated_decode_test(child_env: &str, test_name: &str, body: impl FnOnce()) {
    if std::env::var_os(child_env).is_some() {
        body();
        return;
    }
    // Isolate the process-global allocator guard from parallel tests.
    run_isolated_decode_test(test_name, child_env);
}

// Arm the allocation guard, run `f`, disarm, and read back the requested
// byte total: the shared scaffold of the allocator-guard child bodies.
fn count_allocations<T>(f: impl FnOnce() -> T) -> (T, usize) {
    REQUESTED_DECODE_BYTES.store(0, Ordering::Relaxed);
    COUNT_DECODE_ALLOCATIONS.store(true, Ordering::Relaxed);
    let value = f();
    COUNT_DECODE_ALLOCATIONS.store(false, Ordering::Relaxed);
    (value, REQUESTED_DECODE_BYTES.load(Ordering::Relaxed))
}

// The decode-shaped call site of `count_allocations`.
fn decode_counting_allocations(input: &[u8]) -> (Result<Pack, String>, usize) {
    count_allocations(|| decode(input))
}

fn run_isolated_decode_test(test_name: &str, child_env: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env(child_env, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{test_name} child failed: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
        "{test_name} child did not run: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn get_looks_up_map_keys() {
    let map = Pack::Map(vec![
        (Pack::String("a".into()), Pack::Int(1)),
        (Pack::Int(2), Pack::String("non-string key".into())),
    ]);
    assert_eq!(map.get("a"), Some(&Pack::Int(1)));
    assert_eq!(map.get("missing"), None);
    assert_eq!(Pack::Int(1).get("a"), None, "get on a non-map is None");
}

// The shape that amplified allocation: a chain of single-entry maps with
// large keys. Materializing `format!("{path}.{key}")` per level kept every
// ancestor path string alive, so a 128-level chain with 8 KiB keys allocated
// roughly 64x the key bytes on the connection task. The lazy path pushes
// borrowed segments and renders only on failure, so conversion-time
// allocation must stay within 4x the total key bytes (output key strings,
// BTreeMap nodes, and the path-segment vector).
#[test]
fn nested_map_conversion_does_not_amplify_path_allocations() {
    isolated_decode_test(
        "RELATE_PACKSTREAM_LAZY_PATH_CHILD",
        "packstream::tests::nested_map_conversion_does_not_amplify_path_allocations",
        || {
            const LEVELS: usize = 128;
            const KEY_BYTES: usize = 8 * 1024;
            let key = "k".repeat(KEY_BYTES);
            let mut value = Pack::Null;
            for _ in 0..LEVELS {
                value = Pack::Map(vec![(Pack::String(key.clone()), value)]);
            }
            let total_key_bytes = LEVELS * KEY_BYTES;

            // Count around the conversion only: building the wire-shaped
            // Pack above must not count toward the budget.
            let (converted, requested) = count_allocations(|| value.to_mdb());

            assert!(converted.is_ok(), "the map chain must convert");
            assert!(
                requested <= 4 * total_key_bytes,
                "conversion must allocate at most 4x the {total_key_bytes} total key \
                 bytes; requested {requested}"
            );
        },
    );
}

#[test]
fn roundtrip_int() {
    let p = Pack::Int(42);
    let e = encode(&p).unwrap();
    assert_eq!(decode(&e).unwrap(), p);
}

#[test]
fn roundtrip_bytes() {
    // 8-bit size marker (0xCC)
    let small = Pack::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(decode(&encode(&small).unwrap()).unwrap(), small);
    // 16-bit size marker (0xCD)
    let large = Pack::Bytes(vec![0xAB; 300]);
    assert_eq!(decode(&encode(&large).unwrap()).unwrap(), large);
}

#[test]
fn valid_fixed_width_values_and_containers_roundtrip() {
    for value in [
        Pack::Float(-1.25),
        Pack::Int(-100),
        Pack::Int(-30_000),
        Pack::Int(i32::MAX as i64),
        Pack::Int(i64::MIN),
        Pack::String("z".repeat(300)),
        Pack::List(vec![Pack::Null; 300]),
        Pack::Map(vec![(Pack::Null, Pack::Bool(true)); 300]),
        Pack::Struct {
            tag: 0x01,
            fields: vec![Pack::Null],
        },
    ] {
        assert_eq!(decode(&encode(&value).unwrap()).unwrap(), value);
    }
}

#[test]
fn truncated_fixed_width_and_lengths_return_errors() {
    for (marker, width) in [
        (0xC1, 8),
        (0xC8, 1),
        (0xC9, 2),
        (0xCA, 4),
        (0xCB, 8),
        (0xCC, 1),
        (0xCD, 2),
        (0xCE, 4),
        (0xD0, 1),
        (0xD1, 2),
        (0xD2, 4),
        (0xD4, 1),
        (0xD5, 2),
        (0xD6, 4),
        (0xD8, 1),
        (0xD9, 2),
        (0xDA, 4),
    ] {
        for available in 0..width {
            let mut data = vec![marker];
            data.extend(std::iter::repeat_n(0, available));
            assert!(
                decode(&data).is_err(),
                "marker {marker:#x}, {available} bytes"
            );
        }
    }
    assert!(decode(&[0xB0]).is_err(), "struct tag must be present");
}

/// The two-byte signature read the session's RESET pre-scan uses
/// (SEC-serve-batch): the tag of a leading structure, and `None` for
/// anything that does not open a structure. A `Some` signature promises
/// nothing about the body — a truncated struct still fails [`decode`].
#[test]
fn struct_tag_reads_the_leading_signature_only() {
    assert_eq!(struct_tag(&[0xB0, 0x0F]), Some(0x0F));
    assert_eq!(struct_tag(&[0xB3, 0x10, 0x00, 0x00, 0x00]), Some(0x10));
    assert_eq!(struct_tag(&[0xBF, 0x0F, 0xC0]), Some(0x0F));
    assert_eq!(struct_tag(&[0xAF, 0x0F]), None, "a map is not a structure");
    assert_eq!(struct_tag(&[0xC0, 0x0F]), None, "null is not a structure");
    assert_eq!(struct_tag(&[0xB0]), None, "the tag byte must be present");
    assert_eq!(struct_tag(&[]), None);

    // The signature says nothing about the body: this payload has a RESET
    // signature and still cannot decode.
    let truncated = [0xB2, 0x0F];
    assert_eq!(struct_tag(&truncated), Some(0x0F));
    assert!(decode(&truncated).is_err());
}

#[test]
fn borrowed_cursor_preserves_field_boundaries_and_errors() {
    assert_eq!(decode(&[]), Err("eof".into()));
    assert_eq!(
        decode(&[0xC4]),
        Err("unknown packstream marker 0xc4".into())
    );
    for prefix in [
        vec![0x82],
        vec![0xD0, 2],
        vec![0xD1, 0, 2],
        vec![0xD2, 0, 0, 0, 2],
    ] {
        let mut data = prefix;
        data.push(b'a');
        assert_eq!(decode(&data), Err("short string".into()));
    }
    for prefix in [vec![0xCC, 2], vec![0xCD, 0, 2], vec![0xCE, 0, 0, 0, 2]] {
        let mut data = prefix;
        data.push(0xFF);
        assert_eq!(decode(&data), Err("short bytes".into()));
    }
    for value in [
        Pack::String("é".into()),
        Pack::String("é".repeat(130)),
        Pack::String("é".repeat(33_000)),
        Pack::Bytes(vec![0xFF; 256]),
        Pack::Bytes(vec![0xFF; 65_536]),
    ] {
        assert_eq!(decode(&encode(&value).unwrap()), Ok(value));
    }
    assert_eq!(
        decode(&[0x81, 0xFF]),
        Err("invalid utf-8 sequence of 1 bytes from index 0".into())
    );
}

#[test]
fn unparsed_payload_has_no_full_input_allocation() {
    isolated_decode_test(
        "RELATE_PACKSTREAM_BORROWED_CHILD",
        "packstream::tests::unparsed_payload_has_no_full_input_allocation",
        || {
            // The first value ends at byte 1; none of the trailing data needs
            // ownership. The former Bytes::copy_from_slice allocated 8 MiB.
            let payload = vec![0xC0; 8 * 1024 * 1024];
            let (decoded, requested) = decode_counting_allocations(&payload);
            assert_eq!(decoded, Ok(Pack::Null));
            assert_eq!(requested, 0);
        },
    );
}

#[test]
fn impossible_container_lengths_are_rejected() {
    assert!(decode(&[0xD6, 0xFF, 0xFF, 0xFF, 0xFF]).is_err());
    assert!(decode(&[0xDA, 0xFF, 0xFF, 0xFF, 0xFF]).is_err());
    assert!(decode(&[0xD4, 2, 0xC0]).is_err());
    assert!(decode(&[0xD8, 1, 0xC0]).is_err());
}

#[test]
fn aggregate_decode_budget_rejects_wire_sized_list_before_allocating() {
    // The pre-existing 16 MiB wire cap alone allowed this 16 MiB list to
    // reserve hundreds of MiB for Pack values before handling a query.
    let n = (16 * 1024 * 1024 - 5) as u32;
    let mut bomb = vec![0xD6];
    bomb.extend_from_slice(&n.to_be_bytes());
    bomb.resize(16 * 1024 * 1024, 0xC0);
    assert_eq!(decode(&bomb), Err(OVER_BUDGET.into()));

    // The budget counts one node per value: the root, the container itself,
    // both map halves, and siblings, not just one top-level declared size.
    let fits = MAX_DECODE_BYTES / NODE_BYTES;
    let accepted = Pack::List(vec![Pack::Null; fits - 1]);
    assert_eq!(decode(&encode(&accepted).unwrap()).unwrap(), accepted);
    let rejected = Pack::List(vec![Pack::Null; fits]);
    assert_eq!(decode(&encode(&rejected).unwrap()), Err(OVER_BUDGET.into()));
    let siblings = Pack::List(vec![
        Pack::List(vec![Pack::Null; fits / 2]),
        Pack::List(vec![Pack::Null; fits / 2]),
    ]);
    assert_eq!(decode(&encode(&siblings).unwrap()), Err(OVER_BUDGET.into()));
    let map = Pack::Map(vec![(Pack::Null, Pack::Null); fits / 2]);
    assert_eq!(decode(&encode(&map).unwrap()), Err(OVER_BUDGET.into()));
}

#[test]
fn string_and_byte_payloads_are_charged_beyond_their_node() {
    // One 1 MiB string per element: a few dozen of them exhaust the budget
    // even though the node count stays tiny.
    let big = Pack::String("x".repeat(1024 * 1024));
    let over = Pack::List(vec![big.clone(); MAX_DECODE_BYTES / (1024 * 1024) + 1]);
    assert_eq!(decode(&encode(&over).unwrap()), Err(OVER_BUDGET.into()));
    let under = Pack::List(vec![big; MAX_DECODE_BYTES / (1024 * 1024) / 2]);
    assert_eq!(decode(&encode(&under).unwrap()).unwrap(), under);
}

#[test]
fn a_dense_row_batch_far_past_the_old_count_cap_decodes() {
    // The LABS-1412 notebook shape: 133k rows of 3 integer fields, about
    // 930k values — over the former 262,144-value count, ~30 MB decoded.
    let row = Pack::Map(
        ["a", "b", "c"]
            .into_iter()
            .map(|k| (Pack::String(k.into()), Pack::Int(1)))
            .collect(),
    );
    let rows = Pack::List(vec![row; 133_000]);
    assert_eq!(decode(&encode(&rows).unwrap()).unwrap(), rows);
}

#[test]
fn a_hostile_scalar_list_is_refused_within_twice_the_budget_of_allocation() {
    isolated_decode_test(
        "RELATE_PACKSTREAM_HOSTILE_SCALARS_CHILD",
        "packstream::tests::a_hostile_scalar_list_is_refused_within_twice_the_budget_of_allocation",
        || {
            // 16 MiB of one-byte scalars would decode to ~512 MiB of nodes;
            // the budget stops the decode after at most ~2x its size in
            // pushed slots (Vec doubling), independent of the message size.
            let n = (16 * 1024 * 1024 - 5) as u32;
            let mut bomb = vec![0xD6];
            bomb.extend_from_slice(&n.to_be_bytes());
            bomb.resize(16 * 1024 * 1024, 0xC0);
            let (decoded, requested) = decode_counting_allocations(&bomb);
            assert_eq!(decoded, Err(OVER_BUDGET.into()));
            assert!(
                requested <= 2 * MAX_DECODE_BYTES + 1024 * 1024,
                "decode requested {requested} bytes before refusing"
            );
        },
    );
}

#[test]
fn nested_declared_lengths_do_not_preallocate_children() {
    isolated_decode_test(
        "RELATE_PACKSTREAM_NESTED_ALLOCATION_CHILD",
        "packstream::tests::nested_declared_lengths_do_not_preallocate_children",
        || {
            // 128 list32 headers each declare almost the full shared decode
            // budget. The innermost list has enough bytes and values to pass
            // both guards; the outer list fails only after decoding its child.
            // Old Vec::with_capacity(n) reserved roughly 1 GiB across frames.
            // A quarter of the budget per frame: the innermost legitimate decode
            // stays well under the allocation guard, while preallocating each
            // declared size across 128 frames would be gigabytes.
            let n = MAX_DECODE_BYTES / NODE_BYTES / 4 - MAX_DEPTH - 1;
            let mut bomb = Vec::with_capacity(5 * MAX_DEPTH + n);
            for _ in 0..MAX_DEPTH {
                bomb.push(0xD6);
                bomb.extend_from_slice(&(n as u32).to_be_bytes());
            }
            bomb.resize(bomb.len() + n, 0xC0);
            assert!(bomb.len() < 3 * 1024 * 1024);

            let (decoded, requested) = decode_counting_allocations(&bomb);
            assert_eq!(decoded, Err("eof".into()));
            assert!(requested < MAX_REQUESTED_DECODE_BYTES);
        },
    );
}

#[test]
fn nesting_is_bounded_on_a_small_thread_stack() {
    // 2 MiB — the old tokio worker stack size — proves the depth limit fires
    // long before any realistically sized stack overflows.
    relate_test_support::run_with_stack(2 * 1024 * 1024, || {
        // A container is one depth level; a scalar inside the innermost
        // container does not add to the count.
        let mut at_limit = vec![0x91; 128];
        at_limit.push(0xC0);
        assert!(decode(&at_limit).is_ok());
        let mut over_limit = vec![0x91; 129];
        over_limit.push(0xC0);
        assert!(decode(&over_limit).is_err());
        let mut bomb = vec![0x91; 100_000];
        bomb.push(0xC0);
        assert!(decode(&bomb).is_err());
    });
}

#[test]
fn mixed_containers_observe_same_depth_limit() {
    let mut valid = vec![0xB1, 0x01, 0xA1, 0xC0];
    valid.extend(std::iter::repeat_n(0x91, 126));
    valid.push(0xC0);
    assert!(decode(&valid).is_ok());
    valid.insert(valid.len() - 1, 0x91);
    assert!(decode(&valid).is_err());
}

#[test]
fn map32_roundtrips_and_oversized_struct_is_an_error() {
    let map = Pack::Map((0..65_536).map(|n| (Pack::Int(n), Pack::Null)).collect());
    let bytes = encode(&map).unwrap();
    assert_eq!(bytes[0], 0xDA);
    assert_eq!(decode(&bytes).unwrap(), map);
    assert!(encode(&Pack::Struct {
        tag: 0x70,
        fields: vec![Pack::Null; 16],
    })
    .is_err());
    assert!(try_struct_msg(0x70, vec![Pack::Null; 16]).is_err());
}
