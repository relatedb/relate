# Executor memo budget (ADR-0657)

Run `cargo run -p relate --example memo_budget --locked`. The
[compiled example](../../../../relate/relate/examples/memo_budget.rs) runs one
distinct-datetime statement at budgets 0, 1 and 16 MiB through
`OpenOptions::expression_memo_budget` and asserts the rows are identical, then
checks that a budget too large to express in bytes fails the open naming
`expression_memo_budget`. The environment form, `RELATE_EXPRESSION_MEMO_BUDGET=8`,
reads the same whole-MiB value when the option is `None`; the integration test
`relate/relate/tests/memo_budget.rs` pins the differential across budgets and the
environment resolution.
