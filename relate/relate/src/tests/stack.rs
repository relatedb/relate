//! ADR-0259: every query runs on the connection's 64 MiB worker, spawned on
//! the first query and reused after that, so the calling thread's stack size
//! never matters. At-cap input lowers/plans/executes successfully on the
//! worker and beyond-cap input returns typed errors — never an abort — even
//! when the caller runs on a 2 MiB stack (tokio's worker default), and a
//! worker panic is caught and returned as a typed internal error. The same
//! worker then serves the next statement.
//!
//! ADR-0504 (the connection-thread fast path): a connection whose caller
//! asserted the worker's own stack budget runs bounded-work statements
//! inline on the calling thread — the tests below prove the ADR-0259
//! guarantees hold on that path too: the depth cap's margin is the same
//! stack budget (at-cap input succeeds, beyond-cap input errors typed,
//! inline, on a 64 MiB caller), a panic is contained to the same typed
//! internal error class, and the statements that could outlive a
//! keep-alive interval still take the worker.
//!
//! ADR-0663 (the cache-hit inline route): a connection whose caller never
//! asserted anything still runs the point-lookup family inline from its
//! second execution — the plan cache carries the plan, a hit skips the
//! front (ADR-0651), and the shallow-bounded predicate is the
//! stack-safety fact. The route's tests below prove it on the 2 MiB
//! caller stack the worker existed to protect, and that everything cold,
//! deep or unbounded stays on the worker.

use crate::{Database, Error};
use relate_test_support::run_with_stack;

/// Spawn a caller thread with a 2 MiB stack (tokio's documented worker
/// default, and the size on which release builds used to SIGABRT below the
/// ADR-0257 depth cap) and run `f` on it.
fn on_small_stack_caller<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    run_with_stack(2 * 1024 * 1024, f)
}

/// The parser's nesting cap (ADR-0257) — the number the at-cap and
/// beyond-cap fixtures derive from, never a hand-evaluated copy.
const MAX_DEPTH: u32 = relate_parser::parser::MAX_NESTING_DEPTH;

/// One nested-parenthesis expression of the given depth.
fn parens(depth: usize) -> String {
    format!("RETURN {}1{}", "(".repeat(depth), ")".repeat(depth))
}

/// Spawn a caller thread with the query worker's own 64 MiB budget
/// (ADR-0259's `QUERY_WORKER_STACK_BYTES`, the canonical constant) and run
/// `f` on it — the stack a fast-path-asserting host's connection thread
/// carries (the Bolt surface, ADR-0233), and the one on which the inline
/// tests below prove the ADR-0259 guarantees.
fn on_worker_grade_stack_caller<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    run_with_stack(crate::QUERY_WORKER_STACK_BYTES, f)
}

/// Beyond-cap inputs return typed errors through the public query path even
/// when the caller's own stack is 2 MiB: the input-proportional recursion
/// runs on the engine's 64 MiB worker, so the ADR-0257 depth guard fires
/// long before any stack is exhausted. If this test aborts the process
/// instead of failing, the worker-thread guarantee is broken.
///
/// Covers all eight guarded grammar classes of the parser suite
/// (`test_parse_*_at_and_beyond_depth_limit` in
/// `relate/relate-parser/tests/limits.rs`) plus property chains (the
/// front-end suite's guarded class): nested expression parens, `NOT`
/// chains, unary `-` chains, label `!` chains, nested label parens, nested
/// subpath parens, nested `FOREACH`, nested value types (`LIST<…>` via both
/// the `::TYPE` cast and the `IS ::` predicate spellings), and deep
/// property chains.
#[test]
fn beyond_cap_queries_error_typed_on_two_mib_caller_stack() {
    let cases: Vec<(String, String)> = on_small_stack_caller(|| {
        // Nested parentheses: used to SIGABRT parse+lower at ~310–320 on a
        // 2 MiB release stack, below the 500 cap.
        let parens = format!("RETURN {}1{}", "(".repeat(5000), ")".repeat(5000));
        // Nested NOT: used to abort front-end lowering at ~1000.
        let nots = format!("RETURN {}true", "NOT ".repeat(2000));
        // Nested unary minus.
        let minuses = format!("RETURN {}1", "-".repeat(2000));
        // Nested label `!` chain.
        let label_bangs = format!("MATCH (n:{}A) RETURN n", "!".repeat(600));
        // Nested label parens.
        let label_parens = format!("MATCH (n:{}A{}) RETURN n", "(".repeat(600), ")".repeat(600));
        // Nested subpath parens.
        let subpath_parens = format!("MATCH {}a{} RETURN a", "(".repeat(600), ")".repeat(600));
        // Nested FOREACH: used to SIGABRT at ~480 on a 2 MiB release stack.
        let mut foreach = String::new();
        for i in 0..600 {
            foreach.push_str(&format!("FOREACH (x{i} IN [1] | "));
        }
        foreach.push_str("CREATE (n)");
        foreach.push_str(&")".repeat(600));
        // Nested value types, `::TYPE` cast spelling.
        let value_type_cast = format!(
            "RETURN 1::{}INTEGER{} AS x",
            "LIST<".repeat(600),
            ">".repeat(600)
        );
        // Nested value types, `IS ::` type-predicate spelling.
        let value_type_predicate = format!(
            "RETURN 1 IS :: {}INTEGER{}",
            "LIST<".repeat(600),
            ">".repeat(600)
        );
        // Deep property chain.
        let chain = format!("RETURN x{}", ".a".repeat(16 * 1024));

        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        [
            parens,
            nots,
            minuses,
            label_bangs,
            label_parens,
            subpath_parens,
            foreach,
            value_type_cast,
            value_type_predicate,
            chain,
        ]
        .into_iter()
        .map(|q| {
            let outcome = match conn.query(&q) {
                Ok(_) => "unexpected ok".to_string(),
                Err(e) => format!("{e:?}"),
            };
            (q.chars().take(40).collect(), outcome)
        })
        .collect()
    });

    for (prefix, outcome) in &cases {
        assert!(
            outcome.starts_with("Syntax(") || outcome.starts_with("Semantic("),
            "beyond-cap query `{prefix}…` must produce a typed error, got: {outcome}"
        );
        assert!(
            outcome.contains("maximum nesting depth"),
            "beyond-cap query `{prefix}…` must name the ADR-0257 limit, got: {outcome}"
        );
    }
}

/// At-cap inputs parse, lower, plan, and execute successfully through the
/// public query path even when the caller's own stack is 2 MiB: the
/// recursion runs on the engine's 64 MiB worker, where the ADR-0257 cap of
/// 500 was probe-verified to hold with 3.3×–48× margin. This is the
/// end-to-end proof that 500 is a sound cap on the worker — the beyond-cap
/// tests alone only exercise the parse guard. Depth arithmetic mirrors the
/// parser suite's at-limit cases; the property chain uses 450, the
/// front-end suite's under-cap value
/// (`property_chain_just_under_the_limit_lowers` in
/// `relate/relate-front/src/lower.rs`).
#[test]
fn at_cap_queries_succeed_on_two_mib_caller_stack() {
    on_small_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();

        // 499-deep NOT chain: the enclosing RETURN expression occupies
        // depth 1, so 499 NOTs sit exactly at the limit.
        let nots = format!("RETURN {}true", "NOT ".repeat(499));
        // 499-deep parenthesised expression, same arithmetic.
        let parens = format!("RETURN {}1{}", "(".repeat(499), ")".repeat(499));
        // Just-under-limit FOREACH nest: 499 nested FOREACH clauses plus the
        // innermost CREATE body sit exactly at the limit.
        let foreach = format!(
            "{}CREATE (:A){}",
            "FOREACH (x IN [1] | ".repeat(499),
            ")".repeat(499)
        );
        // 450-hop property chain over a null map: evaluates to null.
        let chain = format!("WITH {{a: null}} AS n RETURN n{}", ".a".repeat(450));

        for q in [&nots, &parens, &foreach, &chain] {
            let prefix: String = q.chars().take(40).collect();
            conn.query(q)
                .unwrap_or_else(|e| panic!("at-cap query `{prefix}…` must succeed, got: {e:?}"));
        }

        // Spot-check the results, not just the absence of an error.
        let result = conn.query(&parens).unwrap();
        assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
        let result = conn.query(&nots).unwrap();
        assert_eq!(result.rows, vec![vec![crate::Value::Bool(false)]]);
    });
}

/// The engine stays usable after a beyond-cap query on a small-stack caller.
#[test]
fn engine_usable_after_beyond_cap_query_on_two_mib_caller() {
    on_small_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        let deep = format!("RETURN {}1{}", "(".repeat(5000), ")".repeat(5000));
        assert!(conn.query(&deep).is_err());
        let result = conn.query("RETURN 1 AS n").unwrap();
        assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
    });
}

/// Several statements on one connection spawn one worker.
#[test]
fn one_connection_reuses_one_worker() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    assert_eq!(conn.query_worker_spawns(), 0);
    for _ in 0..4 {
        let result = conn.query("RETURN 1 AS n").unwrap();
        assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
    }
    assert_eq!(conn.query_worker_spawns(), 1);
}

/// A panic inside the connection's worker is a typed internal error, and
/// the same connection can run the next statement on that same worker.
#[test]
fn connection_survives_worker_panic() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let result = conn.drive_query_worker(|| panic!("injected worker panic"));
    match result {
        Err(Error::Internal(msg)) => {
            assert!(
                msg.contains("query worker thread panicked"),
                "unexpected message: {msg}"
            );
            assert!(msg.contains("injected worker panic"), "payload lost: {msg}");
        }
        other => panic!("expected typed internal error, got: {other:?}"),
    }
    assert_eq!(conn.query_worker_spawns(), 1);
    let result = conn.query("RETURN 1 AS n").unwrap();
    assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
    assert_eq!(conn.query_worker_spawns(), 1);
}

/// A panic inside the worker is caught and mapped to a typed internal
/// error; it never unwinds into or aborts the host. The same worker then
/// serves the next call.
#[test]
fn worker_panic_maps_to_typed_internal_error() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let result = conn.drive_query_worker(|| panic!("injected worker panic"));
    match result {
        Err(Error::Internal(msg)) => {
            assert!(
                msg.contains("query worker thread panicked"),
                "unexpected message: {msg}"
            );
            assert!(msg.contains("injected worker panic"), "payload lost: {msg}");
        }
        other => panic!("expected typed internal error, got: {other:?}"),
    }
    // The caller is still alive and the same worker still returns a value.
    assert_eq!(conn.drive_query_worker(|| 42u32).unwrap(), 42);
}

/// Worker results and ordinary query errors pass through untouched.
#[test]
fn worker_passes_through_results_and_errors() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let err = conn
        .drive_query_worker(|| Err::<u32, _>(Error::semantic("plain failure")))
        .unwrap();
    assert!(matches!(err, Err(Error::Semantic(_))));
    assert_eq!(conn.drive_query_worker(|| 7u32).unwrap(), 7);
}

/// ADR-0462 (LABS-606): a value nesting bomb — a `reduce` accumulator or a
/// chain of `WITH [a] AS a` wraps that grows one container level per
/// iteration — fails with a typed `ValueDepthExceeded` execution error at
/// level 501 instead of building a value whose later recursive walks
/// overflow the stack. The engine stays usable, and the failure reproduces
/// on a 2 MiB caller stack: the check is iterative and runs on the worker,
/// so no recursion proportional to the value's depth happens anywhere.
#[test]
fn value_depth_bombs_error_typed_on_two_mib_caller_stack() {
    on_small_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();

        // `WITH [] AS a WITH [a] AS a …` — one new list level per clause.
        let mut withs = String::from("WITH [] AS a ");
        for _ in 0..600 {
            withs.push_str("WITH [a] AS a ");
        }
        withs.push_str("RETURN a AS a");

        for (name, query) in [
            ("WITH wraps", withs),
            (
                "reduce accumulator",
                "RETURN reduce(acc = [], x IN range(1, 600) | [acc]) AS a".to_string(),
            ),
        ] {
            match conn.query(&query) {
                Err(Error::Execution(msg)) => {
                    assert!(
                        msg.starts_with("ValueDepthExceeded:"),
                        "{name}: typed payload expected, got: {msg}"
                    );
                    assert!(
                        msg.contains("500"),
                        "{name}: the message must name the limit: {msg}"
                    );
                }
                Ok(_) => panic!("{name}: the value-depth bomb must not succeed"),
                Err(other) => panic!("{name}: expected a typed execution error, got: {other:?}"),
            }
            // The connection survives the refusal and answers the next
            // statement.
            let result = conn.query("RETURN 1 AS n").unwrap();
            assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
        }
    });
}

/// ADR-0462: exactly [`crate::MAX_VALUE_DEPTH`] container levels are
/// accepted and one more is rejected, measured through the public query
/// path. An at-cap value then renders (JSON, Cypher literal) and drops
/// without recursion trouble; both halves run release-mode-safe under the
/// 2 MiB caller.
#[test]
fn value_depth_limit_is_500_through_the_query_path() {
    on_small_stack_caller(|| {
        use crate::MAX_VALUE_DEPTH;

        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();

        // `[]` is level 1; 499 wraps make 500 — accepted.
        let at_cap = format!(
            "RETURN reduce(acc = [], x IN range(1, {}) | [acc]) AS a",
            MAX_VALUE_DEPTH - 1
        );
        let result = conn.query(&at_cap).unwrap();
        let value = &result.rows[0][0];
        assert!(value.depth_within(MAX_VALUE_DEPTH));
        assert!(!value.depth_within(MAX_VALUE_DEPTH - 1));
        // The recursive render walks over the at-cap value succeed on a
        // 2 MiB caller stack (the drop follows when `result` falls out of
        // scope).
        assert!(value.to_json_plain().is_array());
        assert!(value.to_cypher_literal().starts_with('['));

        // One more level is refused.
        let over_cap =
            format!("RETURN reduce(acc = [], x IN range(1, {MAX_VALUE_DEPTH}) | [acc]) AS a");
        match conn.query(&over_cap) {
            Err(Error::Execution(msg)) => assert!(msg.starts_with("ValueDepthExceeded:")),
            Ok(_) => panic!("{MAX_VALUE_DEPTH} + 1 levels must be rejected"),
            Err(other) => panic!("expected a typed execution error, got: {other:?}"),
        }
    });
}

/// ADR-0462 regression (PR #547 review): the collect family — plain
/// `collect()`, the horizontal-aggregate form and `COLLECT { … }` — is a
/// dynamic container construction like any other. A chain of clauses where
/// each `collect` wraps the previous value in one more list must hit the
/// typed depth error at level 501, not build an over-cap value that later
/// overflows the result heap or the derived drop/clone walks.
#[test]
fn collect_chains_error_typed_at_the_value_depth_cap() {
    on_small_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();

        let chains = [
            ("collect", "WITH collect(a) AS a "),
            ("COLLECT subquery", "WITH COLLECT { RETURN a } AS a "),
            // The horizontal-aggregate rewrite triggers on a list variable
            // argument (`collect(xs)`), so `[a]` makes it collect one
            // element — one more level.
            (
                "horizontal collect",
                "WITH [a] AS xs WITH collect(xs) AS a ",
            ),
        ];
        for (name, clause) in chains {
            let mut query = String::from("WITH [] AS a ");
            for _ in 0..600 {
                query.push_str(clause);
            }
            query.push_str("RETURN a AS a");
            match conn.query(&query) {
                Err(Error::Execution(msg)) => {
                    assert!(
                        msg.starts_with("ValueDepthExceeded:"),
                        "{name}: typed payload expected, got: {msg}"
                    );
                }
                Ok(_) => panic!("{name}: the collect chain must not build a value past the cap"),
                Err(other) => panic!("{name}: expected a typed execution error, got: {other:?}"),
            }
            let result = conn.query("RETURN 1 AS n").unwrap();
            assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
        }
    });
}

// ADR-0504: the connection-thread fast path. The calling thread of every
// test below carries the worker's own 64 MiB budget
// (`on_worker_grade_stack_caller`) — the precondition
// `assume_worker_stack_on_caller_thread` asserts — so these tests prove
// the fast path on a stack that honestly satisfies it.

/// ADR-0504: on an asserted connection, bounded-work READS run on the
/// calling thread — the worker is never spawned. This is the observable
/// core of the fast path: `worker_spawns` stays at zero across point
/// reads, joined reads and admin commands — and the first write (every
/// write escalates: an auto-commit write must never commit after its
/// client gave up waiting) spawns it.
#[test]
fn asserted_connection_runs_bounded_reads_inline() {
    on_worker_grade_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        // Seed through a separate, unasserted connection so the fixture's
        // own writes do not spawn the worker under test.
        let mut seed = db.connect().unwrap();
        seed.query("CREATE (:User {id: 1})").unwrap();
        seed.query("CREATE RANGE INDEX user_id FOR (u:User) ON (u.id)")
            .unwrap();
        drop(seed);

        let mut conn = db.connect().unwrap();
        conn.assume_worker_stack_on_caller_thread();
        for cypher in [
            "RETURN 1 AS n",
            "MATCH (n:User) WHERE n.id = 1 RETURN n.id",
            "MATCH (n:User {id: 1}) RETURN n.id",
            "SHOW DATABASES",
            "SHOW INDEXES",
            "USE neo4j MATCH (n:User) RETURN n",
        ] {
            conn.query(cypher)
                .unwrap_or_else(|e| panic!("`{cypher}` must run, got: {e:?}"));
        }
        assert_eq!(
            conn.query_worker_spawns(),
            0,
            "bounded reads must never spawn the query worker"
        );
        // Spot-check the results, not just the absence of errors.
        let result = conn
            .query("MATCH (n:User) WHERE n.id = 1 RETURN n.id")
            .unwrap();
        assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
        // The reads-only boundary (ADR-0504): every write escalates, so
        // the first write spawns the worker.
        conn.query("CREATE (:User {id: 2})").unwrap();
        assert_eq!(conn.query_worker_spawns(), 1, "writes must escalate");
        // Reads keep flowing inline after the escalation, same worker.
        conn.query("RETURN 2 AS n").unwrap();
        assert_eq!(conn.query_worker_spawns(), 1);
    });
}

/// ADR-0504: a statement that could outlive a keep-alive interval
/// (UNWIND's unbounded row potential) escalates to the worker on an
/// asserted connection — spawning it exactly once — and the connection
/// keeps serving inline work afterwards without spawning another.
#[test]
fn asserted_connection_escalates_unbounded_work_to_the_worker() {
    on_worker_grade_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.assume_worker_stack_on_caller_thread();
        conn.query("RETURN 1 AS n").unwrap();
        assert_eq!(conn.query_worker_spawns(), 0);
        // UNWIND: unbounded row potential, never fast-path eligible.
        let result = conn.query("UNWIND range(1, 4) AS x RETURN x").unwrap();
        assert_eq!(
            result.rows,
            vec![
                vec![crate::Value::Int(1)],
                vec![crate::Value::Int(2)],
                vec![crate::Value::Int(3)],
                vec![crate::Value::Int(4)],
            ]
        );
        assert_eq!(
            conn.query_worker_spawns(),
            1,
            "escalation spawns the worker"
        );
        // Inline work keeps flowing after the escalation, same worker.
        conn.query("RETURN 2 AS n").unwrap();
        assert_eq!(conn.query_worker_spawns(), 1);
    });
}

/// ADR-0259's guarantee on the fast path (ADR-0504): at-cap input succeeds
/// and beyond-cap input returns the typed ADR-0257 error — inline, on a
/// caller carrying the worker's own 64 MiB stack. Deep expression nesting
/// is exactly the recursion the worker exists for, and the fast path is
/// safe for it because the asserted stack IS the worker's budget — the
/// same probe-verified margin, same depth-500 cap. If this test aborts
/// the process instead of failing, the fast-path boundary is broken.
#[test]
fn at_and_beyond_cap_queries_run_inline_on_worker_grade_caller() {
    on_worker_grade_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.assume_worker_stack_on_caller_thread();

        // At-cap: one inside the cap — succeeds inline (the statement
        // is bounded work: no clause that escalates).
        let at_cap = parens((MAX_DEPTH - 1) as usize);
        let result = conn.query(&at_cap).unwrap();
        assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
        // Beyond-cap: well past the cap — the typed error names the
        // ADR-0257 limit, inline.
        let deep = parens(MAX_DEPTH as usize * 10);
        match conn.query(&deep) {
            Err(e) => assert!(
                format!("{e:?}").contains("maximum nesting depth"),
                "beyond-cap must name the ADR-0257 limit, got: {e:?}"
            ),
            Ok(_) => panic!("beyond-cap query must not succeed"),
        }
        // The connection stays usable, and inline work keeps flowing.
        let result = conn.query("RETURN 1 AS n").unwrap();
        assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
        assert_eq!(conn.query_worker_spawns(), 0);
    });
}

/// ADR-0259's panic containment on the fast path (ADR-0504): a panicking
/// statement caught on the caller's thread becomes the same typed
/// internal-error class the worker reports, and the connection serves the
/// next statement — a panic must never unwind the session's thread.
#[test]
fn asserted_connection_contains_an_inline_panic() {
    on_worker_grade_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.assume_worker_stack_on_caller_thread();
        let result = crate::catch_inline_panic(|| panic!("injected inline panic"));
        match result {
            Err(Error::Internal(msg)) => {
                assert!(
                    msg.contains("query panicked on the caller's thread"),
                    "unexpected message: {msg}"
                );
                assert!(msg.contains("injected inline panic"), "payload lost: {msg}");
            }
            other => panic!("expected typed internal error, got: {other:?}"),
        }
        // The caller is still alive and the connection still answers.
        let result = conn.query("RETURN 1 AS n").unwrap();
        assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
    });
}

/// ADR-0259's cancellation semantics are identical on both paths
/// (ADR-0504): the token is checked by the executor's own entry points,
/// which run wherever the statement runs — an already-cancelled token
/// fails the statement with the same typed error inline and on the
/// worker, and the connection stays usable either way.
#[test]
fn cancelled_token_fails_statements_identically_on_both_paths() {
    on_worker_grade_stack_caller(|| {
        use crate::CancelToken;

        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.assume_worker_stack_on_caller_thread();

        // Inline path: an eligible statement under a cancelled token.
        let token = CancelToken::new();
        token.cancel();
        let err = conn
            .query_with_params_cancellable(
                "RETURN 1 AS n",
                std::collections::BTreeMap::new(),
                token,
            )
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "inline: {err:?}");
        // Escalated path: an UNWIND statement under a cancelled token.
        let token = CancelToken::new();
        token.cancel();
        let err = conn
            .query_with_params_cancellable(
                "UNWIND range(1, 4) AS x RETURN x",
                std::collections::BTreeMap::new(),
                token,
            )
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "escalated: {err:?}");
        // Both paths leave the connection usable.
        let result = conn.query("RETURN 1 AS n").unwrap();
        assert_eq!(result.rows, vec![vec![crate::Value::Int(1)]]);
    });
}

/// A nested-addition expression of the given depth over a parameter — a
/// bounded-work statement whose expression tree neither collapses at
/// lowering (parentheses do) nor constant-folds (literals do), so its
/// plan is deeper than the shallow bound and its cache hits must stay
/// on the worker (ADR-0663's stack-safety half).
fn nested_add(depth: usize) -> String {
    format!("RETURN {}$x{}", "$x+(".repeat(depth), ")".repeat(depth))
}

/// ADR-0663: the point-lookup family runs inline from its second
/// execution on a connection whose caller never asserted a stack budget —
/// proven on the 2 MiB stack the query worker exists to protect. The
/// first execution is cold (the worker parses); every hit after that is
/// the inline route, binding-invariant, answering exactly what the worker
/// answered — the point read, the one-fixed-hop read, and the count fold.
#[test]
fn cache_hits_of_the_point_lookup_family_run_inline_on_a_small_caller_stack() {
    use crate::Value;
    use std::collections::BTreeMap;
    on_small_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.query(
            "CREATE (:Commit {id: 'c1', position: 7}), \
                   (:Commit {id: 'c2', position: 9}), \
                   (c:Build {id: 'b1'})-[:R]->(:Artifact {id: 'c1'})",
        )
        .unwrap();
        let point = "MATCH (c:Commit {id: $id}) RETURN c.position AS position";
        let one_hop = "MATCH (b:Build {id: $id})-[:R]->(a) RETURN a.id AS artifact";
        let fold = "MATCH (c:Commit) WHERE c.position > $min RETURN count(c.id) AS c";
        for (cypher, binding, key, expected) in [
            (
                point,
                BTreeMap::from([("id".to_string(), Value::String("c1".into()))]),
                "id",
                vec![vec![Value::Int(7)]],
            ),
            (
                one_hop,
                BTreeMap::from([("id".to_string(), Value::String("b1".into()))]),
                "id",
                vec![vec![Value::String("c1".into())]],
            ),
            (
                fold,
                BTreeMap::from([("min".to_string(), Value::Int(5))]),
                "min",
                vec![vec![Value::Int(2)]],
            ),
        ] {
            // The counter is cumulative across the connection, so each
            // phase reads as a delta against the statements before it.
            let before = conn.hit_inline_served();
            let first = conn.query_with_params(cypher, binding.clone()).unwrap();
            assert_eq!(
                conn.hit_inline_served(),
                before,
                "the first execution is cold — the worker parses: {cypher}"
            );
            assert_eq!(first.rows, expected, "the worker answers: {cypher}");
            let second = conn.query_with_params(cypher, binding.clone()).unwrap();
            assert_eq!(
                conn.hit_inline_served(),
                before + 1,
                "the cache hit runs inline: {cypher}"
            );
            assert_eq!(
                second.rows, expected,
                "the inline hit answers the worker: {cypher}"
            );
            // A different binding over the same template is still a hit —
            // the route is binding-invariant, the plan cache's own
            // contract. The swap key rides the fixture tuple: the
            // binding-invariance assert is unconditional, never skipped.
            let mut other = binding.clone();
            other.insert(key.to_string(), Value::String("zz".into()));
            conn.query_with_params(cypher, other).unwrap();
            assert_eq!(
                conn.hit_inline_served(),
                before + 2,
                "the hit serves any binding: {cypher}"
            );
        }
    })
}

/// ADR-0663's own walk depth, pinned: the probe measures nesting on the
/// calling thread, so its measurement walks must fit a small stack on
/// the deepest statement the parser admits — a 499-deep NOT chain, the
/// deepest *bounded-work* shape (its plan keeps the chain; parentheses
/// collapse). The hit declines on the depth conjunct and the probe's
/// walks complete on this 2 MiB stack with room to spare — the pin that
/// keeps the depth-measuring walks honest as they evolve.
#[test]
fn the_probes_depth_measurement_fits_a_small_stack_on_the_deepest_plan() {
    // The margin pin first: the deepest bounded plan's probe walks fit
    // a 384 KiB stack — 5x below the 2 MiB floor this route promises
    // (measured: the walks' frames are ~350 bytes, not the parser's
    // ~6 KiB). Pinning the margin catches a walk-frame regression five
    // times before it can break the contract below.
    relate_test_support::run_with_stack(384 * 1024, || {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        let nots = format!("RETURN {}$x", "NOT ".repeat(499));
        let params =
            std::collections::BTreeMap::from([("x".to_string(), crate::Value::Bool(true))]);
        conn.query_with_params(&nots, params.clone()).unwrap();
        let second = conn.query_with_params(&nots, params).unwrap();
        assert_eq!(second.rows, vec![vec![crate::Value::Bool(false)]]);
        assert_eq!(conn.hit_inline_served(), 0);
    });
    on_small_stack_caller(|| {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        // Over a parameter, so neither the front nor the planner can
        // fold the chain away: the cached plan keeps all 499 levels.
        let nots = format!("RETURN {}$x", "NOT ".repeat(499));
        let params =
            std::collections::BTreeMap::from([("x".to_string(), crate::Value::Bool(true))]);
        conn.query_with_params(&nots, params.clone()).unwrap();
        let second = conn.query_with_params(&nots, params).unwrap();
        assert_eq!(second.rows, vec![vec![crate::Value::Bool(false)]]);
        assert_eq!(
            conn.hit_inline_served(),
            0,
            "the depth conjunct declines the deepest bounded plan"
        );
    })
}

/// ADR-0663's exclusions: a write is never inline (cold every time), a
/// var-length statement is never bounded work (its hits stay on the
/// worker), and a deep-expression bounded statement's hits stay on the
/// worker too — the depth bound is the stack-safety half of the route's
/// plan fact.
#[test]
fn cold_deep_and_unbounded_statements_stay_on_the_worker() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (a:X {id: 'a'})-[:T]->(b:X {id: 'b'})")
        .unwrap();
    assert_eq!(
        conn.hit_inline_served(),
        0,
        "a write is cold: the worker serves"
    );
    conn.query("CREATE (:N)").unwrap();
    assert_eq!(conn.hit_inline_served(), 0, "every write is cold");
    // Var-length: bounded work refuses the shape, so even its cache hits
    // take the worker.
    let varlen = "MATCH (a:X {id: $id})-[:T*]->(b) RETURN count(b) AS c";
    let params =
        std::collections::BTreeMap::from([("id".to_string(), crate::Value::String("a".into()))]);
    conn.query_with_params(varlen, params.clone()).unwrap();
    conn.query_with_params(varlen, params).unwrap();
    assert_eq!(
        conn.hit_inline_served(),
        0,
        "a var-length statement's hits stay on the worker"
    );
    // Deep expressions: bounded work, but deeper than the shallow bound.
    let deep = nested_add(20);
    let deep_params = std::collections::BTreeMap::from([("x".to_string(), crate::Value::Int(1))]);
    conn.query_with_params(&deep, deep_params.clone()).unwrap();
    conn.query_with_params(&deep, deep_params).unwrap();
    assert_eq!(
        conn.hit_inline_served(),
        0,
        "a deep-expression hit stays on the worker"
    );
}
