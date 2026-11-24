//! The front-end static argument checks for builtin calls: the literal
//! argument types and value domains the statement text decides (`vector()`
//! construction and similarity, `char_length()`, `nullif()`/`format()`
//! arity, `replace()`'s limit, the temporal constructors' `(value, pattern)`
//! form, quantifier/`allReduce()` list operands, path-selector counts, and
//! `IN TRANSACTIONS` concurrency).
//!
//! Neo4j performs these during semantic analysis and reports them as
//! compile-time `SyntaxError`s: a literal of the wrong type — or a literal
//! value outside its domain — is a front-end rejection, never a row-time
//! failure. The executor keeps its runtime backstop for arguments whose type
//! is only known per row (a property read, a `CASE` result), so the two
//! classes split on "statically knowable", exactly as live Neo4j splits
//! them. Every rejection here rides the front-end boundary, which reports the
//! SyntaxError class.

use relate_core::{
    builtin_arity, homogeneous_element_class, LiteralElementClass, LiteralValueKind, Value,
    TOSTRING_TYPE_ERROR_DETAIL,
};
use relate_error::{EngineError, ErrorKind};
use relate_model::VectorCoordToken;
use relate_parser::{Clause, Concurrency, Expr, FunctionCallExpr, PathCount, PathSelector};

use crate::ast::tree::{unparen, walk_clause_list_tree, SubqueryDescent};

use super::static_arithmetic::expr_is_bare_var;
use super::static_literal::{
    non_numeric_leaf_class, require_integer_argument, require_static_type, static_int_value,
    static_list_element_kinds, static_literal_type, static_string_value, INTEGER_ONLY,
    INTEGER_OR_NULL, LIST_OR_NULL, MAP_OR_NULL, STRING_OR_NULL,
};
use super::static_types::is_statically_non_boolean;

/// Apply this module's static argument checks to one statement node's
/// clauses (the module contract above).
pub fn validate_static_call_types(clauses: &[Clause]) -> Result<(), EngineError> {
    // Every expression of this node's clause shapes (the CIP-168 page stages
    // included). Nested statement bodies are opaque here: the validation
    // battery already visits each as its own node, so entering them would
    // re-check every body once per ancestor.
    walk_clause_list_tree(clauses, SubqueryDescent::Opaque, &mut check_static_call)?;
    for clause in clauses {
        match clause {
            Clause::Match(mc) => {
                for path in &mc.pattern {
                    check_path_selector(path.selector.as_ref())?;
                }
            }
            Clause::CallSubquery(cs) => {
                if let Some(in_tx) = &cs.in_transactions {
                    check_concurrency(&in_tx.concurrency)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A path-selector count is a positive integer. The parser rejects a literal
/// zero outright; a plan-cache placeholder (the literal it replaced) and a
/// caller's parameter arrive as `PathCount::Parameter`, and only the
/// placeholder is decidable here.
fn check_path_selector(selector: Option<&PathSelector>) -> Result<(), EngineError> {
    let count = match selector {
        Some(PathSelector::Any(count)) => count.as_ref(),
        Some(PathSelector::Shortest { count, .. }) => count.as_ref(),
        _ => None,
    };
    let Some(PathCount::Parameter(name)) = count else {
        return Ok(());
    };
    match relate_core::auto_literal(name) {
        Some(Value::Int(value)) if value > 0 => Ok(()),
        Some(Value::Int(_)) => Err(EngineError::semantic(
            ErrorKind::NegativeIntegerArgument,
            "path selector count must be positive".to_string(),
        )),
        Some(_) => Err(EngineError::semantic(
            ErrorKind::InvalidArgumentType,
            "path selector count must be an integer".to_string(),
        )),
        // A caller's own parameter: the runtime resolves it.
        None => Ok(()),
    }
}

/// CIP-34's concurrency value is an integer; a statically-known value of any
/// other type (`IN null CONCURRENT TRANSACTIONS`) is a compile-time type
/// error.
fn check_concurrency(concurrency: &Concurrency) -> Result<(), EngineError> {
    let Concurrency::Value(expr) = concurrency else {
        return Ok(());
    };
    // A null concurrency is rejected here too: CIP-34's value is an integer.
    require_integer_argument(expr, "concurrency", INTEGER_ONLY)
}

/// The per-node visitor for [`validate_static_call_types`].
fn check_static_call(e: &Expr) -> Result<(), EngineError> {
    match e {
        Expr::FunctionCall(fc) => check_static_function_call(fc),
        Expr::Quantifier(q) => require_list_operand(&q.list, "quantifier"),
        Expr::AllReduce(r) => check_all_reduce(r),
        _ => Ok(()),
    }
}

fn check_static_function_call(fc: &FunctionCallExpr) -> Result<(), EngineError> {
    let name = fc.name.with_str(str::to_ascii_lowercase);
    check_builtin_arity(&name, &fc.args)?;
    match name.as_str() {
        "vector" => check_vector_call(&fc.args),
        // Both take two vector-shaped operands. A missing/extra argument is
        // the runtime arity rejection (a dynamic shape, not a type fault).
        "vector.similarity.cosine" | "vector.similarity.euclidean" if fc.args.len() == 2 => {
            require_vector_operand(&fc.args[0], &name)?;
            require_vector_operand(&fc.args[1], &name)
        }
        "char_length" | "character_length" => check_string_argument(&fc.args, &format!("{name}()")),
        // CIP-271's static half. `toStringOrNull()` is deliberately not
        // matched: it maps the rule's type error to null at the value level
        // and must not raise.
        "tostring" => check_tostring_argument(&fc.args),
        "properties" => check_properties_argument(&fc.args),
        // Arity alone: the contract above owns the count and its message.
        "nullif" | "format" => Ok(()),
        "replace" => check_replace_limit(&fc.args),
        n if relate_core::temporal::is_constructor(n) => check_temporal_call(&fc.args),
        _ => Ok(()),
    }
}

/// CIP-271's static half: `toString()` rejects a `LIST<T>` with a concrete
/// `T` — a list literal whose elements all share one known class. The rule
/// is implemented once (`homogeneous_element_class`) and the executor
/// applies the same one to a runtime value, so a list is refused whatever
/// spelling produced it. The static check still owns the compile-time phase
/// for the shapes it can type; `EXPLAIN` and a zero-row CASE arm fail with
/// the rest of semantic analysis.
fn check_tostring_argument(args: &[Expr]) -> Result<(), EngineError> {
    let [arg] = args else {
        return Ok(());
    };
    if is_homogeneous_list_literal(arg) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidArgumentValue,
            TOSTRING_TYPE_ERROR_DETAIL,
        ));
    }
    Ok(())
}

/// `properties(x)` requires a node, relationship or map; a statically known
/// literal of another kind is the compile-time rejection (Graph9 5-7).
fn check_properties_argument(args: &[Expr]) -> Result<(), EngineError> {
    let [arg] = args else {
        return Ok(());
    };
    require_static_type(arg, MAP_OR_NULL, |ty| {
        format!(
            "properties() requires a node, relationship, or map, got {}",
            ty.name()
        )
    })
}

/// Whether `list` is a list *literal* (or the plan-cache placeholder that
/// replaced one) whose elements all share one known element class — the
/// `LIST<T>` CIP-271 rejects for `toString()`. The element derivation is the
/// one shared [`static_list_element_kinds`]; the equality rule (the
/// empty-list case included) is the one core [`homogeneous_element_class`].
fn is_homogeneous_list_literal(list: &Expr) -> bool {
    static_list_element_kinds(list).is_some_and(|kinds| {
        homogeneous_element_class(
            kinds
                .into_iter()
                .map(|kind| kind.and_then(LiteralElementClass::of_kind)),
        )
        .is_some()
    })
}

/// The cross-layer arity contract `relate-core` owns for `name`: a call with
/// the wrong argument count is the compile-phase
/// `InvalidNumberOfArguments` rejection. One table feeds this check and the
/// planner's rule dispatch, so the count and the authored message have one
/// home.
fn check_builtin_arity(name: &str, args: &[Expr]) -> Result<(), EngineError> {
    let Some(contract) = builtin_arity(name) else {
        return Ok(());
    };
    match contract.violation(args.len()) {
        // The contract carries the bare detail; the typed channel composes
        // the class token exactly once.
        Some(detail) => Err(EngineError::semantic(
            ErrorKind::InvalidNumberOfArguments,
            detail,
        )),
        None => Ok(()),
    }
}

/// `vector(list, dimension, coordinateType)` — the only supported shape. The
/// three-argument count is the shared arity contract checked before this
/// function; the parser produces the coordinate type as a literal string, so
/// its vocabulary is the other static fact here.
fn check_vector_call(args: &[Expr]) -> Result<(), EngineError> {
    let [list, dimension_arg, coordinate_type]: &[Expr; 3] = args
        .try_into()
        .expect("check_builtin_arity admitted exactly three vector() arguments");
    if let Some(coord) = static_string_value(coordinate_type) {
        if relate_model::VectorCoordKind::from_alias(&coord).is_none() {
            return Err(EngineError::semantic(
                ErrorKind::InvalidArgumentType,
                format!("Invalid vector inner type: {coord}"),
            ));
        }
    }
    require_integer_argument(dimension_arg, "vector() dimension", INTEGER_OR_NULL)?;
    if let Some(dimension) = static_int_value(dimension_arg) {
        if !relate_model::VECTOR_DIMENSION_RANGE.contains(&dimension) {
            return Err(EngineError::semantic(
                ErrorKind::NumberOutOfRange,
                format!(
                    "vector() dimension must be in the range {}",
                    relate_model::dimension_range_text()
                ),
            ));
        }
    }
    match static_literal_type(list) {
        Some(LiteralValueKind::String) => {
            if let Some(text) = static_string_value(list) {
                if coordinate_text_is_malformed(&text) {
                    return Err(EngineError::semantic(
                        ErrorKind::InvalidArgumentType,
                        "InvalidVectorCoordinates".to_string(),
                    ));
                }
            }
            Ok(())
        }
        Some(LiteralValueKind::List) => require_numeric_list_elements(list, "vector()"),
        None | Some(LiteralValueKind::Null) => Ok(()),
        Some(ty) => Err(EngineError::semantic(
            ErrorKind::InvalidArgumentType,
            format!("vector() requires a numeric list, got {}", ty.name()),
        )),
    }
}

/// Whether a literal coordinate string is malformed at the *input* level:
/// missing brackets, or a token the shared tokenizer classifies as
/// [`VectorCoordToken::Malformed`]. Live Neo4j reports a non-numeric but
/// valid value token (`foo`, `'x'`, `true`) as a run-time type mismatch
/// (`Neo.ClientError.Statement.TypeError`), while a malformed numeric token
/// (`6 005`) is a compile-time input error.
fn coordinate_text_is_malformed(text: &str) -> bool {
    match relate_model::VectorCoordKind::coordinate_tokens(text) {
        None => true,
        Some(tokens) => tokens
            .iter()
            .any(|token| matches!(token, VectorCoordToken::Malformed)),
    }
}

/// One operand of `vector.similarity.*`: a vector, a list of numbers, or
/// null. A literal list element of any other static type makes the list a
/// compile-time type error.
fn require_vector_operand(arg: &Expr, what: &str) -> Result<(), EngineError> {
    // A no-op for a non-list; a list literal passes the type gate below.
    require_numeric_list_elements(arg, what)?;
    require_static_type(arg, LIST_OR_NULL, |ty| {
        format!(
            "Type mismatch: expected Vector, List<Float>, List<Integer> or \
                 List<Number> but was {}",
            ty.name()
        )
    })
}

/// Reject a static list literal with an element of a static non-numeric,
/// non-null type (BOOLEAN, STRING, a nested LIST, MAP, …). Unknown element
/// types (a variable, a property read) stay with the runtime.
fn require_numeric_list_elements(expr: &Expr, what: &str) -> Result<(), EngineError> {
    if let Expr::List(items) = unparen(expr) {
        for item in items {
            if let Some(ty) = static_literal_type(item) {
                if non_numeric_leaf_class(ty) {
                    return Err(EngineError::semantic(
                        ErrorKind::InvalidArgumentType,
                        format!("{what} needs numeric lists, got {}", ty.name()),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn check_string_argument(args: &[Expr], what: &str) -> Result<(), EngineError> {
    let Some(arg) = args.first() else {
        return Ok(());
    };
    require_static_type(arg, STRING_OR_NULL, |ty| {
        format!("{what} needs a string but was {}", ty.name())
    })
}

/// CIP-215's `replace(source, search, replace [, limit])`: a statically
/// negative limit is a compile-time value rejection. A limit of a *wrong
/// type* stays the runtime `TypeError`: the corpus pins that class for
/// `replace(…, '3')` / `replace(…, 3.0)` / `replace(…, true)` even as
/// literals, so this site deliberately does not share the literal
/// integer gate the vector dimension and the page bounds use.
fn check_replace_limit(args: &[Expr]) -> Result<(), EngineError> {
    let Some(limit) = args.get(3) else {
        return Ok(());
    };
    if static_int_value(limit).is_some_and(|value| value < 0) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidArgumentValue,
            "replace limit must be non-negative".to_string(),
        ));
    }
    Ok(())
}

/// A temporal constructor: the CIP-228 `(value, pattern)` form's pattern must
/// be a STRING. (A literal that cannot construct is the planner's rejection:
/// its class depends on the construction error, which the front-end boundary
/// would flatten to SyntaxError.)
fn check_temporal_call(args: &[Expr]) -> Result<(), EngineError> {
    // More than two arguments is the dispatcher's arity rejection, and the
    // zero-argument clock read has no pattern: neither is this rule's
    // business.
    if args.len() > 2 {
        return Ok(());
    }
    let Some(pattern) = args.get(1) else {
        return Ok(());
    };
    require_static_type(pattern, STRING_OR_NULL, |ty| {
        format!("Type mismatch: expected String but was {}", ty.name())
    })
}

/// `allReduce(acc = <init>, x IN <list> | <expr>, <predicate>)`: a static
/// non-list source or statically non-boolean predicate is a compile-time type
/// error. The predicate may be the accumulator itself, whose static type is
/// the init expression.
fn check_all_reduce(r: &relate_parser::AllReduceExpr) -> Result<(), EngineError> {
    require_list_operand(&r.list_expr, "allReduce()")?;
    // A predicate that is the accumulator itself has the init expression's
    // static type: resolve the effective predicate once.
    let predicate_is_accumulator = expr_is_bare_var(&r.predicate, &r.accumulator);
    let effective_predicate = if predicate_is_accumulator {
        &r.init_expr
    } else {
        &r.predicate
    };
    if is_statically_non_boolean(effective_predicate) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidArgumentType,
            "allReduce() predicate must be boolean".to_string(),
        ));
    }
    Ok(())
}

fn require_list_operand(expr: &Expr, what: &str) -> Result<(), EngineError> {
    require_static_type(expr, LIST_OR_NULL, |ty| {
        format!("{what} needs a list, got {}", ty.name())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Statically-typed builtin-call arguments: compile-time rejections ----
    //
    // Neo4j rejects a *literal* argument of the wrong type (or outside its
    // value domain) during semantic analysis; the front end owns that
    // decision, and the runtime evaluator keeps its backstop for values whose
    // type is only known per row. These tests pin the compile-time half.

    fn parse_and_check_static_calls(query: &str) -> Result<(), EngineError> {
        let stmt = relate_parser::parse(query).expect("parse");
        validate_static_call_types(&stmt.clauses)
    }

    fn assert_rejected(queries: &[&str]) {
        for query in queries {
            assert!(
                parse_and_check_static_calls(query).is_err(),
                "`{query}` must be rejected at compile time"
            );
        }
    }

    fn assert_accepted(queries: &[&str]) {
        for query in queries {
            assert!(
                parse_and_check_static_calls(query).is_ok(),
                "`{query}` must not be rejected by the static pass"
            );
        }
    }

    #[test]
    fn vector_constructor_static_rejections() {
        assert_rejected(&[
            // Unsupported / null coordinate type.
            "RETURN vector([1, 2, 3], 3, STRING) AS r",
            "RETURN vector([1, 2, 3], 3, ANY) AS r",
            "RETURN vector([1, 2, 3], 3, null) AS r",
            // Dimension outside 1..=4096.
            "RETURN vector([1, 2, 3], 0, INTEGER) AS r",
            "RETURN vector([1, 2, 3], -5, INTEGER) AS r",
            "RETURN vector([1, 2, 3], 8192, INTEGER) AS r",
            // A statically non-integer dimension is the same literal gate.
            "RETURN vector([1, 2, 3], 'three', INTEGER) AS r",
            "RETURN vector([1, 2, 3], 3.5, INTEGER) AS r",
            // Non-numeric coordinate literal.
            "RETURN vector([true], 1, INTEGER) AS r",
            // Malformed string-coordinate literal.
            "RETURN vector(\"1, 2, 3\", 3, INTEGER) AS r",
            "RETURN vector(\"[1, 2, 3\", 3, INTEGER) AS r",
            "RETURN vector(\"[6 005, 5]\", 2, FLOAT32) AS r",
            // One argument is an argument-count error.
            "RETURN vector([1, 2, 3]) AS r",
        ]);
        assert_accepted(&[
            "RETURN vector([1, 2, 3], 3, INTEGER) AS r",
            "RETURN vector(\"[1, 2, 3]\", 3, INTEGER) AS r",
            // A non-numeric but valid value token is live's run-time type
            // mismatch, not a compile-time input error.
            "RETURN vector(\"[foo, bar]\", 2, INTEGER) AS r",
            // A caller's own parameter keeps the runtime decision.
            "RETURN vector($p, 3, INTEGER) AS r",
        ]);
    }

    #[test]
    fn vector_similarity_static_argument_types() {
        assert_rejected(&[
            "RETURN vector.similarity.cosine(42, [1, 0]) AS r",
            "RETURN vector.similarity.euclidean([1, 0], 'not a vector') AS r",
            "RETURN vector.similarity.cosine([true, false], [1, 0]) AS r",
            "RETURN vector.similarity.cosine(['a', 'b'], [1, 0]) AS r",
            "RETURN vector.similarity.cosine([[1, 2], [3, 4]], [1, 0]) AS r",
        ]);
        assert_accepted(&[
            "RETURN vector.similarity.cosine([1, 0], [1, 0]) AS r",
            // A null element is a value-level (runtime) fault, not a static
            // type fault.
            "RETURN vector.similarity.cosine([1, null], [1, 0]) AS r",
            "RETURN vector.similarity.cosine($p, [1, 0]) AS r",
        ]);
    }

    #[test]
    fn string_and_arity_static_rejections() {
        assert_rejected(&[
            "RETURN char_length([1, 2, 3])",
            "RETURN character_length(42)",
            "RETURN NULLIF(1) AS x",
            "RETURN NULLIF(1, 2, 3) AS x",
            "RETURN format(date('2024-06-27'), 'yyyy', 'extra') AS x",
            "RETURN replace('woof', 'woof', 'meow', -1) AS x",
        ]);
        assert_accepted(&[
            "RETURN char_length('abc')",
            "RETURN char_length($p)",
            "RETURN NULLIF(1, 2) AS x",
            "RETURN format(date('2024-06-27'), 'yyyy') AS x",
            "RETURN replace('woof', 'woof', 'meow', 1) AS x",
            // A non-integer limit keeps the runtime `TypeError` the corpus
            // pins; the literal integer gate does not apply at this site.
            "RETURN replace('woof', 'woof', 'meow', 'one') AS x",
        ]);
    }

    /// The shared contract carries a bare detail; the typed channel adds the
    /// `InvalidNumberOfArguments` token exactly once. Both alias spellings
    /// render the one text.
    #[test]
    fn char_length_arity_rejection_renders_one_class_token() {
        for query in [
            "RETURN char_length() AS x",
            "RETURN character_length('a', 'b') AS x",
        ] {
            let err = parse_and_check_static_calls(query).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::InvalidNumberOfArguments, "{query}");
            assert_eq!(
                err.to_string(),
                "InvalidNumberOfArguments: function requires one argument",
                "{query}"
            );
        }
    }

    /// The rejection names the spelling the call was made with.
    #[test]
    fn string_argument_rejection_names_the_called_spelling() {
        for (query, spelling) in [
            ("RETURN char_length(42)", "char_length()"),
            ("RETURN character_length(42)", "character_length()"),
        ] {
            let err = parse_and_check_static_calls(query).unwrap_err();
            assert!(err.to_string().contains(spelling), "{query}: {err}");
        }
    }

    #[test]
    fn temporal_pattern_static_rejections() {
        // A literal that cannot *construct* is the planner's rejection (its
        // class follows the construction error);
        // `relate::tests::compile_time_error_classes` covers the end-to-end
        // `date('2022-1')` class.
        assert_rejected(&[
            "RETURN date('2020-01-01', 42) AS x",
            "RETURN duration('5 seconds', 42) AS x",
        ]);
        assert_accepted(&[
            "RETURN date('2020-01-01') AS x",
            "RETURN datetime('2020-01-01T00:00:00Z') AS x",
            // A caller's own parameter keeps the runtime decision.
            "RETURN date($p) AS x",
        ]);
    }

    #[test]
    fn quantifier_and_allreduce_static_operand_types() {
        assert_rejected(&[
            "RETURN any(x IN 5 WHERE x > 0) AS result",
            "RETURN allReduce(acc = 0, x IN 5 | acc + x, acc < 5) AS result",
            // The predicate is the accumulator itself, whose static type is
            // the integer init expression.
            "RETURN allReduce(acc = 0, x IN [1, 2, 3] | acc + x, acc) AS result",
        ]);
        assert_accepted(&[
            "RETURN any(x IN [1, 2] WHERE x > 0) AS result",
            "RETURN allReduce(acc = 0, x IN [1, 2] | acc + x, acc < 5) AS result",
            "RETURN any(x IN $l WHERE x > 0) AS result",
        ]);
    }

    #[test]
    fn in_transactions_concurrency_is_statically_typed() {
        assert_rejected(&["CALL { CREATE (n) } IN null CONCURRENT TRANSACTIONS"]);
        assert_accepted(&["CALL { CREATE (n) } IN 2 CONCURRENT TRANSACTIONS"]);
    }

    // ---- CIP-271: toString() rejects a homogeneous LIST<T> ----

    #[test]
    fn tostring_rejects_a_homogeneous_list_literal_at_compile_time() {
        // The four shapes Neo4j's checker rejects: homogeneous INTEGER,
        // STRING and nested-LIST literals, plus the mixed number list
        // (INTEGER and FLOAT are one class). The error is the
        // InvalidArgumentValue detail CIP-271 pins.
        for query in [
            "RETURN toString([1, 2]) AS result",
            "RETURN toString(['a', 'b']) AS result",
            "RETURN toString([1, 2.0]) AS result",
            "RETURN toString([[1]]) AS result",
            "RETURN toString([1 + 1, 2]) AS result",
            "CYPHER 25 RETURN toString([1, 2]) AS result",
        ] {
            let err = parse_and_check_static_calls(query)
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
    fn tostring_accepts_a_dynamic_or_mixed_list_and_the_ornull_spelling() {
        // `LIST<ANY>` (or a non-list) falls through to the runtime, which
        // applies the same homogeneous-list rule to the value it sees.
        // `toStringOrNull()` is never rejected here: it maps that rule's
        // type error to null at the value level and must not raise.
        assert_accepted(&[
            "RETURN toString([]) AS result",
            "RETURN toString([1, 'a']) AS result",
            "RETURN toString([1, null, 'a']) AS result",
            "RETURN toString([true, null]) AS result",
            "RETURN toString($p) AS result",
            "UNWIND [[1, 2]] AS xs RETURN toString(xs) AS result",
            "MATCH (n) RETURN toString(n.ids) AS result",
            "RETURN toString(1) AS result",
            "RETURN toString({a: 1}) AS result",
            "RETURN toStringOrNull([1, 2]) AS result",
            "RETURN toStringOrNull(['a', 'b']) AS result",
        ]);
    }
}
