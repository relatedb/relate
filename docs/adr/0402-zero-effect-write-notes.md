---
type: adr
area: cli
claim: 6feb207321bcc9a8ada401ea8b2db8d3e9aad2ea
name: 0402-zero-effect-write-notes
status: accepted
tags:
  - output
  - transactions
title: Zero-effect write notes in batch sessions
REFERENCES:
  - "[[adr/0079-anonymous-usage-metrics]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0164-exit-open-transaction]]"
  - "[[adr/0194-cli-error-envelope-and-exit-codes]]"
  - "[[adr/0196-row-cap-for-agent-runs]]"
  - "[[adr/0199-statement-progress-on-failure]]"
  - "[[adr/0200-query-command-and-database-flag]]"
  - "[[adr/0309-bolt-summary-metadata-neo4j-4-4]]"
  - "[[adr/0332-query-type-w-rw-by-returned-columns]]"
---

# ADR-0402: Zero-effect write notes in batch sessions

## Context

[LABS-313](https://linear.app/neo4j/issue/LABS-313/cli-silent-zero-effect-transactions-begincommit-blocks-with-an-empty)
was filed while scripting a gbuild store through the statement-session verb
`cypherlite query` ([ADR-0200](./0200-query-command-and-database-flag.md)). A `.begin` / `.commit` block whose statement is

```cypher
MATCH (m:NoSuchLabel) DELETE m CREATE (n:Label {id: 1})
```

applies nothing: the intermediate `MATCH` binds zero rows, the row pipeline
short-circuits the whole statement — correct Cypher semantics, the same in
Neo4j — and the `CREATE`'s uniqueness violation never fires. The CLI exits 0
and writes zero bytes to stdout and to stderr, which reads exactly like a
transaction that was rolled back after partially applying. It cost a real
debugging session on a reopen script that had silently done nothing.

Verified 2026-10-24 on HEAD `605e6334` (release build; a fresh store with the
`Label.id` uniqueness constraint in place):

- the block above as a one-shot `cypherlite query` run exits 0 with 0 bytes on
  stdout and 0 bytes on stderr; the same block from piped stdin is identical;
- the same violating `CREATE` as its own statement exits 6 with
  `semantic error: ConstraintValidationFailed: …`;
- a zero-row write outside a transaction (`MATCH (m:NoSuchLabel) DELETE m`)
  piped on stdin also exits 0 with both streams empty, and in `--mode json` it
  prints `[]`.

The engine already carries the fact that separates "wrote nothing" from "read
nothing": the plan-derived query type (`w` / `rw` for writes, `r` for reads,
[ADR-0332](./0332-query-type-w-rw-by-returned-columns.md)) and the per-statement mutation counters (`QueryResult::stats`,
[ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md)), both on every result. The CLI reads neither for reporting, so the
silence is a missing report, not missing information.

The first criterion drafted from those two facts — query type is `w`/`rw` and
every counter is zero — was not truthful, verified on this branch before merge.
It reported two kinds of statement that are not zero-effect writes:

- a **catalog change**, which persists while moving no graph mutation counter:
  `cypherlite query "ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 5"` on a
  fresh store printed `note: write statement changed nothing; the graph is
  unchanged`, and the same note followed `CREATE GRAPH TYPE gt AS { (n:Person
  => {name :: STRING}) }` and `CALL db.ontology.set(…)` / `CALL
  db.ontology.drop()` (with their preview opt-ins);
- a **read-only statement the planner conservatively classifies as a write**
 ([ADR-0332](./0332-query-type-w-rw-by-returned-columns.md)'s deliberate over-approximation, shared with the namespace write
  gate): `CALL { MATCH (n) RETURN n } RETURN n` and a bare `LOAD CSV … RETURN`
  printed the note while mutating nothing — and a transaction holding only such
  statements printed the commit summary too.

Neither fact can be repaired into a truthful answer from outside the engine.
The query type is conservative by design, and the counters count graph
entities, schema and properties — not the catalog, and not the attempt a
short-circuited write still makes. The missing fact is the engine's own: did a
mutation operator reach execution, and did the engine apply a change. That is
recorded where it is known, in the executor, and published on every result as
`QueryResult::effect` (a `StatementEffect`), named once at the statement
boundary (`finish_statement`) so no other site restates it.

Two CLI precedents bound what the new report may do. [ADR-0199](./0199-statement-progress-on-failure.md)'s batch progress
line is batch-only, prints nothing for a single statement or a successful run,
and counts dot-commands in its numbering; [ADR-0164](./0164-exit-open-transaction.md) established that a stderr
line can be an observable contract that wrapper scripts grep, so its wording is
byte-for-byte stable. The policy forks below were agreed with the maintainer on
2026-10-24.

## Decision

1. **Default-on, batch sessions only, stderr only.** A batch session — the
   one-shot `QUERY` and piped-stdin halves of the invocation-mode trichotomy
 ([ADR-0079](./0079-anonymous-usage-metrics.md) §4), the `is_batch` split [ADR-0199](./0199-statement-progress-on-failure.md) and [ADR-0164](./0164-exit-open-transaction.md) already read —
   reports a zero-effect write without being asked. There is no flag to enable
   or disable it: an opt-in misses the author who does not suspect the no-op,
   which is the whole audience, and a suppression flag is a separate decision.
   The line goes to stderr in every output mode, so stdout stays the
   machine-readable payload it is today.
2. **A zero-effect write is an executed write that applied nothing.** The
   criterion is the engine's effect record (`QueryResult::effect`, a
   `StatementEffect`): a mutation operator reached execution **and** the
   engine applied no change. Both facts come from execution, never from the
 plan's conservative query type ([ADR-0332](./0332-query-type-w-rw-by-returned-columns.md)), the statement's spelling, or
   matching clause or procedure names in the CLI:

   - *A mutation operator reached execution* means the executor dispatched a
     data-write clause (`CREATE`, `MERGE`, `DELETE`, `SET`, `REMOVE`,
     `FOREACH`), schema DDL, a catalog write (`ALTER DATABASE`, the graph-type
     commands) or a writes-flagged procedure (`db.ontology.set`/`drop`/`apply`,
     `db.createLabel`). It is recorded before rows flow, so a write whose
     pipeline is empty — the LABS-313 shape — still counts as an attempt. The
     mark is a walk over the statement's clauses, `NEXT` stages and nested
     bodies, so a write inside a `CALL { … }` body or an untaken `WHEN`
     branch is an attempt even when that body never runs.
   - *Applied a change* means the statement pushed at least one change into
     the write redo, the one funnel every entity, schema and **catalog** change
     goes through. A catalog change therefore counts as applied even though
 every [ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md) mutation counter stays zero (`ALTER DATABASE`, `CREATE
     GRAPH TYPE`, `db.ontology.set`/`drop`), and a read-only statement counts
     as no attempt at all however the planner classified it — a top-level
     `CALL { … }` with a read-only body and a bare `LOAD CSV … RETURN` are
     write-*routed* by the conservative access fold but run no mutation
     operator, so they print nothing. A change the engine rolled back is not
     applied: a failed statement restores its statement-spanning state, and a
     batch the statement rolls back (`IN TRANSACTIONS … ON ERROR CONTINUE`)
     un-counts the changes it truncated, so only changes that survived count.

   Rows are not the signal: a write without `RETURN` returns no rows whatever
   it wrote. The counters are not the signal either, for the two reasons above;
   the engine's redo is. A plan artifact takes only the **note** out of scope,
   not the effect: `EXPLAIN` does not execute and carries the zero effect,
   `StatementEffect::None`, while `PROFILE` executes the wrapped statement and
   carries its true effect — an applied `PROFILE` write counts in its
   transaction's merged effect (Decision 4) — but its plan table is the run's
   own report, so it adds no note. Silence after a successful statement
   therefore means one of exactly three things: it read only, it applied a
   change, or it was a plan-producing run. A failed statement reports its
   failure and carries no note — including the one failure that still applied
   something: a statement that executed and then failed to render reports the
   render failure (exit 1), takes no note, and keeps its real effect for the
   transaction ledger (Decision 4), so the failure is never reported as a
   no-op.

   The same per-clause answer is published once — `clause_mutates_store` in the
   planner, plus the executor's `mutates_store` for the `LOAD CSV` and
   procedure shapes — and the composition rules read it too. A catalog write
   and a `FOREACH` therefore count for the `USE` placement, `CALL { … } IN
   TRANSACTIONS` and subquery-expression rejections, where the previous write
   list left catalog commands out: `EXISTS { ALTER CURRENT GRAPH TYPE … }` used
   to be admitted to a read view and reach the read-only scratch's invariant
   panic, and now fails at plan time with the composition error. That is a
   dialect tightening recorded here because it shares the note's one predicate;
   it changes no accepted query's rows, only the error a write in a read
   position earns.

   The rerun cases follow: an idempotent `MERGE` that matches an existing
   pattern and writes nothing applied nothing, so it reports; an idempotent
   `SET` that re-writes the same value still applies the property write (the
   redo records it), so it does not; and a `MATCH … SET` whose `MATCH` bound no
   rows applied nothing, so it reports. The boundary is what the engine
   applied, not whether the graph looks different afterwards.

   The effect is the engine's execution metadata, published on `QueryResult`
   beside the token receipt rather than as announced result metadata
   (`ResultHeader`): no wire envelope, Bolt / HTTP / MCP field or client-ABI
   behavior changes, and the surfaces that render client-facing metadata stay
   byte-identical (Decision 5).
3. **Per-statement note.** After a batch statement that is a zero-effect write
   and succeeded, one line to stderr:

   ```text
   note: write statement changed nothing; the graph is unchanged
   ```

   A batch holding more than one statement names the statement's position,
 with [ADR-0199](./0199-statement-progress-on-failure.md)'s numbering exactly (a dot-command counts as typed; empty
   fragments and a trailing `;` do not):

   ```text
   note: write statement 2 of 3 changed nothing; the graph is unchanged
   ```

   The line follows the statement's own output, once per zero-effect write.
4. **Commit-time summary.** When `.commit` succeeds and closes a transaction
   whose merged effect (Decision 2's three states, merged over every statement
   the transaction ran) is a write that applied nothing, one line:

   ```text
   note: .commit changed nothing; the transaction applied no changes
   ```

   Every statement the transaction executed folds into the ledger — the effect
   is the execution's fact, so a statement whose render failed after the
   engine applied its change still folds its real effect in — and `.begin`
   (like a successful `.commit` or `.rollback`) is its one reset, so at
   `.commit` the ledger holds exactly the statements the open transaction
   ran: an autocommit statement outside a transaction cannot reach a later
   summary, and no in-transaction guard duplicates the reset. A plan-producing
   `PROFILE` folds in too — it executed, so its applied write silences the
   summary even though it printed no per-statement note, and its attempted
   no-op makes the summary report — while `EXPLAIN`, which never executes,
   folds in as a read. A transaction holding no write statement stays silent —
   committing a read-only transaction is not news, and neither is one whose only
   write-classified statements are read-only to the engine (`CALL { … }` with a
   read-only body, `LOAD CSV … RETURN`). The summary is additional to the
   per-statement note: the statement line names where the no-op happened, the
   summary states that the commit persisted nothing. `.rollback` prints no
   **summary** — the discard was asked for — but it does not retroactively
   silence the per-statement notes: a statement note is printed when its
   statement succeeds, before the discard is known, so a rolled-back
   transaction still shows the notes of the zero-effect writes it ran. A
   rolled-back change is never counted as applied for a summary either: the
   engine un-counts the changes a rolled-back batch truncated (Decision 2), and
   the `.rollback` reset drops the ledger before any later `.commit` could read
   it. A `.commit` that fails keeps its error line and exit code with no
   summary.
5. **Exit codes and streams are unchanged.** A note is a report, never a
   failure: the run keeps the code it earned — 0 for a successful zero-effect
 write — unlike [ADR-0164](./0164-exit-open-transaction.md)'s exit 1, which reports a real discard of writes. The
 note is not an error and never joins the [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) / [ADR-0199](./0199-statement-progress-on-failure.md) error envelope;
   stdout bytes are unchanged in every mode. Decision 2's engine effect is
   execution metadata on `QueryResult` (beside the token receipt), not
   announced result metadata: no protocol message, wire envelope or client-ABI
   field changes, and Bolt, HTTP, MCP and the client ABIs do not report it.
6. **The wording is a byte-pinned observable contract**, on the [ADR-0164](./0164-exit-open-transaction.md)
   precedent: the three lines above are exactly the bytes the CLI writes and
   stay stable across releases, and tests pin them verbatim. A wrapper script
   greps stderr for the **anchored note prefix**, not the shared tail: the
   per-statement note starts `note: write statement` (then the optional
 [ADR-0199](./0199-statement-progress-on-failure.md) position) and the summary starts `note: .commit`, while `changed
   nothing` is common to both. The lowercase `note:` prefix (no trailing
   period) matches the CLI's other non-failure diagnostics such as the row-cap
 `warning:` line ([ADR-0196](./0196-row-cap-for-agent-runs.md)); [ADR-0164](./0164-exit-open-transaction.md)'s `Warning:` remains its own text.

## Alternatives considered

- **An opt-in `--strict` / `--verbose` flag reporting per-statement row
  counts** (what LABS-313 suggested). Rejected: the author who does not suspect
  a no-op never passes the flag, which is precisely the reader who lost the
  debugging time; it also adds a flag to every bundle for a report the engine
  already makes cheap, while a row-count report states the wrong statistic for
  writes (see the row-signal alternative below).
- **One note at `.commit` only, no per-statement line.** Rejected: a script can
  hold several transactions and dozens of statements, and a commit-only line
  says the transaction did nothing without naming the statement whose pipeline
  was empty — the one fact the debugging session needed. The statement line is
  what points at the Cypher to fix.
- **Include the interactive shell.** Rejected: at a prompt a `MATCH` that finds
  nothing and writes nothing is routine, and the user already sees each
 statement's result; [ADR-0199](./0199-statement-progress-on-failure.md) keeps the shell out of batch reporting for the
  same reason, and the chatter would be pure noise in exploration.
- **Change the exit code of a zero-effect write or transaction.** Rejected: the
  semantics are correct Cypher — a conditional `MERGE` or `DELETE` that matched
  nothing is a legitimate outcome, not an error — and a non-zero exit would
  break scripts that rely on it and every wrapper that treats non-zero as a
 crash. [ADR-0164](./0164-exit-open-transaction.md)'s exit 1 reports discarded writes (data loss); this ADR
  reports a correct no-op and leaves the code alone.
- **Use the returned row count as the effect signal instead of the counters.**
  Rejected: rows are empty for every write without `RETURN` (`CREATE (:A)`,
  `MATCH … SET`), so the criterion would report every successful write and still
  not identify the no-op; only the engine's execution facts state the effect
  directly, per statement.
- **Keep the query-type plus `QueryStats` criterion and special-case the false
  positives.** Rejected: it needs the CLI to know that `ALTER DATABASE`, the
  graph-type commands and the ontology procedures are catalog writes, that a
  top-level `CALL { … }` and `LOAD CSV` are conservative classifications, and
  every future clause or procedure would need another entry — the CLI would
  re-derive an engine fact from names, and get it wrong the next time the
  planner grows a write form.
- **Make the planner's query type precise instead of adding an engine effect.**
  Rejected: the conservative classification is deliberate and shared with the
 namespace write gate ([ADR-0332](./0332-query-type-w-rw-by-returned-columns.md), [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §4), so a top-level `CALL { … }` and
  `LOAD CSV` must keep classifying `w`/`rw`; and a precise type still says
  nothing about whether the execution applied a change, which is the fact the
  note needs.
- **Add the effect as announced result metadata (a `ResultHeader` field or a
  wire envelope key).** Rejected: the note is CLI-only (Decision 5), so a wire
  field would push a reporting decision into Bolt, HTTP, MCP and every client
  ABI that nobody asked for, and change every metadata byte pin. The effect is
  an execution fact of the token-receipt class, published on `QueryResult`
  without a protocol change.

## Consequences

- `docs/api/cli/spec.md` documents the three pinned lines beside the [ADR-0199](./0199-statement-progress-on-failure.md)
 progress line and the [ADR-0164](./0164-exit-open-transaction.md) exit warning, and the CLI description and skill
  bundle tell an agent that a silently-empty script is visible on stderr (grep
  the anchored `note: write statement` prefix) without changing what stdout
  carries.
- Tests pin each line byte-for-byte and each decision-table row: one-shot, piped
  stdin, the shell's silence, `EXPLAIN`'s silence, `PROFILE`'s plan table
  instead of a note, a `PROFILE` write silencing the commit summary (with the
  created node read back), a `PROFILE` no-op printing the summary without its
  own note, an autocommit no-op staying out of a later transaction's summary,
  read silence, the commit summary, a transaction with
  effect staying silent, the unchanged exit codes, and the criterion's negative
  cases — `ALTER DATABASE` (pinned end to end, with its persisted default
  language read back), the preview-gated ontology procedures and `CREATE GRAPH
  TYPE`, and the read-only `CALL { … }` / `LOAD CSV … RETURN` statements the
  planner classifies as writes.
- The engine's effect is covered where it is recorded (`cypherlite-executor`'s
  statement-effect tests, including a write inside a subquery body that
  never dispatches and a rolled-back batch that un-counts its changes) and on
  the public facade (`cypherlite/tests/write_effect.rs`), so the note's
  criterion cannot drift from the engine without a red test.
- The CLI keeps the effect on a run whose render failed after execution, so a
  write the engine persisted is never described as having changed nothing: the
  run reports the render failure (exit 1) and its real effect still reaches the
  transaction ledger; a unit test forces that failure through a closed writer.
- A zero-effect write inside a transaction prints two lines — the statement note
  and the commit summary. The redundancy is deliberate: locality for the
  statement, durability for the transaction.
- Machine consumers are unaffected: stdout, the JSON / JSONL / envelope
  payloads and the error object stay byte-identical to today, so an existing
  pipeline sees no change.
- The engine grows one execution fact (`QueryResult::effect`), documented in
  `docs/api/rust/spec.md` with a runnable example; no Bolt / HTTP / MCP field
  and no client-ABI behavior changes. A later suppression flag, or a `--strict`
  mode reporting per-statement counters, is a new ADR.
