# ADR-0609: Execute the full-text analyzer catalogue from the one analyzer table

Status: Proposed

Claim: 743b149fea8a3335874cf3b6fb46ff3b4fd43f78

## Context

`db.index.fulltext.listAvailableAnalyzers` was a `SHOW PROCEDURES`-only
registry row: listed, with its signature, but not callable. The row's
rationale was that no scenario calls it — a classification the Cypher
compatibility corpus has since contradicted: scenarios call it on the user
and system databases, alone and composed with `SHOW` commands, and two pin
the `swedish` row's description verbatim.

The engine already carries the full-text analyzer vocabulary in one table:
the names live Neo4j admits, each mapped to the behaviour this engine
implements. The catalogue procedure therefore has a source of truth to
answer from, and leaving it unexecutable only masks the composition rules
those scenarios exist to test.

## Decision

- The procedure executes and reports one row per name in the analyzer
  table, in the table's order, with its declared three columns.
- `description` is the catalogue text live Neo4j reports for the analyzer,
  held as a column of that same table.
- `stopwords` reports the stop-word set this engine's analyzer applies: the
  english analyzer's Lucene default set, empty for every other name (they
  share the stop-free standard tokenizer). It is not a copy of a
  per-language Lucene list the engine does not apply.
- The name→behaviour→stop-word mapping lives in one accessor read by both
  the analyzer and the catalogue, so a row cannot state a policy the
  analyzer does not follow.

## Alternatives considered

- **Carry live's per-language stop-word lists verbatim.** Rejected: roughly
  80 KB of Lucene data for a column the engine's analyzers do not use, and
  it would claim filtering the engine does not perform.
- **Add a second, name-keyed description table.** Rejected: a parallel
  vocabulary that can drift from the analyzer table the same names come
  from.
- **Keep the row catalog-only.** Rejected: the procedure is a documented
  built-in the corpus calls; refusing it leaves every calling scenario
  stopping at `ProcedureNotFound` instead of at the rule it tests.

## Consequences

- Calling scenarios now reach the system-database composition rules they
  exercise. Rows that expect those rules to reject a query still fail where
  the engine does not implement the rule — a separate gap.
- The analyzer table grows by one description column and is the single
  place a new analyzer's name, behaviour, description and stop words land.
- `SHOW PROCEDURES` output is unchanged: the row was and stays listed,
  READ and system-database capable.
