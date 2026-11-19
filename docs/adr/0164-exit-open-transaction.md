---
type: adr
area: cli
date: 2026-10-05
deciders: CypherLite maintainers
name: 0164-exit-open-transaction
status: accepted
tags:
  - commands
  - transactions
title: CLI exit contract for an open transaction
REFERENCES:
  - "[[adr/0028-user-visible-transactions]]"
---

# 0164. CLI exit contract for an open transaction

## Context

[ADR-0028](./0028-user-visible-transactions.md) gave the CLI user-visible transactions via `.begin` / `.commit` /
`.rollback`. An open `.begin` transaction is never checkpointed, so its
writes are discarded when the process exits — `exit_closed` /
`final_checkpoint` in `surfaces/cypherlite-cli/src/main.rs` document this
("An open `.begin` transaction is never checkpointed, so its writes are
discarded"). Today that discard is **silent**: every exit path prints
nothing and exits 0, so a user (or a script) that forgot `.commit` loses
writes with no signal.

This is a regression. Historically (commit 51e999bd, removed in 579592c9)
the shell printed to stderr on exit with an open transaction:

```
Warning: exiting with an open transaction; rolling back uncommitted changes. Use .commit before exiting to persist them.
```

and still exited 0. External wrapper scripts grep stderr for `rolling
back` to detect the discard, so the exact wording is an observable
contract, not prose.

The CLI has three exit paths (see `main` in
`surfaces/cypherlite-cli/src/main.rs`): one-shot (positional query),
piped stdin, and the interactive REPL (`.quit` / EOF). One-shot and piped
runs are batch contexts where a discarded transaction is almost always a
scripting error; the REPL is an exploratory context where the user saw
each statement execute and a non-zero exit would punish normal
`.quit`-without-`.commit` usage.

## Decision

1. **The historical warning text is restored verbatim.** Every exit path
   that finds a transaction open at exit prints exactly

   ```
   Warning: exiting with an open transaction; rolling back uncommitted changes. Use .commit before exiting to persist them.
   ```

   to stderr before exiting. The text is byte-for-byte the pre-579592c9
   string so existing wrapper scripts that grep stderr for `rolling back`
   keep working.
2. **One-shot and piped-stdin runs exit 1 in that case.** A positional
   query run or a piped-stdin run that reaches exit with a transaction
   still open prints the warning and exits 1 (the warning does not
   override a higher code already earned by a failed statement). Batch
   callers get a machine-checkable failure, not just a log line.
3. **Interactive REPL exit stays 0.** `.quit` / EOF with an open
   transaction prints the same warning but exits 0; discarding on
 interactive exit is the documented [ADR-0028](./0028-user-visible-transactions.md) behavior (connection drop
   is implicit rollback), not an error.
4. **The discard itself is unchanged.** The open transaction is still
   never checkpointed; only the reporting changes. `.commit` before
   exiting remains the only way to persist.

## Alternatives considered

- **Keep the silent discard (status quo).** Rejected: silent data loss
 on a path [ADR-0028](./0028-user-visible-transactions.md) makes easy to reach, and it regressed a stderr
  contract external scripts already depend on.
- **Exit 1 on every exit path, including the REPL.** Rejected: an
  interactive user who explored with `.begin` and quit without
 committing made no mistake — implicit rollback on drop is the [ADR-0028](./0028-user-visible-transactions.md)
  contract — and a non-zero REPL exit would break wrappers that treat
  any non-zero code as a shell crash.
- **Reword or modernize the warning.** Rejected: wrapper scripts grep
  stderr for `rolling back`; any rewording breaks them for zero
  behavioral gain. The pre-579592c9 text is restored verbatim.
- **Checkpoint the open transaction on exit instead of discarding.**
  Rejected: it would make uncommitted writes durable without `.commit`,
 reversing the [ADR-0028](./0028-user-visible-transactions.md) rule that only an explicit `commit` appends the
  transaction's redo.

## Consequences

- Wrapper scripts that grep stderr for `rolling back` work again against
  the CLI on every exit path.
- Batch callers (one-shot, piped stdin) can detect a forgotten `.commit`
  via exit code 1 without parsing stderr.
- Interactive users see the warning on `.quit` / EOF but keep exit 0, so
  REPL wrappers and shell prompts are unaffected.
- The CLI spec's exit-behavior documentation
  (`docs/api/cli/spec.md`) and the `exit_closed` / `final_checkpoint`
  doc comments must describe this contract; tests cover all three exit
  paths.
- [ADR-0028](./0028-user-visible-transactions.md) is unchanged; this ADR records the CLI surface contract for
  the discard it specifies.
