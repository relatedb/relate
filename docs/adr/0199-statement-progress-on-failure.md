---
type: adr
area: cli
date: 2026-10-09
deciders: CypherLite maintainers
name: 0199-statement-progress-on-failure
status: proposed
tags:
  - errors
  - output
title: Statement progress when a multi-statement run fails
REFERENCES:
  - "[[adr/0164-exit-open-transaction]]"
  - "[[adr/0194-cli-error-envelope-and-exit-codes]]"
---

# 0199. Statement progress when a multi-statement run fails

## Context

The agent-CLI audit (`docs/agent-cli-audit.md`, finding 13) found that a run of
several statements gives no account of how far it got. For
`CREATE (:A); RETRN 2; CREATE (:B)` the first statement commits, the second is
rejected and the third never runs, and the output shows only the error. An agent
that sees exit `6` cannot tell that the database is now half-changed, or which
statement to fix and resume from.

Whether the earlier statements are durable depends on the session: each
statement commits on its own, except inside an explicit `.begin` transaction,
where nothing is committed until `.commit` and an exit discards it ([ADR-0164](./0164-exit-open-transaction.md)).

## Decision

1. **Report progress when a multi-statement run fails.** A run with more than
   one statement (one-shot `-c`, piped stdin, an interactive line holding
   several) that stops on a failed statement reports where it stopped. A run of a
   single statement, and a run that succeeds, print exactly what they print today.
2. **Stderr gets one extra line**, after the existing error line and before
   anything else:
   `statement 2 of 3 failed; 1 completed and committed; 1 not run`.
   Inside an open `.begin` transaction the middle clause reads
   `1 completed, not committed (open transaction)`.
3. **The error envelope gains three optional fields** (additive to [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md)),
   present only for a multi-statement run:

   | Field | Meaning |
   | --- | --- |
   | `statement` | 1-based index of the failed statement |
   | `statements` | how many statements the input held |
   | `committed` | how many earlier statements are durable: all that completed, or `0` while an explicit transaction is open |

   ```json
   {"error":{"code":"validation_error","exit_code":6,"kind":"syntax","message":"…","retryable":false,"statement":2,"statements":3,"committed":1}}
   ```
4. **What counts as a statement** is what the splitter yields and the run
   executes: a Cypher statement or a `.`-command, so the index matches what was
   typed. Empty fragments are not counted; `statements` is the number of runnable
   fragments, so a trailing `;` does not add one. **Only a failed Cypher
   statement reports.** A failed `.`-command (for example `.commit` with no open
   transaction) stops the run as it does today, with its existing error line and
 exit code and no envelope ([ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) keeps dot-commands outside the error
   object); giving them one is a separate decision.
5. **No change to execution.** A failure still stops a piped or one-shot batch
 at the first error ([ADR-0194](./0194-cli-error-envelope-and-exit-codes.md)'s exit code is that statement's class). This ADR
   adds reporting only.
6. **Interactive sessions** report nothing extra. A line holding several
   statements keeps going after a failure in the shell, so "N not run" would be
   false there, and the user sees the state at the prompt. Only a run that stops
   at its first failure (`-c`, piped stdin) reports.

## Alternatives considered

- **`--atomic` (all statements in one transaction) and `--continue-on-error`.**
  `neo4j-cli` has both. Rejected for now: they change what a run does, not what it
  reports, and need their own decision on interaction with `.begin`/`.commit`,
  the exit code of a partly failed run, and checkpointing. The report added here
  is what an agent needs to recover with the existing semantics.
- **A per-statement result array.** Rejected: it would change the
  `json`/`envelope` output shape for every multi-statement run, a breaking change
  for what is today a failure-only addition.
- **Always print a summary, even on success.** Rejected: it changes the output of
  every successful run and every existing script.
- **Put the progress in the `message` text only.** Rejected: an agent should
 branch on a field, not parse a sentence ([ADR-0194](./0194-cli-error-envelope-and-exit-codes.md)).

## Consequences

- An agent can tell how much of its input took effect and where to resume.
- The stderr text gains a line only on a failed multi-statement run; stdout is
  unchanged outside the JSON modes, where the error object gains fields.
- `docs/api/cli/spec.md` (Query-run errors), the README agent section and the
  CHANGELOG document the fields.
- `--atomic` / `--continue-on-error` remain open as a separate decision.
