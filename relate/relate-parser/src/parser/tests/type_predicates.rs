use super::*;

#[test]
fn test_parse_is_type_predicate() {
    let it = is_type_of("RETURN elementId(n) IS :: STRING AS isString");
    assert_eq!(
        it.type_expr,
        TypeExpr::Predefined {
            kind: PredefinedKind::String,
            not_null: false,
        }
    );
    assert!(!it.negated);
}

#[test]
fn test_parse_is_not_type_predicate_with_not_null() {
    let it = is_type_of("RETURN 1 IS NOT :: STRING NOT NULL AS r");
    assert_eq!(
        it.type_expr,
        TypeExpr::Predefined {
            kind: PredefinedKind::String,
            not_null: true,
        }
    );
    assert!(it.negated);
}

#[test]
fn test_parse_is_typed_and_bare_shorthand() {
    let expected = TypeExpr::Predefined {
        kind: PredefinedKind::Integer,
        not_null: false,
    };
    assert_eq!(
        is_type_of("RETURN 1 IS TYPED INTEGER AS a").type_expr,
        expected
    );
    assert_eq!(is_type_of("RETURN 1 :: INTEGER AS b").type_expr, expected);
}

#[test]
fn test_parse_type_predicate_no_chaining_without_parens() {
    assert!(parse("RETURN [1] IS :: LIST<INT> IS :: BOOLEAN AS r").is_err());
    assert!(parse("RETURN \"ab\" STARTS WITH \"a\" IS :: BOOLEAN AS r").is_err());
    assert!(parse("RETURN (\"ab\" STARTS WITH \"a\") IS :: BOOLEAN AS r").is_ok());
    assert!(parse("RETURN ([1] IS :: LIST<INT>) IS :: BOOLEAN AS r").is_ok());
}

#[test]
fn test_parse_vector_type_cypher_style() {
    assert_eq!(
        is_type_of("RETURN 1 IS :: VECTOR<INT32>(42) AS r").type_expr,
        TypeExpr::Vector {
            coord: Some(VectorCoordKind::Int32),
            dimension: Some(42),
            not_null: false,
        }
    );
}

#[test]
fn test_parse_vector_type_gql_style_dimension_first() {
    assert_eq!(
        is_type_of("RETURN 1 IS :: VECTOR(4, FLOAT64) AS r").type_expr,
        TypeExpr::Vector {
            coord: Some(VectorCoordKind::Float64),
            dimension: Some(4),
            not_null: false,
        }
    );
}

#[test]
fn test_parse_vector_type_partial_forms() {
    // Both halves are grammatically optional (rejected only later, at
    // constraint-creation time -- CIP-216 Negative 1/2).
    assert_eq!(
        is_type_of("RETURN 1 IS :: VECTOR(3) AS r").type_expr,
        TypeExpr::Vector {
            coord: None,
            dimension: Some(3),
            not_null: false,
        }
    );
    assert_eq!(
        is_type_of("RETURN 1 IS :: VECTOR<INTEGER32> AS r").type_expr,
        TypeExpr::Vector {
            coord: Some(VectorCoordKind::Int32),
            dimension: None,
            not_null: false,
        }
    );
}

#[test]
fn test_parse_vector_dimension_out_of_range_is_rejected() {
    assert!(parse("RETURN 1 IS :: VECTOR<INT32>(0) AS r").is_err());
    assert!(parse("RETURN 1 IS :: VECTOR<INT32>(4097) AS r").is_err());
    assert!(parse("RETURN 1 IS :: VECTOR<INT32>(-1) AS r").is_err());
    assert!(parse("RETURN 1 IS :: VECTOR<INT32>(1) AS r").is_ok());
    assert!(parse("RETURN 1 IS :: VECTOR<INT32>(4096) AS r").is_ok());
}

#[test]
fn test_parse_vector_invalid_coordinate_type_is_rejected() {
    assert!(parse("RETURN 1 IS :: VECTOR<STRING>(3) AS r").is_err());
}

#[test]
fn test_parse_vector_only_coordinate_type_rejected_as_plain_type() {
    assert!(parse("RETURN 1 IS :: INTEGER32 AS r").is_err());
    assert!(parse("RETURN 1 IS :: FLOAT32 AS r").is_err());
    // INT64/FLOAT64 remain valid plain-type aliases (CIP-216 S-6).
    assert!(parse("RETURN 1 IS :: INT64 AS r").is_ok());
    assert!(parse("RETURN 1 IS :: FLOAT64 AS r").is_ok());
}

// Every alias in the CIP-200 coordinate vocabulary parses inside
// `VECTOR<...>`: the coordinate grammar reads `VectorCoordKind::from_alias`,
// the one table the `vector()` constructor and the standalone-type rejection
// consult too. `SIGNED INTEGER` is the two-token spelling.
#[test]
fn test_parse_vector_coord_type_accepts_every_alias() {
    for (alias, coord) in [
        ("INT8", VectorCoordKind::Int8),
        ("INTEGER8", VectorCoordKind::Int8),
        ("INT16", VectorCoordKind::Int16),
        ("INTEGER16", VectorCoordKind::Int16),
        ("INT32", VectorCoordKind::Int32),
        ("INTEGER32", VectorCoordKind::Int32),
        ("INT", VectorCoordKind::Int64),
        ("INTEGER", VectorCoordKind::Int64),
        ("INT64", VectorCoordKind::Int64),
        ("INTEGER64", VectorCoordKind::Int64),
        ("SIGNED INTEGER", VectorCoordKind::Int64),
        ("FLOAT16", VectorCoordKind::Float16),
        ("BFLOAT16", VectorCoordKind::BFloat16),
        ("FLOAT32", VectorCoordKind::Float32),
        ("FLOAT", VectorCoordKind::Float64),
        ("FLOAT64", VectorCoordKind::Float64),
    ] {
        assert_eq!(
            is_type_of(&format!("RETURN 1 IS :: VECTOR<{alias}>(3) AS r")).type_expr,
            TypeExpr::Vector {
                coord: Some(coord),
                dimension: Some(3),
                not_null: false,
            },
            "{alias}"
        );
    }
    // A half-written two-word alias stays a syntax error.
    assert!(parse("RETURN 1 IS :: VECTOR<SIGNED>(3) AS r").is_err());
}

#[test]
fn test_parse_bare_double_colon_vector_type_in_constraint() {
    // CIP-216 01#2/#4: the bare `x :: <type>` form skips `IS` entirely.
    let stmt =
        parse("CREATE CONSTRAINT c FOR (n:Label) REQUIRE n.prop :: VECTOR(4, FLOAT64)").unwrap();
    match &stmt.clauses[0] {
        Clause::CreateConstraint(cc) => {
            assert_eq!(cc.constraint_type, "propertyType");
            assert_eq!(
                cc.property_type,
                Some(TypeExpr::Vector {
                    coord: Some(VectorCoordKind::Float64),
                    dimension: Some(4),
                    not_null: false,
                })
            );
        }
        other => panic!("expected CreateConstraint, got {other:?}"),
    }
}

#[test]
fn test_parse_list_and_union_types() {
    match is_type_of("RETURN 1 IS :: INTEGER | FLOAT AS r").type_expr {
        TypeExpr::Union(components) => assert_eq!(components.len(), 2),
        other => panic!("expected Union, got {other:?}"),
    }

    match is_type_of("RETURN [1] IS :: LIST<INTEGER> AS r").type_expr {
        TypeExpr::List { element, not_null } => {
            assert!(!not_null);
            assert_eq!(
                *element,
                TypeExpr::Predefined {
                    kind: PredefinedKind::Integer,
                    not_null: false,
                }
            );
        }
        other => panic!("expected List, got {other:?}"),
    }

    // Mixed nullability among union components is a syntax error.
    assert!(parse("RETURN 1 IS :: INTEGER NOT NULL | FLOAT AS r").is_err());
    // NOT NULL directly after a closed union is a syntax error.
    assert!(parse("RETURN 1 IS :: ANY<INTEGER | FLOAT> NOT NULL AS r").is_err());

    // CIP-246: `UUID` is a standalone leaf type, not an `ANY`-prefixed
    // production. `ANY UUID` is a syntax error, exactly like every other
    // scalar type name after `ANY` (`ANY STRING`, ...).
    assert!(parse("RETURN 1 IS :: UUID AS r").is_ok());
    assert!(parse("RETURN 1 IS :: ANY UUID AS r").is_err());
    assert!(parse("RETURN 1 IS :: ANY STRING AS r").is_err());
}

/// CIP-102: `!` is an alternative spelling for a trailing `NOT NULL` in
/// a type expression -- it must produce the exact same `TypeExpr` as
/// the word form, everywhere `NOT NULL` is accepted (scalars, list
/// inner/outer, union members), and it must tolerate whitespace before
/// it (Neo4j-verified deviation from the CIP text).
#[test]
fn test_parse_not_null_bang_shorthand() {
    // Scalar: `!` and `NOT NULL` parse to an identical TypeExpr.
    let bang = is_type_of("RETURN 1 IS :: INTEGER! AS r").type_expr;
    let word = is_type_of("RETURN 1 IS :: INTEGER NOT NULL AS r").type_expr;
    assert_eq!(bang, word);

    // Whitespace (including a newline) before `!` is accepted.
    assert_eq!(is_type_of("RETURN 1 IS :: INTEGER ! AS r").type_expr, word);
    assert_eq!(is_type_of("RETURN 1 IS :: INTEGER\n! AS r").type_expr, word);

    // List outer/inner `!` matches the NOT NULL spelling.
    let list_bang = is_type_of("RETURN [1] IS :: LIST<INTEGER!>! AS r").type_expr;
    let list_word = is_type_of("RETURN [1] IS :: LIST<INTEGER NOT NULL> NOT NULL AS r").type_expr;
    assert_eq!(list_bang, list_word);

    // Union members: `!` on each component matches NOT NULL.
    let union_bang = is_type_of("RETURN 1 IS :: INTEGER! | FLOAT! AS r").type_expr;
    let union_word = is_type_of("RETURN 1 IS :: INTEGER NOT NULL | FLOAT NOT NULL AS r").type_expr;
    assert_eq!(union_bang, union_word);

    // Mixed nullability is still a compile-time error, whether spelled
    // with `!` or NOT NULL.
    assert!(parse("RETURN 1 IS :: INTEGER! | FLOAT AS r").is_err());
    // `!` directly after a closed union is likewise a syntax error.
    assert!(parse("RETURN 1 IS :: ANY<INTEGER! | FLOAT!>! AS r").is_err());
}

#[test]
fn test_parse_vector_type_float16_coordinate_kinds() {
    // VectorValueAcceptance's type-predicate rows name the narrow float
    // coordinate kinds inside `VECTOR<...>`.
    for (query, coord) in [
        (
            "RETURN 1 IS :: VECTOR<FLOAT16>(3) AS r",
            crate::ast::VectorCoordKind::Float16,
        ),
        (
            "RETURN 1 IS :: VECTOR<BFLOAT16>(3) AS r",
            crate::ast::VectorCoordKind::BFloat16,
        ),
    ] {
        assert_eq!(
            is_type_of(query).type_expr,
            TypeExpr::Vector {
                coord: Some(coord),
                dimension: Some(3),
                not_null: false,
            },
            "{query:?}"
        );
    }
}

/// The list comprehension of a single-`RETURN` statement's first item —
/// one extraction shared by the comprehension-WHERE type-predicate tests
/// below, instead of re-matching the clause/item/expression chain per
/// test (the comprehension sibling of `is_type_of`).
fn comprehension_of(query: &str) -> ListComprehensionExpr {
    let stmt = parse_ok(query);
    match return_expr(&stmt) {
        Expr::ListComprehension(lc) => (**lc).clone(),
        other => panic!("{query}: expected a list comprehension, got {other:?}"),
    }
}

/// The pattern comprehension of a single-`RETURN` statement's first item —
/// the pattern sibling of [`comprehension_of`].
fn pattern_comprehension_of(query: &str) -> PatternComprehensionExpr {
    match return_expr(&parse_ok(query)) {
        Expr::PatternComprehension(pc) => (**pc).clone(),
        other => panic!("{query}: expected a pattern comprehension, got {other:?}"),
    }
}

/// The map comprehension of a single-`RETURN` statement's first item —
/// the map sibling of [`comprehension_of`].
fn map_comprehension_of(query: &str) -> MapComprehensionExpr {
    match return_expr(&parse_ok(query)) {
        Expr::MapComprehension(mc) => (**mc).clone(),
        other => panic!("{query}: expected a map comprehension, got {other:?}"),
    }
}

/// The type predicate of a comprehension's WHERE — the extraction the
/// comprehension type-predicate tests assert through, naming the query
/// when the WHERE is something else.
fn where_is_type<'a>(query: &str, lc: &'a ListComprehensionExpr) -> &'a IsTypeExpr {
    match &lc.where_expr {
        Some(Expr::IsType(it)) => it,
        other => panic!("{query}: the WHERE is the type predicate, got {other:?}"),
    }
}

/// Asserts a comprehension's map expression is its own loop variable —
/// the one expectation spelling every pipe-yielding test asserts with,
/// naming the query when it fails.
fn assert_map_is_loop_variable(query: &str, lc: &ListComprehensionExpr) {
    assert_eq!(
        lc.map_expr,
        Some(Expr::Variable(lc.variable.clone())),
        "{query}: the map expression is the loop variable"
    );
}

/// LABS-1606: inside a comprehension's WHERE, a type predicate's `|` may
/// be the comprehension's map separator — the union loop leaves it there
/// when no type term follows. Both repro shapes: the dynamic-subscript
/// one from the report and the plain-variable one (the subscript is
/// irrelevant; a type predicate plus `|` is the failing pair).
#[test]
fn comprehension_where_type_predicate_yields_the_pipe_to_the_map_separator() {
    for query in [
        // The report's exact statement, EXPLAIN prefix included.
        "EXPLAIN MATCH (n) RETURN [k IN keys(n) WHERE n[k] IS :: STRING | k] AS x",
        "MATCH (n) RETURN [k IN keys(n) WHERE n[k] IS :: STRING | k] AS x",
        "RETURN [k IN ['a','b'] WHERE k IS :: STRING | k] AS x",
    ] {
        let lc = comprehension_of(query);
        let it = where_is_type(query, &lc);
        assert_eq!(
            it.type_expr,
            TypeExpr::Predefined {
                kind: PredefinedKind::String,
                not_null: false,
            },
            "{query}"
        );
        assert!(!it.negated, "{query}");
        assert_map_is_loop_variable(query, &lc);
    }
}

/// Every spelling that routes through the `<value type>` parse —
/// `IS NOT ::`, `IS TYPED` and the bare `::` shorthand — resolves the
/// ambiguity the same way, because they share `parse_value_type`.
#[test]
fn every_type_predicate_spelling_yields_the_pipe_in_a_comprehension_where() {
    for (query, negated) in [
        (
            "RETURN [k IN ['a'] WHERE k IS NOT :: STRING | k] AS x",
            true,
        ),
        (
            "RETURN [k IN ['a'] WHERE k IS TYPED STRING | k] AS x",
            false,
        ),
        ("RETURN [k IN ['a'] WHERE k :: STRING | k] AS x", false),
    ] {
        let lc = comprehension_of(query);
        let it = where_is_type(query, &lc);
        assert_eq!(it.negated, negated, "{query}");
        assert_map_is_loop_variable(query, &lc);
    }
}

/// A real union in a comprehension WHERE consumes its components
/// greedily: `STRING | INTEGER | k` is the two-component union followed by
/// the separator, not a three-component union erroring on `k`. The greedy
/// split is the deliberate choice — a term that parses as a type and sits
/// in front of a token the type grammar can continue with is kept as a
/// union component, exactly as the label disjunction is kept
/// (`parse_value_type_at_current_depth`'s commit-on-success rule mirrors
/// `parse_label_or`).
#[test]
fn a_union_in_a_comprehension_where_is_greedy_and_the_last_pipe_separates() {
    let query = "RETURN [k IN ['a'] WHERE k IS :: STRING | INTEGER | k] AS x";
    let lc = comprehension_of(query);
    let it = where_is_type(query, &lc);
    assert_eq!(
        it.type_expr,
        TypeExpr::Union(vec![
            TypeExpr::Predefined {
                kind: PredefinedKind::String,
                not_null: false,
            },
            TypeExpr::Predefined {
                kind: PredefinedKind::Integer,
                not_null: false,
            },
        ])
    );
    assert_map_is_loop_variable(query, &lc);
}

/// A comprehension WHERE's type predicate without a pipe keeps today's
/// parse: the WHERE is the predicate and no map expression follows.
#[test]
fn a_comprehension_where_type_predicate_without_a_pipe_has_no_map() {
    let lc = comprehension_of("RETURN [k IN ['a','b'] WHERE k IS :: STRING] AS x");
    assert!(matches!(&lc.where_expr, Some(Expr::IsType(_))));
    assert!(lc.map_expr.is_none());
}

/// Outside a comprehension the strict union grammar is unchanged: `|`
/// after a type still demands another type term.
#[test]
fn a_type_union_still_demands_a_term_outside_a_comprehension() {
    let err = parse("RETURN 'a' IS :: STRING | x AS r").unwrap_err();
    assert!(
        err.to_string().contains("expected a type name"),
        "the strict error survived, got {err}"
    );
}

/// Inside type brackets a `|` is always a union separator — the
/// comprehension's separator can never appear before the brackets close —
/// so the strict grammar survives there: `LIST<STRING | k>` keeps the
/// "expected a type name" error, while a well-formed bracketed union
/// still parses with the `|` after the closed bracket yielding to the
/// separator.
#[test]
fn type_brackets_keep_the_strict_union_grammar_in_a_comprehension_where() {
    let query = "RETURN [k IN ['a'] WHERE k IS :: LIST<STRING | INTEGER> | k] AS x";
    let lc = comprehension_of(query);
    let it = where_is_type(query, &lc);
    assert_eq!(
        it.type_expr,
        TypeExpr::List {
            element: Box::new(TypeExpr::Union(vec![
                TypeExpr::Predefined {
                    kind: PredefinedKind::String,
                    not_null: false,
                },
                TypeExpr::Predefined {
                    kind: PredefinedKind::Integer,
                    not_null: false,
                },
            ])),
            not_null: false,
        }
    );
    assert_map_is_loop_variable(query, &lc);

    let err = parse("RETURN [k IN ['a'] WHERE k IS :: LIST<STRING | k> | k] AS x").unwrap_err();
    assert!(
        err.to_string().contains("expected a type name"),
        "the bracket-internal strict error survived, got {err}"
    );
}

/// The dead-end tails: a would-be component that parses as a type term
/// but sits in front of a token no type grammar continues with was the
/// map expression's base variable (spelled like a type keyword), not a
/// union component — `string.foo` (`.`), `string{.p}` (`{`), and, where
/// the comprehension must still produce its map (a pattern
/// comprehension), a bare `string]`.
#[test]
fn dead_end_tails_yield_the_pipe_to_the_map_expression() {
    let lc = comprehension_of("RETURN [k IN ['a'] WHERE k IS :: STRING | string.foo] AS x");
    assert!(matches!(&lc.where_expr, Some(Expr::IsType(_))));
    assert!(matches!(&lc.map_expr, Some(Expr::Property(_))));

    let lc = comprehension_of("RETURN [k IN ['a'] WHERE k IS :: STRING | string{.p}] AS x");
    assert!(matches!(&lc.where_expr, Some(Expr::IsType(_))));
    assert!(matches!(&lc.map_expr, Some(Expr::MapProjection(_))));

    let pc =
        pattern_comprehension_of("RETURN [(a)-[:R]->(b) WHERE a.p IS :: STRING | string] AS x");
    assert!(matches!(&pc.where_expr, Some(Expr::IsType(_))));
    assert!(matches!(&pc.map_expr, Expr::Variable(_)));
}

/// A CIP-272 map comprehension's WHERE shares the same flag machinery
/// (`parse_comprehension_where_expr`), so its type predicate yields the
/// `|` to the `k : v` projection separator the same way the list and
/// pattern forms do.
#[test]
fn a_map_comprehension_where_type_predicate_yields_the_pipe_to_the_projection() {
    let mc = map_comprehension_of("RETURN {k : v IN {a: 'x'} WHERE v IS :: STRING | k : v} AS x");
    assert!(matches!(&mc.where_expr, Some(Expr::IsType(_))));
    assert_eq!(mc.key_expr, Expr::Variable(SmallName::from_name("k")));
    assert_eq!(mc.value_expr, Expr::Variable(SmallName::from_name("v")));
}

/// The two pipe rules compose inside one comprehension WHERE: the label
/// disjunction of an embedded pattern keeps its `|` (the label side's
/// own `pattern_element_depth` scope), while the trailing type
/// predicate's `|` yields to the separator.
#[test]
fn a_pattern_disjunction_and_a_where_type_predicate_split_their_pipes() {
    let query = "MATCH (a) RETURN [x IN [a] WHERE (x)-[:S|T]->() AND x.p IS :: STRING | x] AS out";
    let lc = comprehension_of(query);
    let Some(Expr::Binary(bin)) = &lc.where_expr else {
        panic!("the WHERE is the conjunction, got {:?}", lc.where_expr);
    };
    assert_eq!(bin.op, AND);
    assert!(matches!(&bin.right, Expr::IsType(_)));
    assert_map_is_loop_variable(query, &lc);
}

/// LABS-1606 review round 2: the type grammar's tail classification is a
/// continuation whitelist — a `|` whose would-be union component parses
/// as a type term but sits in front of a token the type grammar cannot
/// continue with is the comprehension's map separator, because those
/// tokens open legal map expressions whose base is the type-named
/// variable: `date(n[k])` and `point(n[k])` (a call), `string[0]` (a
/// subscript), `string IS NOT NULL` (a null check). All four failed when
/// the tail classification was the label side's narrow dead-end set (the
/// `|` committed as a union and the map was never reached).
#[test]
fn a_type_named_map_base_takes_the_pipe_from_the_union() {
    for (query, is_the_map) in [
        (
            "RETURN [k IN keys(n) WHERE n[k] IS :: STRING | date(n[k])] AS x",
            "date",
        ),
        (
            "RETURN [k IN keys(n) WHERE n[k] IS :: STRING | point(n[k])] AS x",
            "point",
        ),
    ] {
        let lc = comprehension_of(query);
        let it = where_is_type(query, &lc);
        assert_eq!(
            it.type_expr,
            TypeExpr::Predefined {
                kind: PredefinedKind::String,
                not_null: false,
            },
            "{query}"
        );
        assert!(
            matches!(&lc.map_expr, Some(Expr::FunctionCall(f)) if f.name == SmallName::from_name(is_the_map)),
            "{query}: the map expression is the {is_the_map} call, got {:?}",
            lc.map_expr
        );
    }

    // A subscript map base: `string[0]`.
    let query = "RETURN [k IN ['a'] WHERE k IS :: STRING | string[0]] AS x";
    let lc = comprehension_of(query);
    let it = where_is_type(query, &lc);
    assert_eq!(
        it.type_expr,
        TypeExpr::Predefined {
            kind: PredefinedKind::String,
            not_null: false,
        }
    );
    assert!(
        matches!(&lc.map_expr, Some(Expr::Subscript(_))),
        "{query}: the map expression is the subscript, got {:?}",
        lc.map_expr
    );

    // A null-check map base: `string IS NOT NULL`.
    let query = "RETURN [k IN ['a'] WHERE k IS :: STRING | string IS NOT NULL] AS x";
    let lc = comprehension_of(query);
    let it = where_is_type(query, &lc);
    assert_eq!(
        it.type_expr,
        TypeExpr::Predefined {
            kind: PredefinedKind::String,
            not_null: false,
        }
    );
    assert!(
        matches!(&lc.map_expr, Some(Expr::IsNull(z)) if z.negated),
        "{query}: the map expression is the negated null check, got {:?}",
        lc.map_expr
    );
}

/// The whitelist's bracket-internal arms: `,` and `)` after a type term
/// can only appear inside a bracket opened before the pipe (call
/// arguments, parenthesized groups) — exactly where the union reading is
/// legitimate and the comprehension's separator cannot appear — so they
/// continue the union. At the WHERE's top level neither token can
/// legally follow a type term, so admitting them costs nothing: the
/// top-level `|` still separates.
#[test]
fn a_bracket_internal_type_union_commits_and_the_top_level_pipe_separates() {
    // A union inside call arguments: the `,` after `INTEGER` continues
    // the union; the `|` after the call is the comprehension's separator.
    let query = "RETURN [x IN [1] WHERE coalesce(x IS :: STRING | INTEGER, true) | x] AS y";
    let lc = comprehension_of(query);
    let Some(Expr::FunctionCall(call)) = &lc.where_expr else {
        panic!("{query}: the WHERE is the call, got {:?}", lc.where_expr)
    };
    assert_eq!(call.name, SmallName::from_name("coalesce"));
    assert!(
        matches!(&call.args[0], Expr::IsType(it)
            if matches!(&it.type_expr, TypeExpr::Union(parts) if parts.len() == 2)),
        "{query}: the union stayed inside the call, got {:?}",
        call.args[0]
    );
    assert_map_is_loop_variable(query, &lc);

    // A union inside a parenthesized group, inside a compound WHERE: the
    // `)` after `DATE` continues the union; the `|` after the whole WHERE
    // is the separator.
    let query = "RETURN [x IN [1] WHERE (x IS :: STRING | DATE) AND x = 1 | x] AS y";
    let lc = comprehension_of(query);
    let Some(Expr::Binary(bin)) = &lc.where_expr else {
        panic!(
            "{query}: the WHERE is the conjunction, got {:?}",
            lc.where_expr
        )
    };
    assert_eq!(bin.op, AND);
    assert!(
        matches!(&bin.left, Expr::Paren(p) if matches!(p.as_ref(), Expr::IsType(it)
            if matches!(&it.type_expr, TypeExpr::Union(parts) if parts.len() == 2))),
        "{query}: the union stayed inside the parens, got {:?}",
        bin.left
    );
    assert_map_is_loop_variable(query, &lc);

    // A union inside a nested list literal: the `]` after `INTEGER`
    // continues the union (the list form's proof — a `]` can close the
    // comprehension legally with the map omitted, so it proves nothing
    // there), the comparison chain parses, and with a trailing pipe the
    // comprehension's own separator still separates. The separator-less
    // spelling is the review round's exact repro.
    for query in [
        "RETURN [x IN [1] WHERE [y IS :: STRING | INTEGER] = [] | x] AS y",
        "RETURN [x IN [1] WHERE [y IS :: STRING | INTEGER] = [] ] AS y",
    ] {
        let lc = comprehension_of(query);
        let Some(Expr::Comparison(cmp)) = &lc.where_expr else {
            panic!(
                "{query}: the WHERE is the comparison, got {:?}",
                lc.where_expr
            )
        };
        let Expr::List(items) = &cmp.left else {
            panic!(
                "{query}: the comparison's left is the nested list, got {:?}",
                cmp.left
            )
        };
        let Expr::IsType(it) = &items[0] else {
            panic!(
                "{query}: the list's element is the type predicate, got {:?}",
                items[0]
            )
        };
        assert!(
            matches!(&it.type_expr, TypeExpr::Union(parts) if parts.len() == 2),
            "{query}: the union stayed inside the list"
        );
        if query.ends_with("| x] AS y") {
            assert_map_is_loop_variable(query, &lc);
        } else {
            assert!(lc.map_expr.is_none(), "{query}: no map expression follows");
        }
    }

    // A union inside a nested map literal: the `}` after `INTEGER`
    // continues the union (any `}` tail closes a nested literal; a `}`
    // directly closing a map comprehension's WHERE is malformed either
    // way, so committing only changes which error).
    let query = "RETURN [x IN [1] WHERE {a: x IS :: STRING | INTEGER} = {} | x] AS y";
    let lc = comprehension_of(query);
    let Some(Expr::Comparison(cmp)) = &lc.where_expr else {
        panic!(
            "{query}: the WHERE is the comparison, got {:?}",
            lc.where_expr
        )
    };
    let Expr::Map(pairs) = &cmp.left else {
        panic!(
            "{query}: the comparison's left is the nested map, got {:?}",
            cmp.left
        )
    };
    let Expr::IsType(it) = &pairs[0].value else {
        panic!(
            "{query}: the map's value is the type predicate, got {:?}",
            pairs[0].value
        )
    };
    assert!(
        matches!(&it.type_expr, TypeExpr::Union(parts) if parts.len() == 2),
        "{query}: the union stayed inside the map"
    );
    assert_map_is_loop_variable(query, &lc);
}

/// The expression-continuation class of the whitelist: a boolean
/// connective after a would-be union component continues the expression
/// whose operand is the finished union — the AND/OR/XOR levels legally
/// take a type predicate as an operand, exactly as the comparison chain
/// does — so the union commits greedily, matching the label side's rule
/// (`x:A | B AND y` reads as `(x:A|B) AND y`) and the changelog's "a
/// genuine `IS :: STRING | INTEGER` union inside the filter still parses
/// greedily". The CIP-87 no-chaining set (`IS`, `IN`,
/// `STARTS`/`ENDS`/`CONTAINS` — `parse_string_null_check`'s "at most
/// one" rule) stays dead-end, so the round-2 map bases
/// (`string IS NOT NULL`) still win.
#[test]
fn an_expression_continuation_tail_commits_the_union_greedily() {
    for (query, op) in [
        (
            "RETURN [k IN ['a'] WHERE k IS :: STRING | INTEGER AND true] AS x",
            AND,
        ),
        (
            "RETURN [k IN ['a'] WHERE k IS :: STRING | INTEGER OR false] AS x",
            OR,
        ),
    ] {
        let lc = comprehension_of(query);
        let Some(Expr::Binary(bin)) = &lc.where_expr else {
            panic!(
                "{query}: the WHERE is the connective, got {:?}",
                lc.where_expr
            )
        };
        assert_eq!(bin.op, op, "{query}");
        let Expr::IsType(it) = &bin.left else {
            panic!(
                "{query}: the connective's left is the type predicate, got {:?}",
                bin.left
            )
        };
        assert!(
            matches!(&it.type_expr, TypeExpr::Union(parts) if parts.len() == 2),
            "{query}: the union stayed the filter's operand"
        );
        assert!(lc.map_expr.is_none(), "{query}: no map expression follows");
    }
}

#[test]
fn simple_case_type_predicate_without_is() {
    // `CASE p WHEN :: PATH THEN …` is the `IS`-less spelling of the simple
    // CASE operand type predicate (MatchModes-adjacent Cypher 25 surface).
    let stmt = parse(
        "MATCH p = () RETURN CASE p WHEN :: STRING THEN 'string' \
         WHEN :: PATH THEN 'path' ELSE 'other' END AS result",
    )
    .unwrap();
    let Clause::Return(ret) = &stmt.clauses[1] else {
        panic!("expected return");
    };
    let Expr::Case(case) = &ret.items[0].expr else {
        panic!("expected case expression");
    };
    assert_eq!(case.whens.len(), 2);
    assert!(case
        .whens
        .iter()
        .all(|when| matches!(&when.condition, Expr::IsType(_))));
    assert!(matches!(&case.whens[0].condition, Expr::IsType(_)));
}
