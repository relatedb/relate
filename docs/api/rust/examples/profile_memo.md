# PROFILE memo-counters example

Run with `cargo run -p relate --example profile_memo`.

A memoised expression (ADR-0640) answers repeated inputs from a table
instead of recomputing them. `PROFILE` (ADR-0654) reports what each memo
did: `ClauseMetrics::memo` is a `MemoCounters { probes, hits, inserts,
refused, bytes, disabled_count }` summed over the memos an operator's stage
ran, with `bytes` the tables' allocation (its own figure, never part of
`memory_bytes`). The temporal constructors' memos serve any expression in
the statement, so they report on the plan **root**, where
`ClauseMetrics::memo_kinds` names the memo types (`node-label`,
`datetime`).

The example profiles a string-fed `datetime(...)` over two distinct texts:
nearly every probe hits, two answers are stored, and the root carries the
group. On the machine-native wire form the group is presence-only — the
`Memo*` integers and the root's `MemoKinds` appear only because a memo was
probed — and a statement that used no memo (`PROFILE RETURN 1`) carries
none. The same group appears as `actual.memo` in the `relate.plan/v1`
document (C ABI, Python, MCP, CLI JSON), in the HTTP `profiledQueryPlan`
and Bolt `profile` args, and as the CLI table's `Memo` column
(`hits/probes`, `*` when a memo gave up).
