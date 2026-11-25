### Changed

- Bolt serves an auto-commit result as a stream: `PULL {n}` is answered from
  bounded batches the statement produces on demand, so a driver paging a
  multi-million-row result no longer makes the server buffer it whole, and
  `DISCARD` finishes the statement without encoding the rest. A statement
  whose execution fails after rows were delivered now reports the FAILURE at
  the `PULL` that reached it. A client that stops pulling has its statement
  stopped after the library-level `ConnLimits::pull_idle_timeout` (default:
  the idle read timeout); the next `PULL` answers
  `Neo.ClientError.Transaction.TransactionTimedOut`. `--max-result-bytes`
  now bounds each streamed batch rather than the whole result, which is no
  longer held. Statements inside an explicit transaction still buffer.
