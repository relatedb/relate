//! The open-time memo budget (ADR-0657): `OpenOptions::expression_memo_budget`
//! and `RELATE_EXPRESSION_MEMO_BUDGET`. The budget changes speed and memory,
//! never results, so the differential runs memo-exercising statements at
//! budgets 0, 1, 8 and 32 MiB and asserts identical rows; a bad value fails
//! the open naming the setting.

mod common;

use relate::{Database, OpenOptions, Value};
use relate_test_support::EnvRestore;

use common::{err_text, rows};

fn open_at(budget: Option<u64>) -> relate::Connection {
    Database::open_with(
        ":memory:",
        OpenOptions {
            expression_memo_budget: budget,
            ..Default::default()
        },
    )
    .unwrap()
    .connect()
    .unwrap()
}

/// A ring of labelled nodes with chords: a guarded variable-length
/// expansion that routes through the node-label memo.
const SEED: &str = "UNWIND range(0, 3999) AS i \
     CREATE (:N {id: i, hot: i % 3 = 0}) \
     WITH count(*) AS made \
     UNWIND range(0, 3999) AS i \
     MATCH (a:N {id: i}), (b:N {id: (i + 1) % 4000}), (c:N {id: (i * 7 + 3) % 4000}) \
     CREATE (a)-[:R]->(b), (a)-[:R]->(c)";

/// Statements whose memoised sites repeat inputs past each warm-up: the
/// distinct-datetime constructor (a 300-text domain over 6000 rows, so the
/// construct memo hits), a clock-pinned constructor, regexes and an
/// `IN`-list, and the guarded expansion.
const STATEMENTS: [&str; 5] = [
    "UNWIND range(1, 6000) AS i \
     WITH datetime('2024-01-01T00:' + substring(toString(100 + (i % 300) / 60), 1) \
        + ':' + substring(toString(100 + (i % 300) % 60), 1) + 'Z') AS d \
     RETURN d.year AS y, d.minute AS m, d.second AS s, count(*) AS c ORDER BY m, s",
    "UNWIND range(1, 3000) AS i RETURN count(DISTINCT datetime('2024-05-01T10:00:00Z') + duration({days: i % 9}))",
    "UNWIND range(1, 3000) AS i WITH 'user' + toString(i % 50) AS u \
     WHERE u =~ 'user[0-9]' OR u IN ['user12', 'user40', 'user49'] \
     RETURN u, count(*) ORDER BY u",
    "MATCH (a:N {id: 0})-[:R*1..4]->(b:N) WHERE b.hot RETURN count(DISTINCT b) AS c",
    "MATCH (a:N) WHERE a.id < 200 MATCH (a)-[:R]->(b:N)-[:R]->(c:N) \
     RETURN count(DISTINCT c) AS c, sum(c.id) AS s",
];

#[test]
fn identical_rows_at_budgets_0_1_8_32() {
    let budgets = [0u64, 1, 8, 32];
    let mut baseline: Option<Vec<Vec<Vec<Value>>>> = None;
    for budget in budgets {
        let mut conn = open_at(Some(budget));
        conn.query(SEED).unwrap();
        let got: Vec<_> = STATEMENTS.iter().map(|q| rows(&mut conn, q)).collect();
        assert!(
            got.iter().all(|r| !r.is_empty()),
            "every statement answers rows at budget {budget}"
        );
        match &baseline {
            None => baseline = Some(got),
            Some(first) => assert_eq!(first, &got, "budget {budget} differs from budget 0"),
        }
    }
}

/// `rand()` and `randomUUID()` are never memoised (ADR-0657 §5): every
/// call is a fresh value at every budget, including a repeated argument.
#[test]
fn volatile_functions_are_never_memoised() {
    for budget in [0u64, 1, 32] {
        let mut conn = open_at(Some(budget));
        let q = "UNWIND range(1, 2000) AS i \
                 WITH randomUUID() AS u, rand() AS r \
                 RETURN count(DISTINCT u) AS u, count(DISTINCT r) AS r";
        let row = rows(&mut conn, q).remove(0);
        assert_eq!(row, vec![Value::Int(2000), Value::Int(2000)]);
    }
}

#[test]
fn bad_option_fails_the_open_naming_the_setting() {
    let err = Database::open_with(
        ":memory:",
        OpenOptions {
            expression_memo_budget: Some(u64::MAX),
            ..Default::default()
        },
    )
    .err()
    .expect("a budget past the byte range fails the open");
    assert!(err.to_string().contains("expression_memo_budget"), "{err}");
}

/// The environment variable resolves like its siblings: read once at open,
/// `0` is valid, a bad value fails the open naming the variable, and the
/// explicit option wins over it.
#[test]
fn environment_variable_resolves_and_fails_loud() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _restore = EnvRestore::new("RELATE_EXPRESSION_MEMO_BUDGET");

    std::env::set_var("RELATE_EXPRESSION_MEMO_BUDGET", "0");
    let mut conn = open_at(None);
    assert_eq!(
        rows(&mut conn, "RETURN datetime('2024-01-01T00:00:00Z').year"),
        vec![vec![Value::Int(2024)]]
    );

    for bad in ["-1", "8mb", "lots"] {
        std::env::set_var("RELATE_EXPRESSION_MEMO_BUDGET", bad);
        let err = err_text(Database::open(":memory:"));
        assert!(
            err.contains("RELATE_EXPRESSION_MEMO_BUDGET"),
            "{bad}: {err}"
        );
        // The explicit option never reads the variable.
        assert!(Database::open_with(
            ":memory:",
            OpenOptions {
                expression_memo_budget: Some(4),
                ..Default::default()
            }
        )
        .is_ok());
    }
}
