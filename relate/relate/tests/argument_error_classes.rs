//! The argument-value and procedure rejections carry the error *class* live
//! Neo4j 2026.10.1 reports for the same statement. The decision and the
//! probe table live in ADR-0565; this file only pins the classes per site.

mod common;

use common::{connection, error_class, error_class_literal, params, run};
use relate::Value;

/// The vector argument family: an empty or zero vector, a null or NaN
/// coordinate, a dimension mismatch and an out-of-bound `vector()`
/// dimension are the `ArgumentError` class (22N38); a wrong-typed
/// argument keeps the `TypeError` class.
#[test]
fn vector_argument_rejections_are_argument_errors() {
    let mut conn = connection();
    for statement in [
        // empty list
        "RETURN vector.similarity.cosine([], []) AS r",
        "RETURN vector.similarity.euclidean([], []) AS r",
        // zero vector (cosine only)
        "RETURN vector.similarity.cosine([0.0, 0.0], [0.5, 0.5]) AS r",
        // null coordinate
        "RETURN vector.similarity.cosine([0.5, 0.5], [0.5, null]) AS r",
        "RETURN vector.similarity.euclidean([0.5, 0.5], [0.5, null]) AS r",
        // dimension mismatch
        "RETURN vector.similarity.cosine([1.0, 2.0], [1.0, 2.0, 3.0]) AS r",
        "RETURN vector.similarity.euclidean([1.0, 2.0], [1.0, 2.0, 3.0]) AS r",
        "RETURN vector_distance(vector([1, 2, 3], 3, FLOAT32), vector([1, 2], 2, FLOAT32), \
         EUCLIDEAN) AS d",
    ] {
        assert_eq!(
            error_class_literal(&mut conn, statement),
            "ArgumentError",
            "{statement}"
        );
    }
    // A NaN coordinate rides a parameter (a NaN literal is not Cypher).
    assert_eq!(
        error_class(
            &mut conn,
            "RETURN vector.similarity.cosine([1.0, 2.0], [1.0, $nan]) AS r",
            params(&[("nan", Value::Float(f64::NAN))])
        ),
        "ArgumentError"
    );
    // The `vector()` dimension bound (1 to 4096) is a value fault; an
    // in-bound dimension that disagrees with the coordinate count is not.
    for dim in [-1, 0, 4097] {
        assert_eq!(
            error_class(
                &mut conn,
                "RETURN vector([1.0, 2.0, 3.0, 4.0], $dim, FLOAT32) AS v",
                params(&[("dim", Value::Int(dim))])
            ),
            "ArgumentError",
            "dimension {dim}"
        );
    }
    assert_eq!(
        error_class(
            &mut conn,
            "RETURN vector([1.0, 2.0, 3.0], $dim, FLOAT32) AS v",
            params(&[("dim", Value::Int(4))])
        ),
        "TypeError"
    );
}

/// A wrong-typed similarity argument at run time stays the `TypeError`
/// class (a node's string property is the corpus's shape).
#[test]
fn wrong_typed_similarity_argument_is_a_type_error() {
    let mut conn = connection();
    run(
        &mut conn,
        "CREATE (:X {name: 'valid', vec: [1, 1]}), (:X {name: 'string', vec: 'nope'})",
    );
    assert_eq!(
        error_class_literal(
            &mut conn,
            "MATCH (n:X) WHERE vector.similarity.cosine([0.5, 0.5], n.vec) < 0.5 RETURN n"
        ),
        "TypeError"
    );
}

/// `trim()`'s single-character rule, `round()`'s precision/mode checks, the
/// `replace()` limit and a zero `CALL … IN TRANSACTIONS` batch size are the
/// `ArgumentError` class.
#[test]
fn function_argument_rejections_are_argument_errors() {
    let mut conn = connection();
    for statement in [
        "RETURN trim(BOTH 'xyx' FROM 'HELLO') AS r",
        "RETURN trim(BOTH '' FROM 'HELLO') AS r",
        "RETURN trim(BOTH '✂️' FROM 'HELLO') AS r",
        "RETURN round(1.49, -1) AS r",
        "RETURN round(1.49, 2147483648) AS r",
        "RETURN round(1.0/0.0, 1, 'BOGUS') AS r",
        "RETURN round(1.49, 1, 'MY_OWN_MODE') AS r",
    ] {
        assert_eq!(
            error_class_literal(&mut conn, statement),
            "ArgumentError",
            "{statement}"
        );
    }
    assert_eq!(
        error_class(
            &mut conn,
            "RETURN replace('hi:hi', ':', ',', $limit) AS r",
            params(&[("limit", Value::Int(-1))])
        ),
        "ArgumentError"
    );
    assert_eq!(
        error_class(
            &mut conn,
            "UNWIND [1, 2] AS x CALL { WITH x RETURN x AS y } IN TRANSACTIONS OF $batchSize \
             ROWS RETURN y",
            params(&[("batchSize", Value::Int(0))])
        ),
        "ArgumentError"
    );
    // A non-integer batch size is the TypeError class, not this one.
    assert_eq!(
        error_class(
            &mut conn,
            "UNWIND [1, 2] AS x CALL { WITH x RETURN x AS y } IN TRANSACTIONS OF $batchSize \
             ROWS RETURN y",
            params(&[("batchSize", Value::String("two".into()))])
        ),
        "TypeError"
    );
}

/// `toInteger()`'s out-of-range string is live's CypherTypeException —
/// 22003 under the `TypeError` class — unlike the integer-argument range
/// sites.
#[test]
fn to_integer_out_of_range_is_a_type_error() {
    let mut conn = connection();
    assert_eq!(
        error_class_literal(&mut conn, "RETURN toInteger('10508455564958384115') AS r"),
        "TypeError"
    );
}

/// A call to an unregistered procedure is the `ProcedureError` class.
#[test]
fn unknown_procedure_is_a_procedure_error() {
    let mut conn = connection();
    assert_eq!(
        error_class_literal(
            &mut conn,
            "CALL db.index.vector.createNodeIndex('x', 'Y', 'z', 10, 'cosine')"
        ),
        "ProcedureError"
    );
}
