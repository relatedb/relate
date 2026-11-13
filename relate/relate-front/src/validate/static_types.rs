//! Compile-time checks decidable from *literal* operand types, and the
//! literal-type vocabulary ([`super::static_literal`]) they share: boolean-operator operands, `IN` list
//! operands, `properties()` argument types, CIP-271's `toString()`
//! argument-type rejection, and quantifier-variable arithmetic over a
//! statically non-numeric list literal. Page bounds live in
//! [`super::page_bounds`]; the builtin-call families live in
//! [`super::static_calls`], the `reduce()` accumulator rule in
//! [`super::static_reduce`]. Everything whose type depends on runtime data is
//! deliberately left to the runtime evaluator.

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr};

use crate::ast::tree::{is_boolean_operator, walk_clause_list_tree, SubqueryDescent};

use super::static_arithmetic::{list_literal_all_non_numeric, predicate_applies_arithmetic_to_var};
use super::static_literal::{
    homogeneous_list_literal_class, require_static_type, static_type_outside, StaticLitType,
};
use super::static_reduce::check_reduce_temporal_arithmetic;

/// The boolean-operand rule's predicate, shared by the operator gate here and
/// `static_calls`' `allReduce()` predicate check: a statically-known type
/// that is neither BOOLEAN nor NULL.
pub(super) fn is_statically_non_boolean(expr: &Expr) -> bool {
    static_type_outside(expr, BOOLEAN_OPERAND_TYPES).is_some()
}

/// `||` admits strings and lists, plus NULL. Its rejection carries the
/// `TypeError` class Neo4j reports for the concatenation mismatch (CIP-144),
/// not the `SyntaxError` the other static operand checks use.
fn require_string_or_list_literal_operand(operand: &Expr) -> Result<(), EngineError> {
    if let Some(ty) = static_type_outside(
        operand,
        &[
            StaticLitType::String,
            StaticLitType::List,
            StaticLitType::Null,
        ],
    ) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidConcatenationOperand,
            format!(
                "Type mismatch: expected String or List but was {}",
                ty.name()
            ),
        ));
    }
    Ok(())
}

/// The types a boolean operand admits: BOOLEAN, plus NULL (three-valued
/// logic). One list, consulted by the operator gate and
/// [`is_statically_non_boolean`].
const BOOLEAN_OPERAND_TYPES: &[StaticLitType] = &[StaticLitType::Boolean, StaticLitType::Null];

/// Reject a boolean-operator operand whose *literal* type is neither Boolean nor
/// Null. A `null` operand is always allowed (three-valued logic); a non-literal
/// operand is left for runtime. A list *literal* stays a compile-time error
/// (openCypher TCK Boolean1-3 [8]); only a runtime list (a parameter, a
/// function result) takes the deprecated list-to-boolean conversion.
fn require_boolean_literal_operand(operand: &Expr) -> Result<(), EngineError> {
    require_static_type(operand, BOOLEAN_OPERAND_TYPES, |ty| {
        format!("Type mismatch: expected Boolean but was {}", ty.name())
    })
}

/// Compile-time type checking for operators/functions whose operand type is
/// statically knowable from a *literal*. Neo4j/openCypher rejects these during
/// semantic analysis (a compile-time `SyntaxError: InvalidArgumentType`), not at
/// runtime — so `RETURN NOT [1]`, `RETURN 1 IN 2`, and
/// `RETURN properties(1)` are compile errors. This validator reproduces exactly
/// that boundary and *only* for literal operands; every non-literal operand
/// (variable, parameter, function result, …) is left to the runtime evaluator,
/// so no valid query is affected. It rides the shared clause walk
/// ([`walk_clause_list_tree`]) over every clause shape's expressions — the
/// `MERGE` `ON CREATE`/`ON MATCH SET` values, `FOREACH` lists, `CALL`
/// arguments and `WHERE`, `SKIP`/`LIMIT` bounds and write-clause property
/// maps included — exactly as [`super::static_calls`]' pass does; nested
/// statement bodies stay opaque, because the validation battery already
/// visits each as its own node.
///
/// Covered (all `expressions/…` TCK features):
/// - `NOT` / `AND` / `OR` / `XOR` require boolean operands (Boolean1-4).
/// - `x IN <list>` requires a list right operand (List5 scenario 42).
/// - `properties(x)` requires a node/relationship/map (Graph9 scenarios 5-7).
/// - `toString()`/`toStringOrNull()` reject a `LIST<T>` literal (CIP-271).
pub fn validate_static_operator_types(clauses: &[Clause]) -> Result<(), EngineError> {
    walk_clause_list_tree(
        clauses,
        SubqueryDescent::Opaque,
        &mut check_static_operator_types,
    )
}

/// The per-node visitor for [`validate_static_operator_types`].
fn check_static_operator_types(e: &Expr) -> Result<(), EngineError> {
    match e {
        Expr::Unary(u) if is_boolean_operator(u.op) => require_boolean_literal_operand(&u.expr)?,
        // `||` concatenates strings or lists; any other statically-known
        // literal operand is the compile-time type mismatch live Neo4j
        // reports (42001 with cause 22NB1: "expected to be STRING or LIST
        // but was 'INTEGER'").
        Expr::Binary(b) if b.op == "||" => {
            require_string_or_list_literal_operand(&b.left)?;
            require_string_or_list_literal_operand(&b.right)?;
        }
        Expr::Binary(b) if is_boolean_operator(b.op) => {
            require_boolean_literal_operand(&b.left)?;
            require_boolean_literal_operand(&b.right)?;
        }
        // `=~` compares strings; a statically-known non-string operand is
        // the compile-time 42001 live Neo4j reports
        // (`WHERE a.age =~ 13`, cause 22NB1).
        Expr::Comparison(c) if c.op == "=~" => {
            for operand in [&c.left, &c.right] {
                if let Some(ty) =
                    static_type_outside(operand, &[StaticLitType::String, StaticLitType::Null])
                {
                    return Err(EngineError::semantic(
                        ErrorKind::UnexpectedSyntax,
                        format!("Type mismatch: expected String but was {}", ty.name()),
                    ));
                }
            }
        }
        Expr::In(ie) => {
            require_static_type(
                &ie.list,
                &[StaticLitType::List, StaticLitType::Null],
                |ty| format!("IN requires a list, got {}", ty.name()),
            )?;
        }
        Expr::FunctionCall(fc)
            if fc.name.with_str(|n| n.eq_ignore_ascii_case("properties")) && fc.args.len() == 1 =>
        {
            require_static_type(
                &fc.args[0],
                &[StaticLitType::Map, StaticLitType::Null],
                |ty| {
                    format!(
                        "properties() requires a node, relationship, or map, got {}",
                        ty.name()
                    )
                },
            )?;
        }
        // CIP-271's static half: Neo4j's type checker rejects the `toString`
        // family on a `LIST<T>` with a concrete `T` — a list literal whose
        // elements all share one known class — while a dynamically typed list
        // (a property/parameter read, statically `LIST<ANY>`) is accepted and
        // renders in display form. The value-level converter cannot see the
        // argument expression, so the shape decision lives here, beside the
        // other static-literal gates; `EXPLAIN` and a zero-row CASE arm fail
        // with the rest of semantic analysis.
        Expr::FunctionCall(fc)
            if fc.args.len() == 1
                && fc.name.with_str(|n| {
                    n.eq_ignore_ascii_case("tostring") || n.eq_ignore_ascii_case("tostringornull")
                }) =>
        {
            if homogeneous_list_literal_class(&fc.args[0]).is_some() {
                return Err(EngineError::semantic(
                    ErrorKind::InvalidArgumentValue,
                    "toString()",
                ));
            }
        }
        // A list quantifier (`all`/`any`/`none`/`single`) whose loop variable
        // ranges over a list *literal* of statically non-numeric elements, and
        // whose predicate applies an arithmetic operator directly to that loop
        // variable, is a compile-time type error in openCypher — e.g.
        // `all(x IN ['Clara'] WHERE x % 2 = 0)`
        // (expressions/quantifier/Quantifier1-4 scenario "[15]/[16] Fail …
        // quantifier on type mismatch between list elements and predicate",
        // `SyntaxError … : InvalidArgumentType` at compile time). Relate
        // otherwise catches it at runtime in `eval::binary`; this narrow static
        // pass moves the statically-decidable case to compile time. Only literal
        // lists with fully-known non-numeric element types qualify, so every
        // query whose element type depends on runtime data is untouched.
        Expr::Quantifier(q) => {
            if let Some(pred) = &q.predicate {
                if list_literal_all_non_numeric(&q.list)
                    && q.variable
                        .with_str(|text| predicate_applies_arithmetic_to_var(pred, text))
                {
                    return Err(EngineError::semantic(
                        ErrorKind::InvalidArgumentType,
                        format!(
                            "Type mismatch: the quantifier variable '{}' ranges over a list of \
                             non-numeric elements and cannot be used in arithmetic",
                            q.variable
                        ),
                    ));
                }
            }
        }
        // The `reduce` analogue: a numerically-initialised accumulator
        // combined by an arithmetic operator with an element of a list
        // *literal* of temporal constructor calls is the front-end type fault
        // live Neo4j reports for the same statement (probed 2026.10.1,
        // `reduce(a = 1, b in [date()] | a + b)` -> 42001 SyntaxError). Only
        // the statically decidable numeric-vs-temporal pair qualifies; every
        // other operand shape keeps the executor's runtime TypeError.
        Expr::Reduce(r) => check_reduce_temporal_arithmetic(r)?,
        _ => {}
    }
    Ok(())
}

/// Parse `query`, run only the narrow static operator/type pass, and return
/// its verdict. The one test entry point for this pass; the sibling static
/// rules' tests call it too rather than re-encoding the call.
#[cfg(test)]
pub(super) fn parse_and_check_static_ops(query: &str) -> Result<(), EngineError> {
    let stmt = relate_parser::parse(query).expect("parse");
    validate_static_operator_types(&stmt.clauses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::tree::tests::return_expr_of;
    use relate_core::Value;

    /// The 22N27/22NB1 detail text renders a folded operand's type through
    /// core's `bare_type_name` and a declined-but-typed one through
    /// [`StaticLitType::name`]: one vocabulary, two call sites. A spelling
    /// change on either side must fail here. `List` sits outside the overlap
    /// by design — the front renders the *kind* (`LIST`), core renders a
    /// list value's element union (`LIST<INTEGER>`), and a folded list always
    /// takes the core path.
    #[test]
    fn static_type_names_match_the_core_vocabulary() {
        use relate_core::bare_type_name;

        for (front, core) in [
            (StaticLitType::Null, Value::Null),
            (StaticLitType::Boolean, Value::Bool(true)),
            (StaticLitType::Integer, Value::Int(0)),
            (StaticLitType::Float, Value::Float(0.0)),
            (StaticLitType::String, Value::String("".into())),
            (StaticLitType::Map, Value::Map(Default::default())),
        ] {
            assert_eq!(front.name(), bare_type_name(&core), "{front:?}");
        }
    }

    // ---- Quantifier over a non-numeric list literal: compile-time TypeError ----
    //
    // openCypher rejects `all/any/none/single(x IN <non-numeric list literal>
    // WHERE <arith on x>)` during semantic analysis, as a `SyntaxError … :
    // InvalidArgumentType` at compile time (expressions/quantifier/Quantifier1-4
    // "[15]/[16] Fail … quantifier on type mismatch between list elements and
    // predicate"). Relate's runtime evaluator would otherwise raise the same
    // TypeError one phase too late.

    /// Parse `query`, run only the narrow static operator/type pass, and return
    /// its verdict.
    fn parse_and_check_static_ops(query: &str) -> Result<(), EngineError> {
        let stmt = relate_parser::parse(query).expect("parse");
        validate_static_operator_types(&stmt.clauses)
    }

    #[test]
    fn quantifier_over_string_literal_arith_predicate_is_compile_time_type_error() {
        // Every quantifier keyword, over both a STRING and a BOOLEAN list literal
        // (the exact Quantifier1-4 [15]/[16] examples). Each must be a *compile*
        // (Semantic) error whose kind is InvalidArgumentType — the detail the
        // TCK step pins.
        for q in ["all", "any", "none", "single"] {
            for list in [
                "['Clara']",
                "['Clara', 'Bob', 'Dave', 'Alice']",
                "[false, true]",
            ] {
                let query = format!("RETURN {q}(x IN {list} WHERE x % 2 = 0) AS result");
                let err = parse_and_check_static_ops(&query)
                    .expect_err(&format!("expected a compile-time type error for `{query}`"));
                assert!(
                    matches!(err, EngineError::Semantic { .. }),
                    "`{query}` must fail at compile time (Semantic), got {err:?}"
                );
                assert_eq!(
                    err.kind(),
                    ErrorKind::InvalidArgumentType,
                    "`{query}` must carry the InvalidArgumentType detail"
                );
                assert_eq!(err.tck_class(), relate_error::TckErrorClass::TypeError);
            }
        }
    }

    #[test]
    fn quantifier_over_numeric_literal_is_untouched() {
        // The numeric-list quantifier the tested type-mismatch scenarios are the
        // negative of: `x % 2 = 0` over integers/floats is perfectly valid and
        // must pass the static pass unchanged (no false positive).
        for query in [
            "RETURN all(x IN [1, 2] WHERE x % 2 = 0) AS result",
            "RETURN any(x IN [1.5, 2.5] WHERE x / 2 > 0) AS result",
            "RETURN none(x IN [1, 2, 3] WHERE x - 1 = 0) AS result",
            "RETURN single(x IN [10] WHERE x * 2 = 20) AS result",
            // A list mixing numbers and null is not statically all-non-numeric.
            "RETURN all(x IN [1, null, 3] WHERE x % 2 = 0) AS result",
        ] {
            assert!(
                parse_and_check_static_ops(query).is_ok(),
                "`{query}` must not be rejected by the static pass"
            );
        }
    }

    #[test]
    fn quantifier_non_arithmetic_predicate_over_strings_is_untouched() {
        // The *valid* Quantifier[5]/[6]/[7]/[11] scenarios use `size(x)`, `x.a`,
        // `x IS NULL`, `x = …` — never arithmetic on the loop variable — and must
        // not be mistaken for the type-mismatch case.
        for query in [
            "RETURN all(x IN ['abc', 'ef'] WHERE size(x) = 3) AS result",
            "RETURN any(x IN [{a: 2}, {a: 4}] WHERE x.a = 2) AS result",
            "RETURN none(x IN ['a', 'b'] WHERE x IS NULL) AS result",
            "RETURN single(x IN [[1, 2, 3]] WHERE size(x) = 3) AS result",
            // `+` on strings is concatenation, not arithmetic — never a
            // compile-time error.
            "RETURN all(x IN ['a', 'b'] WHERE x + 'y' = 'ay') AS result",
            // Arithmetic applies to a *derived* value, not the bare loop var.
            "RETURN all(x IN ['1', '2'] WHERE toInteger(x) % 2 = 0) AS result",
        ] {
            assert!(
                parse_and_check_static_ops(query).is_ok(),
                "`{query}` must not be rejected by the static pass"
            );
        }
    }

    #[test]
    fn quantifier_shadowing_inner_numeric_var_is_not_a_false_positive() {
        // The inner `any` rebinds `x` to a numeric list; the outer `all`'s
        // string binding never reaches the inner `x % 2`. The shadow guard in
        // `predicate_applies_arithmetic_to_var` must keep this valid.
        let query = "RETURN all(x IN ['a'] WHERE any(x IN [1, 2] WHERE x % 2 = 0)) AS result";
        assert!(
            parse_and_check_static_ops(query).is_ok(),
            "shadowed inner numeric quantifier must not trip the outer static check"
        );
    }

    #[test]
    fn quantifier_static_helpers_are_narrow() {
        // list_literal_all_non_numeric: only fully-known non-numeric literals.
        let all_non_numeric =
            |q: &str| list_literal_all_non_numeric(&return_expr_of(&format!("RETURN {q}")));
        assert!(all_non_numeric("['a', 'b']"));
        assert!(all_non_numeric("[false, true]"));
        assert!(all_non_numeric("[[1], ['a']]"));
        assert!(all_non_numeric("[{a: 1}]"));
        assert!(!all_non_numeric("[]")); // empty ⇒ nothing to constrain
        assert!(!all_non_numeric("[1, 2]")); // numeric
        assert!(!all_non_numeric("['a', 1]")); // heterogeneous ⇒ runtime-dependent
        assert!(!all_non_numeric("['a', null]")); // null ⇒ arithmetic yields null
        assert!(!all_non_numeric("$p")); // not a list literal

        // predicate_applies_arithmetic_to_var: bare-var operand under a failing op.
        let hits = |q: &str| {
            let e = return_expr_of(&format!("RETURN {q}"));
            predicate_applies_arithmetic_to_var(&e, "x")
        };
        assert!(hits("x % 2 = 0"));
        assert!(hits("2 - x > 0"));
        assert!(hits("x ^ 2 = 4"));
        assert!(hits("x = 1 OR x / 2 = 0"));
        assert!(!hits("x + 'y' = 'z'")); // `+` excluded (concat/append)
        assert!(!hits("size(x) = 3")); // no arithmetic on x
        assert!(!hits("toInteger(x) % 2 = 0")); // operand is derived, not bare x
        assert!(!hits("y % 2 = 0")); // different variable
    }

    #[test]
    fn quantifier_type_mismatch_surfaces_through_full_engine_at_compile_time() {
        // End-to-end through the engine: the query must error, as a compile
        // (Semantic) InvalidArgumentType, before any row evaluation.
        // Compile-time validation: a static list of strings under `%` is InvalidArgumentType.
        let err =
            parse_and_check_static_ops("RETURN all(x IN ['Clara'] WHERE x % 2 = 0) AS result")
                .expect_err("quantifier type mismatch must error");
        assert_eq!(err.kind(), ErrorKind::InvalidArgumentType);
        assert!(
            matches!(err, EngineError::Semantic { .. }),
            "must be a compile-time (Semantic) error, got {err:?}"
        );
        assert_eq!(err.kind(), ErrorKind::InvalidArgumentType);
    }

    // ---- The pass rides the shared clause walk: every clause shape's
    // expressions are checked, not just the projection/where roots the
    // retired hand-rolled clause match reached ----

    /// A `MERGE`'s `ON CREATE`/`ON MATCH SET` value with a statically
    /// mistyped operand is rejected at compile time — the pinned shape of
    /// the wider walk (F-1-53: the check fires before any row exists, as
    /// a Semantic InvalidArgumentType).
    #[test]
    fn merge_on_create_and_on_match_set_value_type_errors_are_compile_time() {
        for query in [
            "MERGE (n:Person {id: 1}) ON CREATE SET n.flag = NOT [1]",
            "MERGE (n:Person {id: 1}) ON MATCH SET n.flag = NOT [1]",
        ] {
            let err = parse_and_check_static_ops(query)
                .expect_err(&format!("`{query}` must be rejected statically"));
            assert!(
                matches!(err, EngineError::Semantic { .. }),
                "`{query}` must fail at compile time (Semantic), got {err:?}"
            );
            assert_eq!(
                err.kind(),
                ErrorKind::InvalidArgumentType,
                "`{query}` must carry the InvalidArgumentType detail"
            );
        }
    }

    /// The other clause shapes the retired hand-rolled match never reached:
    /// FOREACH lists, CALL arguments and WHERE, SKIP/LIMIT bounds and
    /// write-clause property maps all carry their expressions into the
    /// shared walk, so a statically mistyped operand fails there too.
    #[test]
    fn every_clause_shapes_expressions_are_compile_time_checked() {
        for query in [
            // FOREACH's list expression.
            "FOREACH (x IN NOT [1] | CREATE (n {v: 1}))",
            // A CALL procedure argument and its WHERE filter.
            "CALL db.labels(NOT [1]) YIELD label WHERE label IN 2",
            // SKIP and LIMIT bounds.
            "RETURN 1 AS n ORDER BY n SKIP NOT [1] LIMIT NOT [1]",
            // CREATE and MERGE property-map values.
            "CREATE (n {flag: NOT [1]})",
            "MERGE (n:Person {flag: NOT [1]})",
        ] {
            let err = parse_and_check_static_ops(query)
                .expect_err(&format!("`{query}` must be rejected statically"));
            assert!(
                matches!(err, EngineError::Semantic { .. }),
                "`{query}` must fail at compile time (Semantic), got {err:?}"
            );
        }
    }

    // ---- CIP-271: toString()/toStringOrNull() reject a homogeneous LIST<T> ----

    #[test]
    fn tostring_rejects_a_homogeneous_list_literal_at_compile_time() {
        // The four shapes Neo4j's checker rejects: homogeneous INTEGER,
        // STRING and nested-LIST literals, plus the mixed number list (INTEGER
        // and FLOAT are one class). Both spellings of the function share the
        // rule; the error is the InvalidArgumentValue detail CIP-271 pins.
        for query in [
            "RETURN toString([1, 2]) AS result",
            "RETURN toString(['a', 'b']) AS result",
            "RETURN toString([1, 2.0]) AS result",
            "RETURN toString([[1]]) AS result",
            "RETURN toString([1 + 1, 2]) AS result",
            "RETURN toStringOrNull([1, 2]) AS result",
            "RETURN toStringOrNull(['a', 'b']) AS result",
        ] {
            let err = parse_and_check_static_ops(query)
                .expect_err(&format!("`{query}` must be rejected statically"));
            assert!(
                matches!(err, EngineError::Semantic { .. }),
                "`{query}` must fail at compile time (Semantic), got {err:?}"
            );
            assert_eq!(
                err.kind(),
                ErrorKind::InvalidArgumentValue,
                "`{query}` must carry the InvalidArgumentValue detail"
            );
            assert_eq!(err.tck_class(), relate_error::TckErrorClass::TypeError);
        }
    }

    #[test]
    fn tostring_accepts_a_dynamic_or_mixed_list() {
        // `LIST<ANY>` (or a non-list): empty, mixed classes, a null element, a
        // variable/property read, and a list whose elements the static pass
        // cannot classify all fall through to the runtime display renderer.
        for query in [
            "RETURN toString([]) AS result",
            "RETURN toString([1, 'a']) AS result",
            "RETURN toString([1, null, 'a']) AS result",
            "RETURN toString([true, null]) AS result",
            "RETURN toString($p) AS result",
            "UNWIND [[1, 2]] AS xs RETURN toString(xs) AS result",
            "MATCH (n) RETURN toString(n.ids) AS result",
            "RETURN toString(1) AS result",
            "RETURN toString({a: 1}) AS result",
        ] {
            assert!(
                parse_and_check_static_ops(query).is_ok(),
                "`{query}` must not be rejected by the static pass"
            );
        }
    }

    #[test]
    fn tostring_static_gate_applies_under_every_preamble_spelling() {
        // The static `LIST<T>` rejection (CIP-271) is the one rule under every
        // spelling: the homogeneous list literal is rejected exactly like
        // the unprefixed form under the one admitted `CYPHER 25` preamble.
        for query in [
            "CYPHER 25 RETURN toString([1, 2]) AS result",
            "RETURN toString([1, 2]) AS result",
            "CYPHER 25 RETURN toStringOrNull([1, 2]) AS result",
            "RETURN toStringOrNull([1, 2]) AS result",
        ] {
            let error = parse_and_check_static_ops(query)
                .expect_err(&format!("`{query}` must be rejected"));
            assert!(
                error.to_string().contains("toString()"),
                "`{query}`: {error}"
            );
        }
    }

    #[test]
    fn genuinely_runtime_arithmetic_type_error_stays_runtime_typeerror() {
        // A non-static operand (bound at row time by UNWIND) is NOT caught by the
        // static pass — it must still raise at RUNTIME, now correctly classed as
        // a TypeError (InvalidArgumentType) rather than the old Uncategorized/
        // SemanticError.
        // A non-static operand is NOT caught by the static pass.
        assert!(parse_and_check_static_ops("UNWIND ['abc'] AS x RETURN x % 2").is_ok());
    }
}
