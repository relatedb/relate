//! The executor memo budget as an open-time setting (ADR-0657).
//! Run: cargo run -p relate --example memo_budget

use relate::{Database, OpenOptions};

fn main() -> relate::Result<()> {
    // The same statement at three budgets (whole MiB; 0 disables the
    // budgeted memo tables): speed and memory differ, the rows never do.
    let query = "UNWIND range(1, 2000) AS i \
                 RETURN count(DISTINCT datetime('2024-01-01T00:00:' + \
                 substring(toString(100 + i % 60), 1) + 'Z')) AS distinct_instants";
    let mut answers = Vec::new();
    for budget in [0, 1, 16] {
        let db = Database::open_with(
            ":memory:",
            OpenOptions {
                expression_memo_budget: Some(budget),
                ..Default::default()
            },
        )?;
        answers.push(db.connect()?.query(query)?.rows);
    }
    assert!(answers.windows(2).all(|pair| pair[0] == pair[1]));

    // A budget too large to express in bytes fails the open, naming the
    // setting; there is no silent fallback to the default.
    let refused = Database::open_with(
        ":memory:",
        OpenOptions {
            expression_memo_budget: Some(u64::MAX),
            ..Default::default()
        },
    );
    let message = refused
        .err()
        .expect("an unusable budget fails open")
        .to_string();
    assert!(message.contains("expression_memo_budget"), "{message}");
    Ok(())
}
