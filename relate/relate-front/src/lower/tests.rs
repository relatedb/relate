//! The lowering tests: the plan shapes `lower_statement` produces for each
//! clause family, the fold's declines, and the error texts — the sibling of
//! the lowering code they exercise.

use super::*;
use relate_test_support::run_with_big_stack;

fn lower(query: &str) -> mdb::Query {
    lower_statement(&cl::parse(query).unwrap()).unwrap()
}

fn lower_err(query: &str) -> String {
    lower_statement(&cl::parse(query).unwrap())
        .unwrap_err()
        .to_string()
}

/// The ADR-0257 depth error text, derived from the single-sourced cap
/// (`relate_parser::MAX_NESTING_DEPTH` via `validate`).
fn depth_cap_message() -> String {
    format!(
        "maximum nesting depth of {} exceeded",
        crate::validate::MAX_NESTING_DEPTH
    )
}

/// The CIP-150 `CYPHER <version>` preamble is a parse-level no-op
/// (ADR-0584: CYPHER 25 is the one admitted spelling): the formerly
/// gated Cypher-25-only forms — string interpolation (CIP-261), the
/// `vector()` value function (CIP-214), the `SEARCH` clause (CIP-224),
/// the two-argument temporal constructor (CIP-228), and the plain
/// `RETURN 1 AS n` body — lower to exactly the same IR under an
/// explicit `CYPHER 25` and no prefix at all, for any whitespace after
/// the preamble (the historical query-text prefix sniff missed
/// `CYPHER 25\n…`).
#[test]
fn formerly_gated_forms_lower_identically_under_every_spelling() {
    for body in [
        "RETURN s'a{1}b' AS r",
        "RETURN vector([1.0], 2, FLOAT32) AS v",
        "MATCH (n:Doc) SEARCH n IN (VECTOR INDEX docs FOR [1.0] LIMIT 2) RETURN n",
        "RETURN date('2024-06-27', 'yyyy-MM-dd') AS x",
        "RETURN 1 AS n NEXT RETURN 2 AS m",
        "RETURN 1 AS n",
    ] {
        let unprefixed = lower(body);
        for prefixed in [
            format!("CYPHER 25 {body}"),
            format!("CYPHER 25\n{body}"),
            format!("CYPHER 25\t{body}"),
        ] {
            assert_eq!(
                lower(&prefixed),
                unprefixed,
                "the preamble changes the lowering of {body:?}"
            );
        }
    }
}

#[test]
fn preserves_dynamic_label_and_property_ir() {
    let query = lower(
        "MATCH (n:$any($labels)) WHERE n IS LABELED (A|!B) \
         SET n[$key] = 1, n:$(toString($label)) REMOVE n[$old], n:$($gone) RETURN n[$key]",
    );
    let clauses = &query.parts[0].clauses;
    let mdb::Clause::Match {
        patterns,
        where_clause,
        ..
    } = &clauses[0]
    else {
        panic!("expected MATCH")
    };
    assert!(matches!(
        patterns[0].nodes[0].label_expr,
        Some(mdb::LabelExpr::Dynamic {
            quantifier: mdb::DynamicLabelQuantifier::Any,
            ..
        })
    ));
    assert!(matches!(
        where_clause,
        Some(mdb::Expr::LabelPredicate {
            label_expr: mdb::LabelExpr::Or(_),
            strict: true,
            ..
        })
    ));
    let mdb::Clause::Set { items } = &clauses[1] else {
        panic!("expected SET")
    };
    assert!(matches!(
        items[0],
        mdb::SetItem::Property {
            target: mdb::Expr::DynamicProperty { .. },
            ..
        }
    ));
    assert!(matches!(
        items[1],
        mdb::SetItem::Labels { ref labels, .. }
            if matches!(labels.as_slice(), [mdb::LabelExpr::Dynamic { .. }])
    ));
    let mdb::Clause::Remove { items } = &clauses[2] else {
        panic!("expected REMOVE")
    };
    assert!(matches!(
        items[0],
        mdb::RemoveItem::Property {
            target: mdb::Expr::DynamicProperty { .. }
        }
    ));
}

#[test]
fn preserves_simple_case_label_predicate_marker() {
    let query = lower("MATCH (n) RETURN CASE n WHEN IS LABELED A THEN 1 ELSE 0 END AS result");
    let mdb::Clause::Return { items, .. } = &query.parts[0].clauses[1] else {
        panic!("expected RETURN")
    };
    let mdb::Expr::Case { when_then, .. } = &items[0].expr else {
        panic!("expected CASE")
    };
    assert!(matches!(when_then[0].0, mdb::Expr::LabelPredicate { .. }));
}

#[test]
fn preserves_gpm_modes_selectors_qpp_and_inline_predicates() {
    let query = lower(
        "MATCH REPEATABLE ELEMENTS p = SHORTEST 2 WALK GROUPS \
         (s WHERE s.active)-[lead:LEAD WHERE lead.ok]->\
         ((a)-[r:R]->(b) WHERE r.weight > 0){1,3}(t) RETURN p, a, r",
    );
    let mdb::Clause::Match {
        match_mode,
        patterns,
        ..
    } = &query.parts[0].clauses[0]
    else {
        panic!("expected MATCH")
    };
    assert_eq!(*match_mode, mdb::MatchMode::RepeatableElements);
    let path = &patterns[0];
    assert_eq!(path.mode, Some(mdb::PathMode::Walk));
    assert!(matches!(
        path.selector,
        Some(mdb::PathSelector::Shortest {
            count: Some(mdb::PathCount::Literal(2)),
            group: true,
        })
    ));
    assert!(matches!(
        &path.elements[0],
        mdb::PatternElement::Node(mdb::NodePattern {
            where_clause: Some(_),
            ..
        })
    ));
    assert!(matches!(
        &path.elements[1],
        mdb::PatternElement::Relationship(mdb::RelPattern {
            where_clause: Some(_),
            ..
        })
    ));
    let qpp = path
        .elements
        .iter()
        .find_map(|element| match element {
            mdb::PatternElement::Quantified(qpp) => Some(qpp),
            _ => None,
        })
        .expect("quantified path element");
    assert_eq!((qpp.min, qpp.max), (1, Some(3)));
    assert!(qpp.where_clause.is_some());
    assert!(path.nodes.is_empty(), "QPP must not be flattened away");
}

// ---- LABS-661: every var-length spelling lowers to the one QPP IR ----

/// The first MATCH pattern of a lowered statement.
fn pattern_of(query: &str) -> mdb::PathPattern {
    let mdb::Clause::Match { patterns, .. } = &lower(query).parts[0].clauses[0] else {
        panic!("{query}: expected a leading MATCH");
    };
    patterns[0].clone()
}

/// LABS-661: the compact var-length relationship lowers to the same
/// quantified-path IR the parenthesized group produces — implicit
/// anonymous boundary nodes, one-hop inner rel, bounds on the
/// quantifier — so both spellings take the one GPM engine path.
#[test]
fn var_length_lowering_matches_parenthesized_group_ir() {
    let compact = pattern_of("MATCH p = (x:Person)-[r:knows*1..2]-(y) RETURN p");
    let group = pattern_of("MATCH p = (x:Person)(()-[r:knows]-()){1,2}(y) RETURN p");
    assert_eq!(compact.elements, group.elements);
    assert_eq!(compact.mode, group.mode);
    assert_eq!(compact.variable, group.variable);
    assert!(compact.where_clause.is_none() && group.where_clause.is_none());
    assert!(
        compact.needs_gpm() && group.needs_gpm(),
        "both spellings must route to the GPM engine"
    );
    // The compact `nodes`/`rels` view is retained (the eval-pattern
    // engine walks it); the group form never had one. The ENGINE path
    // is the elements — identical above.
    assert_eq!(compact.rels[0].min_hops, 1);
    assert_eq!(compact.rels[0].max_hops, Some(2));
    assert!(compact.rels[0].var_length);
}

/// The postfix quantifier (`-{m,n}`, CIP-28 Form 5) and the path-mode
/// prefixes lower onto the same quantified element as the legacy
/// bracket form; the mode prefix rides along unchanged.
#[test]
fn postfix_quantifier_and_mode_prefix_share_the_quantified_ir() {
    let legacy = pattern_of("MATCH (x)-[:knows*1..2]->(y) RETURN y");
    let postfix = pattern_of("MATCH (x)-[:knows]->{1,2}(y) RETURN y");
    let qpp_of = |p: &mdb::PathPattern| match &p.elements[1] {
        mdb::PatternElement::Quantified(qpp) => qpp.clone(),
        other => panic!("expected a quantified element, got {other:?}"),
    };
    let (legacy_qpp, postfix_qpp) = (qpp_of(&legacy), qpp_of(&postfix));
    assert_eq!((legacy_qpp.min, legacy_qpp.max), (1, Some(2)));
    assert_eq!((postfix_qpp.min, postfix_qpp.max), (1, Some(2)));
    assert_eq!(legacy_qpp.subpath_variable, postfix_qpp.subpath_variable);
    assert_eq!(legacy_qpp.where_clause, postfix_qpp.where_clause);
    let rel_of = |qpp: &mdb::QuantifiedPathPattern| match &qpp.elements[1] {
        mdb::PatternElement::Relationship(rel) => rel.clone(),
        other => panic!("expected the inner rel, got {other:?}"),
    };
    let (legacy_rel, postfix_rel) = (rel_of(&legacy_qpp), rel_of(&postfix_qpp));
    assert_eq!(legacy_rel.types, postfix_rel.types);
    assert_eq!(legacy_rel.direction, postfix_rel.direction);
    assert!(!legacy_rel.var_length && !postfix_rel.var_length);
    assert_eq!((legacy_rel.min_hops, legacy_rel.max_hops), (1, Some(1)));
    // The postfix spelling keeps its `quantified` marker: an inline
    // rel WHERE keeps its per-traversed-edge semantics there.
    assert!(!legacy_rel.quantified && postfix_rel.quantified);

    let trailed = pattern_of("MATCH p = TRAIL (x)-[:knows*1..2]-(y) RETURN p");
    let trailed_group = pattern_of("MATCH p = TRAIL (x)(()-[:knows]-()){1,2}(y) RETURN p");
    assert_eq!(trailed.elements, trailed_group.elements);
    assert_eq!(trailed.mode, Some(mdb::PathMode::Trail));
    assert_eq!(trailed.mode, trailed_group.mode);
}

/// A var-length chain quantifies position-preserving: the interior node
/// stays between the two groups (first group's target, second group's
/// start), matched once — the legacy semantics.
#[test]
fn var_length_chain_keeps_interior_nodes_as_group_targets() {
    let chain = pattern_of("MATCH (a)-[:R*1..2]->(b:B)-[:S*0..1]->(c) RETURN c");
    let qpp_at = |p: &mdb::PathPattern, i: usize| match &p.elements[i] {
        mdb::PatternElement::Quantified(qpp) => (qpp.min, qpp.max),
        other => panic!("expected a quantified element at {i}, got {other:?}"),
    };
    let node_labels = |p: &mdb::PathPattern, i: usize| match &p.elements[i] {
        mdb::PatternElement::Node(node) => node.label_names().cloned().collect::<Vec<_>>(),
        other => panic!("expected a node element at {i}, got {other:?}"),
    };
    assert_eq!(chain.elements.len(), 5);
    assert_eq!(qpp_at(&chain, 1), (1, Some(2)));
    assert_eq!(
        node_labels(&chain, 2),
        vec![relate_names::SmallName::from_name("B")]
    );
    assert_eq!(qpp_at(&chain, 3), (0, Some(1)));
    // The exact-hop and unbounded spellings carry their bounds onto
    // the quantifier.
    assert_eq!(
        qpp_at(&pattern_of("MATCH (a)-[:R*3]->(b) RETURN b"), 1),
        (3, Some(3))
    );
    assert_eq!(
        qpp_at(&pattern_of("MATCH (a)-[:R*]->(b) RETURN b"), 1),
        (1, None)
    );
}

// ---- LABS-70/LABS-81: nested-statement validation & depth cap ----

/// A nested `CALL { }` body one level down from `stmt`, built by hand so
/// the depth tests exercise the front-end guard directly (the parser
/// applies its own accounting before these ever reach lowering).
fn wrap_in_call(stmt: cl::Statement) -> cl::Statement {
    cl::Statement {
        clauses: vec![cl::Clause::CallSubquery(cl::CallSubqueryClause {
            statement: stmt,
            scope: cl::CallScope::None,
            optional: false,
            in_transactions: None,
        })],
        ..cl::Statement::default()
    }
}

fn return_one_statement() -> cl::Statement {
    cl::Statement {
        clauses: vec![cl::Clause::Return(cl::ReturnClause {
            group_by: None,
            distinct: false,
            items: vec![cl::ReturnItem {
                expr: cl::Expr::Literal(cl::LiteralValue::Integer(1)),
                // Aliased: this statement is also wrapped in `CALL { … }`
                // bodies by `nested_call_statement`, and a CALL-subquery
                // RETURN item must be aliased (Neo4j 42N21).
                alias: Some(relate_names::SmallName::from_name("x")),
                all: false,
                source_text: None,
            }],
            order_by: vec![],
            skip: None,
            limit: None,
        })],
        ..cl::Statement::default()
    }
}

/// `RETURN 1` inside `depth` nested `CALL { }` bodies: the innermost body
/// sits at nesting depth `depth`.
fn nested_call_statement(depth: usize) -> cl::Statement {
    let mut stmt = return_one_statement();
    for _ in 0..depth {
        stmt = wrap_in_call(stmt);
    }
    stmt
}

/// `RETURN 1` with `arms` UNION links chained onto it: the last arm sits
/// at nesting depth `arms`.
fn union_chain_statement(arms: usize) -> cl::Statement {
    let mut stmt = return_one_statement();
    for _ in 0..arms {
        stmt = cl::Statement {
            clauses: return_one_statement().clauses,
            union: Some(Box::new(cl::UnionPart {
                all: false,
                statement: stmt,
            })),
            ..cl::Statement::default()
        };
    }
    stmt
}

#[test]
fn nested_call_subquery_limit_is_rejected_like_top_level() {
    // LABS-81: the top-level equivalent is already rejected; the nested
    // forms must fail with the same error class.
    let top = lower_err("RETURN 1 LIMIT -1");
    assert!(top.contains("NegativeIntegerArgument"), "{top}");
    for query in [
        "CALL { RETURN 1 LIMIT -1 } RETURN 1",
        "CALL { CALL { RETURN 1 LIMIT -1 } RETURN 1 } RETURN 1",
        // A UNION arm is a nested statement too.
        "RETURN 1 AS a UNION RETURN 2 AS a LIMIT -1",
    ] {
        let err = lower_err(query);
        assert!(
            err.contains("NegativeIntegerArgument"),
            "wrong error for {query}: {err}"
        );
    }
}

#[test]
fn conditional_and_foreach_bodies_are_validated_like_top_level() {
    // CIP-135 `WHEN ... THEN { ... }` branch bodies are full statements.
    let err = lower_err("WHEN true THEN { RETURN 1 AS x LIMIT -1 } ELSE { RETURN 2 AS x }");
    assert!(err.contains("NegativeIntegerArgument"), "{err}");
    // FOREACH bodies share the enclosing scope and take the write rules.
    let err = lower_err("FOREACH (x IN [1] | CREATE (a)-[:R]-(b))");
    assert!(err.contains("RequiresDirectedRelationship"), "{err}");
    let err = lower_err("FOREACH (x IN [1] | MERGE (n $props))");
    assert!(err.contains("InvalidParameterUse"), "{err}");
}

#[test]
fn param_property_shorthand_inside_subquery_expressions_is_rejected() {
    // LABS-81: `$props` shorthand is CREATE-only; a subquery expression
    // body is not a hiding place for it.
    let top = lower_err("MATCH (m $props) RETURN m");
    assert!(top.contains("InvalidParameterUse"), "{top}");
    for query in [
        "MATCH (n) WHERE EXISTS { MATCH (m $props) } RETURN n",
        "RETURN count { MATCH (m $props) } AS c",
        "RETURN collect { MATCH (m $props) RETURN m } AS c",
    ] {
        let err = lower_err(query);
        assert!(
            err.contains("InvalidParameterUse"),
            "wrong error for {query}: {err}"
        );
    }
}

#[test]
fn valid_nested_statements_still_lower() {
    for query in [
        "CALL { RETURN 1 AS x } RETURN x",
        "CALL { CALL { RETURN 1 AS x } RETURN x } RETURN x",
        "MATCH (n) WHERE EXISTS { MATCH (n)-->(m) } RETURN n",
        "RETURN count { MATCH (n) } AS c",
        "FOREACH (x IN [1] | CREATE (:L))",
        "WITH 1 AS a CALL { WITH a RETURN a AS b } RETURN a, b",
        "WHEN true THEN { RETURN 1 AS x } ELSE { RETURN 2 AS x }",
        "RETURN 1 AS x UNION RETURN 2 AS x",
    ] {
        lower(query);
    }
}

#[test]
fn statement_nesting_at_the_limit_lowers() {
    // Exactly the ADR-0257 cap: accepted. Run on a big-stack thread (the
    // same pattern as the parser's
    // `test_parse_trim_deeply_nested_is_not_exponential`): 500 levels of
    // lowering/validation recursion overflow the default 2 MiB
    // test-thread stack in debug builds.
    for stmt in [nested_call_statement(500), union_chain_statement(500)] {
        run_with_big_stack(move || {
            lower_statement(&stmt).unwrap();
        });
    }
}

#[test]
fn statement_nesting_past_the_limit_is_a_typed_error() {
    // Limit + 1: a typed error naming the limit, no abort. Big-stack
    // thread as above: the depth guard early-exits at the limit + 1, but
    // even those ~500 levels of walker recursion overflow the default
    // 2 MiB test-thread stack in debug builds.
    let mut next_chain = return_one_statement();
    for _ in 0..501 {
        next_chain = cl::Statement {
            next: Some(Box::new(next_chain)),
            ..cl::Statement::default()
        };
    }
    for stmt in [
        nested_call_statement(501),
        union_chain_statement(501),
        next_chain,
    ] {
        run_with_big_stack(move || {
            let err = lower_statement(&stmt).unwrap_err().to_string();
            assert!(err.contains(&depth_cap_message()), "wrong error: {err}");
        });
    }
}

// ---- Per-hop chains & pattern-embedded subqueries ----

/// `WITH {a:null} AS n RETURN n` + `.a` x `hops`: a postfix property
/// chain the parser builds iteratively (counting no nesting depth).
fn property_chain_query(hops: usize) -> String {
    let mut query = String::from("WITH {a:null} AS n RETURN n");
    for _ in 0..hops {
        query.push_str(".a");
    }
    query
}

#[test]
fn property_chain_past_the_limit_is_a_typed_error_end_to_end() {
    // Verified pre-fix: this 16000-hop chain overflowed the main-thread
    // stack (SIGABRT in ~0.1s on an 8 MiB release stack; boundary
    // ~2000 hops) because neither the parser's iterative postfix loop
    // nor the front-end walker counted the hops, so `lower_expr`
    // recursed one native frame per hop. The depth guard must now
    // reject it with a typed error before lowering recurses. Big-stack
    // thread as in the ADR-0257 tests above.
    run_with_big_stack(|| {
        let err = crate::parse_and_lower(&property_chain_query(16_000))
            .unwrap_err()
            .to_string();
        assert!(err.contains(&depth_cap_message()), "wrong error: {err}");
    });
}

#[test]
fn property_chain_just_under_the_limit_lowers() {
    // 450 hops stays under the cap and must lower cleanly. Execution
    // end-to-end on an 8 MiB release stack was probed via the
    // `relate` CLI (`WITH {a:null} AS n RETURN n` + `.a` x 450
    // evaluates to `null`, no abort); see this node's commit message.
    run_with_big_stack(|| {
        lower(&property_chain_query(450));
    });
}

#[test]
fn param_property_shorthand_in_pattern_embedded_subqueries_is_rejected() {
    // `$props` shorthand is CREATE-only; a pattern comprehension's,
    // pattern predicate's, or shortestPath()'s embedded pattern is not
    // a hiding place for it (both live-exec repros verified executing
    // through `Database::query` pre-fix).
    let top = lower_err("MATCH (m $props) RETURN m");
    assert!(top.contains("InvalidParameterUse"), "{top}");
    for query in [
        "RETURN [(n {p: count { MATCH (m $props) }})-->(m) | m] AS x",
        "MATCH (n) WHERE (n {p: count { MATCH (m $props) RETURN m }})-->() RETURN n",
        "RETURN shortestPath((n {p: count { MATCH (m $props) }})-->(m)) AS p",
    ] {
        let err = lower_err(query);
        assert!(
            err.contains("InvalidParameterUse"),
            "wrong error for {query}: {err}"
        );
    }
}

#[test]
fn limit_inside_pattern_embedded_subquery_is_a_validation_error() {
    // `lower_err` never executes anything: a `LIMIT -1` surfaced here
    // is a *validation* error, not the runtime executor backstop that
    // caught the pattern-predicate form pre-fix.
    let top = lower_err("RETURN 1 LIMIT -1");
    assert!(top.contains("NegativeIntegerArgument"), "{top}");
    for query in [
        "MATCH (n) WHERE (n {p: count { RETURN 1 LIMIT -1 }})-->() RETURN n",
        "RETURN [(n {p: count { RETURN 1 LIMIT -1 }})-->(m) | m] AS x",
        "RETURN shortestPath((n {p: count { RETURN 1 LIMIT -1 }})-->(m)) AS p",
    ] {
        let err = lower_err(query);
        assert!(
            err.contains("NegativeIntegerArgument"),
            "wrong error for {query}: {err}"
        );
    }
}

#[test]
fn valid_pattern_embedded_expressions_still_lower() {
    // Walking pattern-embedded expressions must not reject legitimate
    // ones.
    for query in [
        "MATCH (n) RETURN [(n)-->(m) | m] AS x",
        "RETURN [(n {p: 1})-->(m) | m] AS x",
        "MATCH (n) WHERE (n)-->() RETURN n",
        "MATCH (n) WHERE (n {p: 1})-->() RETURN n",
    ] {
        lower(query);
    }
}

#[test]
fn deeply_nested_call_chain_is_a_typed_error_end_to_end() {
    // Through the full parse + lower path: a typed error, never an abort
    // (LABS-70 verified `RETURN NOT NOT ... x1000 true` aborted the
    // production binary; statement nesting takes the same recursion
    // paths).
    run_with_big_stack(|| {
        let mut query = String::new();
        for _ in 0..600 {
            query.push_str("CALL { ");
        }
        query.push_str("RETURN 1 AS x");
        for _ in 0..600 {
            query.push_str(" }");
        }
        let err = crate::parse_and_lower(&query).unwrap_err().to_string();
        assert!(err.contains(&depth_cap_message()), "wrong error: {err}");
    });
}

// ---- Cross-statement composed expression depth ----

/// `RETURN collect { <inner> }[0..]×hops AS x` wrapped `levels` deep
/// around `RETURN [1] AS x`: expression-embedded subqueries whose
/// per-statement postfix chains compose on one native stack, since
/// `lower_expr` lowers each `collect { }` body from inside its own
/// recursion.
fn collect_chain_composition_query(levels: usize, hops: usize) -> String {
    let mut query = String::from("RETURN [1] AS x");
    for _ in 0..levels {
        let mut wrapped = String::from("RETURN collect { ");
        wrapped.push_str(&query);
        wrapped.push_str(" }");
        for _ in 0..hops {
            wrapped.push_str("[0..]");
        }
        wrapped.push_str(" AS x");
        query = wrapped;
    }
    query
}

#[test]
fn collect_chain_composition_past_the_limit_is_a_typed_error_end_to_end() {
    // 3 nested `collect { }` levels × 450 slice hops each (~6.8 KB).
    // Each per-statement chain is under the cap on its own, but the
    // bodies lower from inside `lower_expr`'s recursion, so the composed
    // ~1350 frames nest on one native stack — pre-fix this passed the
    // depth guard (the count reset at each subquery boundary) and
    // SIGABRT'd inside `parse_and_lower` on an 8 MiB release stack. With
    // cumulative accounting it is a typed depth error, never an abort.
    // Big-stack thread as in the ADR-0257 tests above.
    run_with_big_stack(|| {
        let err = crate::parse_and_lower(&collect_chain_composition_query(3, 450))
            .unwrap_err()
            .to_string();
        assert!(err.contains(&depth_cap_message()), "wrong error: {err}");
    });
}

#[test]
fn deep_collect_chain_composition_errors_without_aborting() {
    // 50 levels × 450 hops: the guard early-exits at the first over-cap
    // composition (mid level 2), so even a deeply composed query returns
    // the typed error instead of aborting. Big-stack thread as above.
    run_with_big_stack(|| {
        let err = crate::parse_and_lower(&collect_chain_composition_query(50, 450))
            .unwrap_err()
            .to_string();
        assert!(err.contains(&depth_cap_message()), "wrong error: {err}");
    });
}

#[test]
fn collect_chain_composition_just_under_the_limit_lowers() {
    // 3 levels × 150 hops composes to ~304 levels of expression depth —
    // under the 500 cap — and must lower cleanly. Big-stack thread as
    // above (a few hundred walker/lowering recursion levels overflow the
    // default 2 MiB test-thread stack in debug builds).
    run_with_big_stack(|| {
        lower(&collect_chain_composition_query(3, 150));
    });
}
/// The q8 WHERE shape: `date(p.birthday) > date("1990-01-01")` lowers
/// with the literal constructor folded to its value — the plan carries
/// no per-row `date()` parse of the constant, only the property-side
/// construction the typed comparison needs.
#[test]
fn temporal_literal_calls_fold_at_lowering() {
    let query = lower(
        "MATCH (p:Person) \
         WHERE date(p.birthday) > date(\"1990-01-01\") RETURN p",
    );
    let mdb::Clause::Match {
        where_clause: Some(expr),
        ..
    } = &query.parts[0].clauses[0]
    else {
        panic!("expected a MATCH with a WHERE")
    };
    let mdb::Expr::Binary { op, left, right } = expr else {
        panic!("expected a comparison")
    };
    assert_eq!(*op, mdb::BinaryOp::Gt);
    // The literal side is the folded value, exactly what the shared
    // construction policy builds for the string.
    let folded = match right.as_ref() {
        mdb::Expr::RuntimeValue(value) => value.clone(),
        other => panic!("literal side must fold, found {other:?}"),
    };
    assert_eq!(
        folded,
        relate_core::temporal::construct_named(
            "date",
            &[relate_core::Value::String("1990-01-01".into())]
        )
        .unwrap()
    );
    // The property side keeps its per-row construction: one `date()`
    // call over `p.birthday`, never a literal argument in the plan.
    assert!(matches!(
        left.as_ref(),
        mdb::Expr::Function { name, distinct: false, args }
            if name == "date"
                && args.len() == 1
                && matches!(&args[0], mdb::Expr::Property { .. })
    ));
}

/// The first projection item's lowered expression — the one scaffold every
/// RETURN-shape assertion below needs.
fn first_return_expr(query: &str) -> mdb::Expr {
    let lowered = lower(query);
    let mdb::Clause::Return { items, .. } = &lowered.parts[0].clauses[0] else {
        panic!("expected RETURN in {query}");
    };
    items[0].expr.clone()
}

/// The fold's declines, each keeping the call's per-evaluation semantics: a
/// parameter (unknown at lowering), the zero-argument clock form, and the
/// clock-reading map form (folding it would bake the lowering-time instant
/// into the plan).
///
/// A literal that cannot construct no longer reaches the fold: the static
/// call validator rejects it at compile time (Neo4j reports a literal
/// temporal parse failure as a SyntaxError;
/// `static_types::tests::temporal_literal_static_rejections` covers it).
#[test]
fn temporal_fold_declines_parameters_and_clock_forms() {
    for src in [
        "RETURN date($d) AS d",
        "RETURN date() AS d",
        "RETURN date({timezone: 'UTC'}) AS d",
    ] {
        let expr = first_return_expr(src);
        assert!(
            matches!(&expr, mdb::Expr::Function { name, .. } if name == "date"),
            "{src} must stay a call, got {expr:?}"
        );
    }

    // A component-field map is deterministic and keeps folding.
    let folded = first_return_expr("RETURN date({year: 1984}) AS d");
    assert!(
        matches!(folded, mdb::Expr::RuntimeValue(_)),
        "date({{year: 1984}}) must fold, got {folded:?}"
    );
}
