//! LABS-832 / ADR-0491: the engine measures the PARSE / PLAN / EXECUTE
//! split of every statement always-on, and the canonical wire envelope
//! (`relate::query_result_to_wire`, the one construction the C ABI and
//! the WASM surface render) carries it beside `elapsed_ms` as
//! `parse_ms` / `plan_ms` / `execute_ms`.

mod common;

use relate::query_result_to_wire;

/// The envelope re-sources each phase from the header's own `Duration`
/// through the one Duration→ms conversion behind the accessors — bit-exact
/// like the `elapsed_ms` re-sourcing check, never a second conversion
/// site — on a plain read, a write, an EXPLAIN (whose execute-equivalent
/// stage is the plan render) and a PROFILE alike.
#[test]
fn wire_envelope_carries_the_phase_split() {
    let mut conn = common::connection();
    conn.query("CREATE (:P {x: 1})").unwrap();
    for stmt in [
        "MATCH (n:P) RETURN n.x AS x",
        "CREATE (:P {x: 2})",
        "EXPLAIN MATCH (n:P) RETURN n",
        "PROFILE MATCH (n:P) RETURN n.x AS x",
    ] {
        let r = common::run(&mut conn, stmt);
        let env = query_result_to_wire(&r).unwrap();
        for (key, re_sourced) in [
            ("parse_ms", r.parse_ms()),
            ("plan_ms", r.plan_ms()),
            ("execute_ms", r.execute_ms()),
        ] {
            let got = env[key]
                .as_f64()
                .unwrap_or_else(|| panic!("envelope carries {key} for {stmt}"));
            // serde_json round-trips an f64 bit-exactly, so the envelope
            // value is the accessor's conversion to the bit.
            assert_eq!(
                got.to_bits(),
                re_sourced.to_bits(),
                "envelope {key} re-sources the header phase for {stmt}"
            );
        }
    }
}

/// The split partitions the total's window (ADR-0491): the three keys sum
/// to no more than `elapsed_ms` — the inter-stage gaps stay in the total
/// alone, never the reverse.
#[test]
fn wire_envelope_phase_split_never_exceeds_elapsed_ms() {
    let mut conn = common::connection();
    conn.query("CREATE (:P {x: 1})").unwrap();
    let r = common::run(&mut conn, "MATCH (n:P) WHERE n.x = 1 RETURN n.x AS x");
    let env = query_result_to_wire(&r).unwrap();
    let parse = env["parse_ms"].as_f64().expect("parse_ms");
    let plan = env["plan_ms"].as_f64().expect("plan_ms");
    let execute = env["execute_ms"].as_f64().expect("execute_ms");
    let elapsed = env["elapsed_ms"].as_f64().expect("elapsed_ms");
    assert!(
        parse + plan + execute <= elapsed,
        "parse {parse} + plan {plan} + execute {execute} exceeds elapsed {elapsed}"
    );
}
