//! Complexity regression guards (LABS-81): parse shapes that used to be
//! exponential or quadratic must complete in near-linear time. Public
//! parser API only.

use relate_names::SmallName;
use relate_parser::parser::MAX_QUERY_BYTES;
use relate_parser::{parse, Clause, Expr, LabelExpr, PatternElement};
use relate_test_support::run_with_big_stack;

#[test]
fn test_pattern_comprehension_probe_nested_is_not_exponential() {
    // Complexity regression guard (LABS-183): the speculative
    // `parse_pattern_path` probe behind `[pathVar = <pattern> | ...]`
    // (`pattern_comprehension_follows_equals`) re-parsed -- probe plus
    // committing re-parse -- the whole nested term at every level of a
    // nested `[v = (n {p: ...}) | v]` shape, giving 2^k parse cost on a
    // sub-KB query (measured before the per-position verdict memo:
    // release 0.74 s at k=20, 2.96 s at k=22, 11.9 s at k=24; debug
    // 3.38 s at k=20 -- a parse-time DoS well under MAX_QUERY_BYTES).
    // k=26 puts the unfixed parse at ~48 s release / ~3.5 min debug,
    // both past the budget below. With the memo each `=` position is
    // probed once, so the probe+re-parse work is O(k^2) and this parse
    // completes in milliseconds even in a debug build (~0.2 ms release
    // at any k).
    const K: usize = 26;
    let mut expr = String::from("[v = (n) | v]");
    for _ in 1..K {
        expr = format!("[v = (n {{p: {expr}}}) | v]");
    }
    let query = format!("RETURN {expr}");
    assert!(query.len() <= MAX_QUERY_BYTES);
    let start = std::time::Instant::now();
    let stmt = parse(&query).unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "nested pattern-comprehension parse took {elapsed:?} (exponential probing?)"
    );
    // AST spot-check: every level is a pattern comprehension with path
    // variable `v`, a bare `(n)` pattern holding the next level as its
    // `p` property, and `v` as the map expression.
    let Clause::Return(r) = &stmt.clauses[0] else {
        panic!("expected return");
    };
    let mut cur = &r.items[0].expr;
    for level in 0..K {
        let Expr::PatternComprehension(pc) = cur else {
            panic!("expected PatternComprehension at level {level}, got {cur:?}")
        };
        assert_eq!(
            pc.path_variable.as_ref(),
            Some(&SmallName::from_name("v")),
            "level {level}"
        );
        assert!(
            matches!(&pc.map_expr, Expr::Variable(v) if v == "v"),
            "level {level}"
        );
        assert_eq!(pc.pattern.elements.len(), 1, "level {level}");
        let PatternElement::Node(node) = &pc.pattern.elements[0] else {
            panic!("expected node pattern at level {level}")
        };
        assert_eq!(
            node.variable.as_ref(),
            Some(&SmallName::from_name("n")),
            "level {level}"
        );
        if level + 1 < K {
            assert_eq!(node.properties.len(), 1, "level {level}");
            assert_eq!(node.properties[0].0, "p", "level {level}");
            cur = &node.properties[0].1;
        }
    }
}

#[test]
fn test_list_comprehension_nested_label_disjunction_probe_is_not_exponential() {
    // Adversarial nesting: k parenthesized `(a|...)` disjunction levels
    // inside a comprehension WHERE. Before the comprehension
    // pipe probe was memoized per `|` position, each level's speculative probe
    // re-parsed (and re-probed) the whole nested term, giving 2^k
    // speculative parses (measured ~13 s at k=26 in release). At k=128
    // that path never terminates; with the memo the probe+re-parse work
    // is O(k^2), so this parse completes in milliseconds even in debug
    // builds. k=128 also stays comfortably under MAX_NESTING_DEPTH.
    const K: usize = 128;
    let mut label_expr = String::from("a");
    for _ in 0..K {
        label_expr = format!("(a|{label_expr})");
    }
    let query = format!("MATCH (n) RETURN [x IN [n] WHERE x:{label_expr} | x.name] AS filtered");
    let stmt = parse(&query).unwrap();
    match &stmt.clauses[1] {
        Clause::Return(rc) => match &rc.items[0].expr {
            Expr::ListComprehension(lc) => {
                assert!(matches!(lc.where_expr.as_ref().unwrap(), Expr::HasLabel(_)));
                // The trailing `|` must still be the comprehension's map
                // separator, not swallowed as a label disjunction.
                assert!(matches!(lc.map_expr.as_ref().unwrap(), Expr::Property(_)));
            }
            other => panic!("expected ListComprehension, got {other:?}"),
        },
        other => panic!("expected return, got {other:?}"),
    }
}

#[test]
fn test_list_comprehension_wide_label_disjunction_fanout_is_subquadratic() {
    // Adversarial WIDE fan-out: `DEPTH` parenthesized nesting levels of
    // label disjunction (each counting against MAX_NESTING_DEPTH), every
    // level fanning out over `WIDTH` `a|` siblings before the nested
    // child. The per-pipe verdict memo alone leaves each probe
    // re-parsing its whole right-sibling subtree, so probe work grows
    // with tokens x depth (measured release before the term-extent
    // memo: 1.44 MB / 12.7 s, 2.88 MB / 35.1 s, 4.16 MB / 63.1 s -- a
    // parse-time DoS surviving the MAX_QUERY_BYTES cap). With the
    // extent memo every token position is probed once in total:
    // O(tokens). This ~1.44 MB shape parses in ~1.5 s even in a debug
    // build (release: ~240 ms); before the fix it needed minutes in
    // debug.
    const WIDTH: usize = 1800;
    const DEPTH: usize = 400;
    let mut label_expr = String::from("a");
    for _ in 0..DEPTH {
        let mut group = String::with_capacity(WIDTH * 2 + label_expr.len() + 8);
        for _ in 0..WIDTH {
            group.push_str("a|");
        }
        group.push_str(&label_expr);
        label_expr = format!("({group})");
    }
    let query = format!("MATCH (n) RETURN [x IN [n] WHERE x:{label_expr} | x.name] AS filtered");
    assert!(query.len() <= MAX_QUERY_BYTES);
    // Run on the shared big-stack harness: the recursive-descent parse of
    // 400 nested groups overflows the default 2 MiB test-worker stack in
    // debug builds. The generous guard only trips if the superlinear probe
    // cost returns.
    let parse_query = query.clone();
    let (elapsed, stmt) = run_with_big_stack(move || {
        let start = std::time::Instant::now();
        let stmt = parse(&parse_query).unwrap();
        (start.elapsed(), stmt)
    });
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "wide label-disjunction fan-out parse took {elapsed:?} (superlinear probing?)"
    );
    // AST spot-check: the disjunction semantics are unchanged -- each
    // level is an Or of WIDTH plain `a` labels plus the nested child,
    // and the trailing `|` remains the comprehension's map separator.
    match &stmt.clauses[1] {
        Clause::Return(rc) => match &rc.items[0].expr {
            Expr::ListComprehension(lc) => {
                let mut expr = match lc.where_expr.as_ref().unwrap() {
                    Expr::HasLabel(hl) => &hl.label_expr,
                    other => panic!("expected HasLabel, got {other:?}"),
                };
                for level in 0..DEPTH {
                    match expr {
                        LabelExpr::Or(parts) => {
                            assert_eq!(parts.len(), WIDTH + 1, "level {level}");
                            for part in &parts[..WIDTH] {
                                assert!(
                                    matches!(part, LabelExpr::Label(l) if l == "a"),
                                    "level {level}"
                                );
                            }
                            expr = &parts[WIDTH];
                        }
                        other => panic!("expected Or at level {level}, got {other:?}"),
                    }
                }
                assert!(matches!(expr, LabelExpr::Label(l) if l == "a"));
                assert!(matches!(lc.map_expr.as_ref().unwrap(), Expr::Property(_)));
            }
            other => panic!("expected ListComprehension, got {other:?}"),
        },
        other => panic!("expected return, got {other:?}"),
    }
}

#[test]
fn test_parse_many_return_items_is_not_quadratic() {
    // Complexity regression guard (LABS-81): token_byte_offset used to
    // rescan the whole source per projection item (twice per item), so
    // 40k RETURN items took ~1.85 s with clean 4x-per-doubling growth.
    // The precomputed line-start index makes each lookup O(1).
    let item_count = 40_000;
    let mut q = String::from("RETURN x0");
    for i in 1..item_count {
        q.push_str(&format!(", x{i}"));
    }
    let start = std::time::Instant::now();
    let stmt = parse(&q).unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "RETURN with {item_count} items took {elapsed:?} (quadratic source_text extraction?)"
    );
    let Clause::Return(r) = &stmt.clauses[0] else {
        panic!("expected return");
    };
    assert_eq!(r.items.len(), item_count);
    // Spot-check implicit-column-name source text at both ends.
    assert_eq!(r.items[0].source_text.as_deref(), Some("x0"));
    assert_eq!(
        r.items[item_count - 1].source_text.as_deref(),
        Some(format!("x{}", item_count - 1).as_str())
    );
}

#[test]
fn test_parse_long_property_chain_is_not_quadratic() {
    // Complexity regression guard (LABS-81): parse_postfix re-walked and
    // re-formatted the namespace chain per dot, plus an O(remaining)
    // lookahead per dot — 16k segments (32 KB) took ~49.5 s. The
    // namespace is now maintained incrementally and the chain lookahead
    // memoized.
    let segments = 16_000;
    let mut q = String::from("RETURN a");
    for i in 0..segments {
        q.push_str(&format!(".p{i}"));
    }
    let start = std::time::Instant::now();
    let stmt = parse(&q).unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "{segments}-segment property chain took {elapsed:?} (quadratic namespace parsing?)"
    );
    // Verify the chain shape iteratively.
    let Clause::Return(r) = &stmt.clauses[0] else {
        panic!("expected return");
    };
    let mut depth = 0usize;
    let mut cur = &r.items[0].expr;
    while let Expr::Property(p) = cur {
        depth += 1;
        cur = &p.expr;
    }
    assert_eq!(depth, segments);
    assert!(matches!(cur, Expr::Variable(v) if v == "a"));
    // The AST is a 16k-deep nested Box chain; the test thread's stack is
    // too small for the recursive Drop, so leak it deliberately.
    std::mem::forget(stmt);
}

#[test]
fn test_parse_long_namespace_call_chain_is_not_quadratic() {
    // Complexity regression guard: even with the incremental namespace
    // and memoized chain scan, `parse_postfix` used to materialize
    // `Expr::Variable(ns.clone())` per segment of a dot-chain the scan
    // had classified as ending in `(` — Θ(n²) clone traffic (release:
    // 640k segments ~5 s, clean 4x-per-doubling; a maximal 4 MiB query
    // ~50 s CPU, a parse-time DoS surviving the size cap). The
    // intermediate Variable was never observed (the terminating `(` arm
    // rebuilds the callee name from `ns` via `ns.take()`), so the chain
    // now accumulates the name string only.
    let segments = 100_000;
    let mut q = String::from("RETURN a");
    for _ in 0..segments {
        q.push_str(".a");
    }
    q.push_str(".f(1)");
    let start = std::time::Instant::now();
    let stmt = parse(&q).unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "{segments}-segment namespace call chain took {elapsed:?} (quadratic namespace parsing?)"
    );
    let Clause::Return(r) = &stmt.clauses[0] else {
        panic!("expected return");
    };
    match &r.items[0].expr {
        Expr::FunctionCall(fc) => {
            let expected_name = format!("a{}.f", ".a".repeat(segments));
            assert_eq!(fc.name.to_string(), expected_name);
            assert_eq!(fc.args.len(), 1);
        }
        other => panic!("expected FunctionCall, got {other:?}"),
    }
}

#[test]
fn test_node_pattern_probe_nested_is_not_exponential() {
    // Complexity regression guard (LABS-183's shape, for the
    // `node_pattern_precedes_relationship` probe): a node pattern's
    // inline WHERE can itself hold a pattern predicate, so nesting
    // `(a{i} WHERE <predicate>)-[:R]->()` k levels deep makes the probe
    // parse every deeper level and the committing parse re-parse it --
    // 2^k parse cost on a sub-KB query without the per-position verdict
    // memo (the same fix shape the `pattern_comprehension_probe_memo`
    // above pins). With the memo each `(` position is probed once and
    // the parse completes in milliseconds at any k.
    const K: usize = 26;
    let mut expr = String::from("(a)-[:R]->()");
    for level in 1..K {
        expr = format!("(a{level} WHERE {expr})-[:R]->()");
    }
    let query = format!("MATCH (n) WHERE {expr} RETURN n");
    assert!(query.len() <= MAX_QUERY_BYTES);
    let start = std::time::Instant::now();
    let stmt = parse(&query).unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "nested pattern-predicate parse took {elapsed:?} (exponential probing?)"
    );
    // AST spot-check: the top-level WHERE is the outermost pattern
    // predicate, whose node pattern is `a{K-1}`; each node's inline WHERE
    // holds the next level down, ending at the bare `(a)`.
    let Clause::Match(m) = &stmt.clauses[0] else {
        panic!("expected match");
    };
    let mut cur = m.where_expr.as_ref();
    for level in (1..K).rev() {
        let Some(Expr::PatternPredicate(pp)) = cur else {
            panic!("expected PatternPredicate at level {level}, got {cur:?}")
        };
        let Some(PatternElement::Node(node)) = pp.pattern.elements.first() else {
            panic!("expected a leading node pattern at level {level}")
        };
        assert_eq!(
            node.variable,
            Some(SmallName::from_name(&format!("a{level}")))
        );
        cur = node.where_expr.as_ref();
    }
    assert!(matches!(cur, Some(Expr::PatternPredicate(_))));
}

#[test]
fn test_nested_pattern_predicate_fanout_is_not_size_times_depth() {
    // Complexity regression guard (the CI security round's shape): each
    // nesting level of an inline-WHERE pattern predicate adds a 100-way OR
    // fan-out, so the query carries ~K*F predicates while every gate's
    // probe would descend its whole nested subtree -- O(size x depth)
    // parse work (measured before the node-pattern tree cache: a 2.74 MB
    // depth-450 query at ~51 s release, 30x the pre-probe mirror). The
    // probe's tree cache makes the descent single-pass: the probe builds
    // each position's tree once, every enclosing/committing parse moves
    // it up. K=300, F=100 puts the uncached shape far past the budget
    // below even in debug; the cached shape parses in well under a
    // second.
    const K: usize = 300;
    const F: usize = 100;
    run_with_big_stack(|| {
        let (stmt, elapsed) = build_and_parse_fanout(K, F);
        check_fanout(stmt, elapsed);
    });
}

fn build_and_parse_fanout(k: usize, f: usize) -> (relate_parser::Statement, std::time::Duration) {
    // The innermost chain link: `(aK)-[:R]->()`.
    let mut expr = format!("(a{k})-[:R]->()");
    for level in (0..k).rev() {
        // The chain link plus F-1 simple siblings, all OR operands of
        // this level's inline WHERE.
        let mut level_expr = format!("(a{level} WHERE {expr}");
        for sibling in 0..f {
            level_expr.push_str(&format!(" OR (b{level}_{sibling})-[:R]->()"));
        }
        level_expr.push_str(")-[:R]->()");
        expr = level_expr;
    }
    let query = format!("MATCH (r:L) WHERE {expr} RETURN r");
    assert!(query.len() <= MAX_QUERY_BYTES, "{}", query.len());
    let start = std::time::Instant::now();
    let stmt = parse(&query).unwrap();
    (stmt, start.elapsed())
}

fn check_fanout(stmt: relate_parser::Statement, elapsed: std::time::Duration) {
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "nested fan-out pattern-predicate parse took {elapsed:?} (O(size x depth) probing?)"
    );
    let Clause::Match(m) = &stmt.clauses[0] else {
        panic!("expected match");
    };
    assert!(matches!(
        m.where_expr.as_ref(),
        Some(Expr::PatternPredicate(_))
    ));
}

/// Stack-safety pin (ADR-0257): a flat, wide OR spine inside a node
/// pattern's inline WHERE parses within the default 2 MiB test-worker
/// stack — deliberately NOT under `run_with_big_stack`. The parse itself
/// is iterative, and the speculation cache parks only O(1) verdicts (a
/// parked node pattern is never cloned), so nothing on this path
/// recurses per spine term. A parked-tree deep clone would recurse once
/// per AST spine node (~1 KiB of stack per OR term) and abort at a few
/// thousand terms on this stack; the 989 KB / 100k-term form exceeded
/// even 96 MiB.
#[test]
fn wide_inline_where_spine_parses_within_the_default_stack() {
    const TERMS: usize = 8_000;
    let mut predicate = String::from("x0");
    for i in 1..TERMS {
        predicate.push_str(&format!(" OR x{i}"));
    }
    let query = format!("MATCH (n) WHERE (a WHERE {predicate})-[:R]->(c) RETURN n");
    assert!(query.len() <= MAX_QUERY_BYTES, "{}", query.len());
    let start = std::time::Instant::now();
    let stmt = parse(&query).unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "wide inline-WHERE spine took {elapsed:?}"
    );
    let Clause::Match(m) = &stmt.clauses[0] else {
        panic!("expected match");
    };
    assert!(matches!(
        m.where_expr.as_ref(),
        Some(Expr::PatternPredicate(_))
    ));
}
