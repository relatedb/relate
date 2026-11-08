//! Always-on per-phase statement timing (ADR-0221 total, ADR-0491 split,
//! LABS-1120 fine split + plan-cache outcome): every `QueryResult` reports
//! its statement's elapsed split across PARSE (front-end parse and
//! lowering), PLAN (binding, logical planning, admission classification)
//! and EXECUTE (the dispatch — or, for EXPLAIN, the plan render), each
//! coarse phase's fine sub-windows, and which plan-cache stage served the
//! plan — measured once around `Connection::run_query`.
//!
//! Run with `cargo run -p relate --example phase_timing`.

use relate::{Database, Result};

fn main() -> Result<()> {
    let db = Database::open(":memory:")?;
    let mut conn = db.connect()?;

    // Plain runs, EXPLAIN and PROFILE all carry the split, always on.
    for stmt in [
        "CREATE (n:Person {name: 'Alice'})",
        "MATCH (n:Person) RETURN n.name AS name",
        "EXPLAIN MATCH (n:Person) RETURN n",
        "PROFILE MATCH (n:Person) RETURN n.name AS name",
    ] {
        let result = conn.query(stmt)?;
        let (parse, plan, execute, elapsed) = (
            result.parse_ms(),
            result.plan_ms(),
            result.execute_ms(),
            result.elapsed_ms(),
        );
        // The phases partition the measured window: the sum never exceeds
        // the total (the inter-stage gaps stay in the total alone).
        assert!(
            parse + plan + execute <= elapsed,
            "phases must partition the elapsed window, got parse {parse} + \
             plan {plan} + execute {execute} > elapsed {elapsed} for {stmt}",
        );
        println!(
            "{stmt} -> elapsed {elapsed:.3}ms (parse {parse:.3} / plan {plan:.3} / execute {execute:.3})"
        );

        // LABS-1120: the fine sub-split partitions the coarse phases the
        // same way — `lex_parse + rewrite + lower` never exceeds `parse`,
        // `result_generation` never exceeds `execute` — and the
        // plan-cache outcome rides beside them.
        let (lex_parse, rewrite, lower, result_generation) = (
            result.lex_parse_ms(),
            result.rewrite_ms(),
            result.lower_ms(),
            result.result_generation_ms(),
        );
        assert!(
            lex_parse + rewrite + lower <= parse,
            "the fine parse windows must partition parse, got lex_parse \
             {lex_parse} + rewrite {rewrite} + lower {lower} > parse {parse} \
             for {stmt}",
        );
        assert!(
            result_generation <= execute,
            "result generation must sit inside execute, got \
             {result_generation} > execute {execute} for {stmt}",
        );
        println!(
            "  fine: lex+parse {lex_parse:.3} / rewrite {rewrite:.3} / lower \
             {lower:.3}; result generation {result_generation:.3}; plan cache \
             {}",
            result.plan_cache().as_str(),
        );

        // The wire envelope's timing block: the one projection bundling the
        // total and its split into the stamp the canonical envelope builder
        // takes (`wire::TimingStampMs`, ADR-0491 + LABS-1120) — the same
        // numbers, never a second conversion.
        let stamp = relate::timing_stamp_ms(&result);
        assert_eq!(stamp.parse_ms, parse);
        assert_eq!(stamp.plan_ms, plan);
        assert_eq!(stamp.execute_ms, execute);
        assert_eq!(stamp.elapsed_ms, elapsed);
        assert_eq!(stamp.lex_parse_ms, lex_parse);
        assert_eq!(stamp.plan_cache, result.plan_cache().as_str());
    }

    // The plan-cache outcome, observed where it matters: a fresh read's
    // first run is a miss; the second run of the same text is a raw
    // fast-path hit — its parse window collapses to the lookup and its
    // fine parse sub-windows are the true zero, which is exactly what the
    // outcome explains. (A statement the loop above did not already run —
    // the loop's own reads populated their entries.)
    let read = "MATCH (n:Person {name: 'Alice'}) RETURN n.name AS name";
    let first = conn.query(read)?;
    assert_eq!(first.plan_cache().as_str(), "miss");
    let hit = conn.query(read)?;
    assert_eq!(hit.plan_cache().as_str(), "hit-raw-fastpath");
    assert_eq!(hit.lex_parse_ms(), 0.0, "a hit parses nothing");
    assert!(
        hit.parse_ms() < first.parse_ms(),
        "the hit's parse window ({}) is smaller than the first compile's \
         ({}) — the cache served it",
        hit.parse_ms(),
        first.parse_ms(),
    );
    Ok(())
}
