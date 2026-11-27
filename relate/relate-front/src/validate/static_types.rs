//! Compile-time checks decidable from *literal* operand types, and the
//! literal-type vocabulary ([`super::static_literal`]) they share:
//! boolean-operator operands, `IN` list operands, and quantifier-variable
//! arithmetic over a statically non-numeric list literal. The builtin-call
//! argument rules — `properties()` and the rest — live in
//! [`super::static_calls`]; page bounds live in [`super::page_bounds`], the
//! `reduce()` accumulator rule in [`super::static_reduce`]. Everything whose
//! type depends on runtime data is deliberately left to the runtime evaluator.

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr};

use crate::ast::tree::{is_boolean_operator, walk_clause_list_tree_with_clauses, SubqueryDescent};

use super::static_arithmetic::check_quantifier_arithmetic;
use super::static_literal::{
    boolean_operand_check, is_concat_operator, is_regex_operator, list_returning_builtin_error,
    require_static_type, require_static_type_kind, BooleanOperandCheck, LIST_OR_NULL,
    STRING_OR_LIST_OR_NULL, STRING_OR_NULL,
};
use super::static_reduce::{check_reduce_accumulator_type, check_reduce_temporal_arithmetic};

/// One boolean position's operand contract, stated once: both rules'
/// authored rejections — the literal rule's operand-mismatch sentence
/// (the `InvalidArgumentType` class the `require_static_type` gate
/// carries) and the builtin rule's reference-server coercion sentence
/// ([`list_returning_builtin_error`], ADR-0715). The operator arms read
/// this; positions that pin their own literal-rule sentence
/// (`static_calls`' `allReduce()` predicate) read [`boolean_operand_check`]
/// and match its answer directly.
fn require_boolean_operand(operand: &Expr) -> Result<(), EngineError> {
    match boolean_operand_check(operand) {
        BooleanOperandCheck::NonBooleanLiteral(kind) => Err(EngineError::semantic(
            ErrorKind::InvalidArgumentType,
            format!("Type mismatch: expected Boolean but was {}", kind.name()),
        )),
        BooleanOperandCheck::ListReturningBuiltin => Err(list_returning_builtin_error()),
        // A gated name is the admission gate's to answer; this position
        // defers ([`BooleanOperandCheck::DeferredToAdmission`]).
        BooleanOperandCheck::DeferredToAdmission | BooleanOperandCheck::Admitted => Ok(()),
    }
}

/// `||` admits strings and lists, plus NULL. Its rejection carries the
/// `TypeError` class Neo4j reports for the concatenation mismatch,
/// not the `SyntaxError` the other static operand checks use.
fn require_string_or_list_literal_operand(operand: &Expr) -> Result<(), EngineError> {
    require_static_type_kind(
        operand,
        STRING_OR_LIST_OR_NULL,
        ErrorKind::InvalidConcatenationOperand,
        |ty| {
            format!(
                "Type mismatch: expected String or List but was {}",
                ty.name()
            )
        },
    )
}

/// `=~` compares strings; a statically-known non-string operand is the
/// compile-time 42001 live Neo4j reports (`WHERE a.age =~ 13`, cause 22NB1).
fn require_string_literal_operand(operand: &Expr) -> Result<(), EngineError> {
    require_static_type_kind(operand, STRING_OR_NULL, ErrorKind::UnexpectedSyntax, |ty| {
        format!("Type mismatch: expected String but was {}", ty.name())
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
/// ([`walk_clause_list_tree_with_clauses`]) over every clause shape's expressions — the
/// `MERGE` `ON CREATE`/`ON MATCH SET` values, `FOREACH` lists, `CALL`
/// arguments and `WHERE`, `SKIP`/`LIMIT` bounds and write-clause property
/// maps included — exactly as [`super::static_calls`]' pass does; nested
/// statement bodies stay opaque, because the validation battery already
/// visits each as its own node.
///
/// Covered (all `expressions/…` TCK features):
/// - `NOT` / `AND` / `OR` / `XOR` require boolean operands (Boolean1-4).
/// - `x IN <list>` requires a list right operand (List5 scenario 42).
pub fn validate_static_operator_types(clauses: &[Clause]) -> Result<(), EngineError> {
    walk_clause_list_tree_with_clauses(
        clauses,
        SubqueryDescent::Opaque,
        &mut check_static_operator_types,
        &mut check_where_root,
    )
}

/// ADR-0715's predicate-position roots: the `WHERE` of a `MATCH`, `WITH`
/// or `CALL … YIELD … WHERE` (the one table: [`Clause::predicate_where`])
/// is itself a boolean position, so its root expression answers the same
/// builtin-list rejection the operator operands get. Only the builtin
/// half of the policy applies here — the literal half's root rejection is
/// the planner's own (its 42N06 SyntaxError class is pinned), and nested
/// boolean positions inside the root (an `AND`, a `NOT`) are the
/// expression visitor's. A path pattern's own per-element `WHERE` is
/// deliberately out of scope (ADR-0715): it is a row-level predicate
/// inside the pattern, evaluated like the dynamically-typed shapes, and
/// no reference rejection pins it.
fn check_where_root(clause: &Clause) -> Result<(), EngineError> {
    if let Some(expr) = clause.predicate_where() {
        if matches!(
            boolean_operand_check(expr),
            BooleanOperandCheck::ListReturningBuiltin
        ) {
            return Err(list_returning_builtin_error());
        }
    }
    Ok(())
}

/// The per-node visitor for [`validate_static_operator_types`].
fn check_static_operator_types(e: &Expr) -> Result<(), EngineError> {
    match e {
        Expr::Unary(u) if is_boolean_operator(u.op) => require_boolean_operand(&u.expr)?,
        // `||` concatenates strings or lists; any other statically-known
        // literal operand is the compile-time type mismatch live Neo4j
        // reports (42001 with cause 22NB1: "expected to be STRING or LIST
        // but was 'INTEGER'").
        Expr::Binary(b) if is_concat_operator(b.op) => {
            require_string_or_list_literal_operand(&b.left)?;
            require_string_or_list_literal_operand(&b.right)?;
        }
        Expr::Binary(b) if is_boolean_operator(b.op) => {
            require_boolean_operand(&b.left)?;
            require_boolean_operand(&b.right)?;
        }
        // `=~` compares strings; a statically-known non-string operand is
        // the compile-time 42001 live Neo4j reports
        // (`WHERE a.age =~ 13`, cause 22NB1).
        Expr::Comparison(c) if is_regex_operator(c.op) => {
            require_string_literal_operand(&c.left)?;
            require_string_literal_operand(&c.right)?;
        }
        Expr::In(ie) => {
            require_static_type(&ie.list, LIST_OR_NULL, |ty| {
                format!("IN requires a list, got {}", ty.name())
            })?;
        }
        // A list quantifier's arithmetic rule: its decision lives in
        // `static_arithmetic` beside every piece it reads.
        Expr::Quantifier(q) => check_quantifier_arithmetic(q)?,
        // The `reduce` static rules, one arm per policy in
        // `static_reduce`: the temporal-arithmetic fault (probed 2026.10.1,
        // `reduce(a = 1, b in [date()] | a + b)` -> 42001 SyntaxError) and
        // the accumulator/body unification (`reduce(a = 1, b in ['s'] |
        // 's')` -> 22G03). Only statically decidable shapes qualify; every
        // other operand shape keeps the executor's runtime TypeError.
        Expr::Reduce(r) => {
            check_reduce_temporal_arithmetic(r)?;
            check_reduce_accumulator_type(r)?;
        }
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

    use super::super::static_arithmetic::{
        list_literal_all_non_numeric, predicate_applies_arithmetic_to_var,
    };

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

    #[test]
    fn genuinely_runtime_arithmetic_type_error_stays_runtime_typeerror() {
        // A non-static operand (bound at row time by UNWIND) is NOT caught by the
        // static pass — it must still raise at RUNTIME, now correctly classed as
        // a TypeError (InvalidArgumentType) rather than the old Uncategorized/
        // SemanticError.
        // A non-static operand is NOT caught by the static pass.
        assert!(parse_and_check_static_ops("UNWIND ['abc'] AS x RETURN x % 2").is_ok());
    }
    /// ADR-0715: a builtin call whose catalog-declared return type is a
    /// list, in a boolean position, is the compile-time coercion
    /// rejection — the WHERE root and every boolean-operator operand —
    /// with the reference server's 22N37 sentence. Dynamically-typed shapes (a
    /// parameter, a property read) keep the runtime conversion, and a
    /// projection position is never a boolean position.
    #[test]
    fn a_list_returning_builtin_call_in_a_boolean_position_rejects() {
        for query in [
            "WITH 1 AS x WHERE tail([1, 2, 3]) RETURN x",
            "MATCH (n) WHERE tail([1, 2, 3]) RETURN n",
            "RETURN true AND tail([1])",
            "RETURN NOT tail([1])",
            "WITH [1] AS l RETURN l = [] OR labels(null)",
            // Paren-transparent: the wrapped spellings reject exactly
            // like their bare twins (the literal half's contract).
            "WITH 1 AS x WHERE (tail([1, 2, 3])) RETURN x",
            "RETURN true AND (tail([1]))",
            "RETURN NOT (tail([1]))",
        ] {
            let error = parse_and_check_static_ops(query)
                .expect_err("a list-returning call in a boolean position must reject");
            assert_eq!(error.kind(), ErrorKind::TypeError, "{query}: {error}");
            assert!(
                error.to_string().contains("Cannot coerce LIST to BOOLEAN"),
                "{query}: {error}"
            );
        }
    }

    #[test]
    fn the_dynamically_typed_and_projection_shapes_keep_their_answers() {
        for query in [
            // A parameter and a property read: the deprecated runtime
            // conversion, pinned by the reference server's passing behavior.
            "WITH 1 AS x WHERE $list RETURN x",
            "MATCH (n:R) WHERE n.coll RETURN n",
            "RETURN true AND $list",
            // A projection position: returning the list is the point.
            "RETURN tail([1, 2, 3])",
            // A comparison against a list result is not a boolean
            // position for the call itself.
            "RETURN tail([1]) = [1]",
            // A non-list builtin is unaffected.
            "WITH 1 AS x WHERE toString(1) RETURN x",
        ] {
            assert!(
                parse_and_check_static_ops(query).is_ok(),
                "{query} keeps its answer"
            );
        }
    }

    /// The CALL `WHERE` root answers the same rejection as MATCH/WITH —
    /// ADR-0715's "wherever a boolean is required", reaching every
    /// clause-level predicate root the walk hands over.
    #[test]
    fn a_call_yield_where_root_rejects_the_list_returning_call() {
        let error = parse_and_check_static_ops(
            "CALL db.labels() YIELD label WHERE tail([label]) RETURN label",
        )
        .expect_err("a CALL WHERE root is a boolean position");
        assert!(
            error.to_string().contains("Cannot coerce LIST to BOOLEAN"),
            "{error}"
        );
    }
}
