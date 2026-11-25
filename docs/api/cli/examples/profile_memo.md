# Example: PROFILE's `Memo` column

A statement that uses an expression memo (ADR-0640) gets a `Memo` column in
the `PROFILE` plan table (ADR-0654): `hits/probes`, with a `*` suffix when a
memo gave up at its hit-rate review. A string-fed `datetime(...)` over two
distinct texts is answered from the constructor memo after its warm-up, and
the memo's counts appear on the root row:

```bash
cargo run -p relate-cli -- --database :memory: --mode table \
  "PROFILE UNWIND range(0, 1999) AS i WITH ['2024-05-05T10:00:00Z', '2023-01-01T09:00:00Z'][i % 2] AS s RETURN count(datetime(s)) AS n"
```

The data table (`2000`) prints on stdout; the plan table prints on stderr,
and its root row ends with the `Memo` cell, for example `1990/1992` (the
operators that ran no memo leave the cell empty):

```
| Operator           | Id | Details                | Rows | Time (ms) | Total (ms) | Db Hits | Memory (Bytes) | Expression Built (Bytes) | Memo      |
| + EagerAggregation | 0  | 1 item(s), aggregation | 1    | 0.742     | 2.695      | 0       | 25             | 0                        | 1990/1992 |
```

A statement that used no memo (`PROFILE RETURN 1 AS n`) has no `Memo` column.
The machine modes carry the same group as `MemoProbes`, `MemoHits`,
`MemoInserts`, `MemoRefused`, `MemoBytes` and `MemoDisabledCount` on the
operator, plus the root's `MemoKinds` (`["datetime"]` here):

```bash
cargo run -p relate-cli -- --database :memory: --mode json \
  "PROFILE UNWIND range(0, 1999) AS i WITH ['2024-05-05T10:00:00Z', '2023-01-01T09:00:00Z'][i % 2] AS s RETURN count(datetime(s)) AS n"
```

Pinned by `surfaces/relate-cli/tests/integration/e2e_profile_memo.rs`.
