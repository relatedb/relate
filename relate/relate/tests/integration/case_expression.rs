//! Generalized simple-CASE `WHEN` operands (CIP-143/CIP-190): a leading
//! comparison operator, a comma-separated `<when operand list>`, an
//! `IS [NOT] [<normal form>] NORMALIZED` predicate (CIP-133) and a bare
//! `:: <value type>` predicate — the forms the acceptance CaseExpression
//! suite exercises. Also pins a pattern predicate as a list comprehension's
//! WHERE filter, which a projection allows only in that position.

use crate::common;

use relate::Value;

use common::{connection, rows};

#[test]
fn simple_case_comparison_when_operand_compares_the_case_operand() {
    // `CASE x WHEN < 1 THEN 1 ELSE 2.0 END` is `CASE WHEN x < 1 ...`.
    assert_eq!(
        rows(
            &mut connection(),
            "UNWIND [0.5, 1.5] AS x WITH (CASE x WHEN < 1 THEN 1 ELSE 2.0 END) AS x \
              RETURN x, x + 1 ORDER BY x"
        ),
        vec![
            vec![Value::Int(1), Value::Int(2)],
            vec![Value::Float(2.0), Value::Float(3.0)],
        ]
    );
}

#[test]
fn simple_case_comparison_when_operand_supports_all_operators() {
    assert_eq!(
        rows(
            &mut connection(),
            "UNWIND [0, 1, 2, 3] AS x \
              RETURN CASE x WHEN <= 0 THEN 'a' WHEN = 1 THEN 'b' WHEN >= 3 THEN 'd' \
              WHEN <> 2 THEN 'c' ELSE 'e' END AS r ORDER BY x"
        ),
        vec![
            vec![Value::String("a".into())],
            vec![Value::String("b".into())],
            vec![Value::String("e".into())],
            vec![Value::String("d".into())],
        ]
    );
}

#[test]
fn simple_case_when_operand_list_is_an_or_of_equalities() {
    assert_eq!(
        rows(
            &mut connection(),
            "UNWIND [1000, 1500, 2000, 2500, 3000, 3500] AS s \
              RETURN CASE s WHEN 1000, 1500 THEN 'low' WHEN 2000, 2500 THEN 'med' \
              WHEN 3000, 3500 THEN 'high' END AS res ORDER BY res"
        ),
        vec![
            vec![Value::String("high".into())],
            vec![Value::String("high".into())],
            vec![Value::String("low".into())],
            vec![Value::String("low".into())],
            vec![Value::String("med".into())],
            vec![Value::String("med".into())],
        ]
    );
}

#[test]
fn simple_case_when_operand_list_mixes_comparison_and_is_null() {
    assert_eq!(
        rows(
            &mut connection(),
            "UNWIND [1000, 2500, 3000] AS s \
              RETURN CASE s WHEN < 1500, IS NULL THEN 'low' WHEN <= 2500 THEN 'med' \
              ELSE 'high' END AS res ORDER BY res"
        ),
        vec![
            vec![Value::String("high".into())],
            vec![Value::String("low".into())],
            vec![Value::String("med".into())],
        ]
    );
}

#[test]
fn simple_case_normalized_when_operand() {
    assert_eq!(
        rows(
            &mut connection(),
            "RETURN CASE 'abc' WHEN IS NFC NORMALIZED THEN 'nfc' ELSE 'other' END AS r"
        ),
        vec![vec![Value::String("nfc".into())]]
    );
    // "é" as e + U+0301 is NFD, not NFC.
    let decomposed = "e\u{301}";
    assert_eq!(
        rows(
            &mut connection(),
            &format!(
            "RETURN CASE '{decomposed}' WHEN IS NOT NFC NORMALIZED THEN 'not' ELSE 'other' END AS r"
        )
        ),
        vec![vec![Value::String("not".into())]]
    );
}

#[test]
fn simple_case_bare_type_predicate_when_operand() {
    assert_eq!(
        rows(
            &mut connection(),
            "RETURN CASE 1 WHEN :: INTEGER THEN 'int' ELSE 'other' END AS r"
        ),
        vec![vec![Value::String("int".into())]]
    );
}

/// A nondeterministic simple-CASE operand stays well-defined under the
/// parse-time lowering of every arm to `operand <op> value` comparisons:
/// `rand()` is in `[0, 1)`, so `< 1.0` holds on every evaluation and the
/// first arm must win on every row.
#[test]
fn simple_case_nondeterministic_operand_keeps_first_true_arm() {
    let got = rows(
        &mut connection(),
        "UNWIND range(1, 25) AS i RETURN CASE rand() \
         WHEN < 1.0 THEN 'first' WHEN < 2.0 THEN 'second' ELSE 'else' END AS r",
    );
    assert_eq!(got.len(), 25);
    assert!(got
        .iter()
        .all(|row| row == &vec![Value::String("first".into())]));
}

#[test]
fn pattern_predicate_is_a_legal_comprehension_where_filter_in_a_projection() {
    let mut conn = connection();
    conn.query("CREATE (:test)<-[:rel]-(:test_rel)").unwrap();
    let result = conn
        .query(
            "MATCH (t:test) WITH COLLECT(t) AS ts \
             WITH CASE 1 WHEN 0 THEN [] ELSE [t IN ts WHERE (t)<--()] END AS res \
             RETURN COUNT(res) AS count",
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}
