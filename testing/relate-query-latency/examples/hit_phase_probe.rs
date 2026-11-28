//! The warm plan-cache hit-path probe (REL8-1423): prices the hit path
//! per statement class on the current engine — parse / plan / execute /
//! total from the always-on phase timing, plus the plan-cache outcome —
//! so a naming or admission change's per-class cost is visible before
//! and after (the REL8-2009 acceptance instrument). A dev example in the
//! `examples/` tradition (`update_bench`, `heap_census`): not CI-gated;
//! time it in a quiet window, load-stamped.
//!
//! ```text
//! cargo run --release -p relate-query-latency --example hit_phase_probe
//! ```

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use relate::{Database, Value};
// The canonical percentile (the one every other report computes).
use relate_test_support::percentile;

fn main() {
    let db = Database::open(":memory:").expect("open in-memory db");
    let mut load = db.connect().expect("connect loading session");
    for i in 0..8 {
        let stmt = "CREATE (:Person {id: $id, name: $name})".to_string();
        let mut params = BTreeMap::new();
        params.insert("id".to_string(), Value::String(format!("p{i}").into()));
        params.insert(
            "name".to_string(),
            Value::String(format!("person {i}").into()),
        );
        load.query_with_params(&stmt, params).unwrap();
    }
    drop(load);

    let mut conn = db.connect().expect("connect measuring session");
    let cases: &[(&str, &str, bool)] = &[
        // (name, statement, has_params)
        ("floor-literal", "RETURN 1 AS one", false),
        ("floor-expr", "RETURN 1 + 2 AS three", false),
        // Unaliased: the column IS the placeholder — the restore's
        // actual trigger, the path Part B replaces (REL8-2009).
        ("floor-unaliased", "RETURN 1", false),
        ("floor-expr-unaliased", "RETURN 1 + 2", false),
        ("floor-sum-unaliased", "RETURN 41 + 1", false),
        (
            "point-param",
            "MATCH (p:Person {id: $id}) RETURN p.name AS name",
            true,
        ),
        (
            "kinds-bracket",
            "MATCH (n:Person) RETURN labels(n)[0] AS kind, count(*) AS c ORDER BY kind",
            false,
        ),
        (
            "labels-plain",
            "MATCH (n:Person) RETURN labels(n)[0] AS kind",
            false,
        ),
        ("subscript", "RETURN [1,2,3][0] AS first", false),
        ("param-subscript", "RETURN [1,2,3][$idx] AS first", true),
        (
            "count-raw",
            "MATCH (n:Person) RETURN count(*) AS people",
            false,
        ),
    ];
    let id = {
        let mut params = BTreeMap::new();
        params.insert("id".to_string(), Value::String("p3".into()));
        params.insert("idx".to_string(), Value::Int(0));
        params
    };
    // Warm every case to its steady cache state, then time.
    for (name, stmt, has_params) in cases {
        for _ in 0..200 {
            let params = has_params.then(|| id.clone()).unwrap_or_default();
            black_box(conn.query_with_params(stmt, params).unwrap());
        }
        let mut parse = Vec::new();
        let mut plan = Vec::new();
        let mut execute = Vec::new();
        let mut total = Vec::new();
        let started = Instant::now();
        for _ in 0..5000 {
            let params = has_params.then(|| id.clone()).unwrap_or_default();
            let r = conn
                .query_with_params(stmt, params)
                .unwrap_or_else(|e| panic!("case {} failed: {e}", stmt));
            black_box(&r.rows);
            parse.push(r.parse_ms());
            plan.push(r.plan_ms());
            execute.push(r.execute_ms());
            total.push(r.elapsed_ms());
        }
        let secs = started.elapsed().as_secs_f64();
        let med = |v: &[f64]| percentile(v, 0.50) * 1000.0;
        let outcomes = {
            let params = has_params.then(|| id.clone()).unwrap_or_default();
            let r = conn.query_with_params(stmt, params).unwrap();
            r.plan_cache().as_str().to_string()
        };
        println!(
            "{:<14} [{:<15}] parse {:>7.2}us  plan {:>7.2}us  execute {:>7.2}us  total {:>7.2}us  \
             (unaccounted {:>6.2}us)  wall/call {:>7.2}us",
            name,
            outcomes,
            med(&parse),
            med(&plan),
            med(&execute),
            med(&total),
            med(&total) - med(&parse) - med(&plan) - med(&execute),
            secs * 1e6 / 5000.0,
        );
    }
}
