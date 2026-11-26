### Added

- Statements now run under a wall-clock budget, the statement timeout
  (ADR-0692). The network surfaces default to 30 seconds: a Bolt statement
  past it fails with `Neo.ClientError.Transaction.TransactionTimedOut`, an
  HTTP request answers 408, an MCP tool call returns the error text, and a
  streamed answer ends with the timeout as its in-band error. A request sets
  its own budget, longer or shorter — Bolt `tx_timeout` in `RUN`/`BEGIN`,
  HTTP `maxExecutionTime`, MCP `timeout_seconds` — and `relate serve` takes
  `--query-timeout-secs` (the default) and `--query-timeout-max-secs` (an
  operator ceiling over every request, `0` = none). The embedded library, the
  C ABI and the CLI apply no default: `Connection::set_statement_timeout`,
  `relate_conn_set_statement_timeout`, the CLI's `--timeout` and the new
  `.timeout [DURATION|off]` dot command, and a per-connection option in the
  Python (`Connection.statement_timeout`), JavaScript, Go, Java, Swift and .NET
  clients opt in. A timeout inside `CALL { … } IN TRANSACTIONS` rolls back the
  in-flight batch, keeps the committed ones, and is never absorbed by
  `ON ERROR CONTINUE`.

### Changed

- Bolt's `--query-timeout-secs` now defaults to 30 seconds instead of off.
  Statements that legitimately run longer set `tx_timeout` per request, or
  start the server with `--query-timeout-secs 0`.
