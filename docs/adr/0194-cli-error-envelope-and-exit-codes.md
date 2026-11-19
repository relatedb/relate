---
type: adr
area: cli
date: 2026-10-09
deciders: CypherLite maintainers
name: 0194-cli-error-envelope-and-exit-codes
status: proposed
tags:
  - errors
  - output
title: CLI error envelope and exit-code map for query runs
REFERENCES:
  - "[[adr/0241-query-timeout-flag-and-exit-code-7]]"
---

# 0194. CLI error envelope and exit-code map for query runs

- **Later references**: [ADR-0241](./0241-query-timeout-flag-and-exit-code-7.md) fills reserved exit code 7 as `timeout` (`--timeout`), superseding the `rate_limited` placeholder

## Context

The agent-CLI audit (`docs/agent-cli-audit.md`, finding 2) found that every
query-run failure — syntax error, read-only write, storage lock, I/O failure —
exits `1`, and that nothing machine-readable is printed: with `--json` stdout
is empty on failure and the cause is only a sentence on stderr. An agent
cannot tell "fix the query" from "retry" from "the store is busy" without
parsing prose.

The sibling `neo4j-cli` already has a closed exit-code enum and a JSON error
envelope (`{"error":{"code","exit_code","message","retryable",…}}`).
Consistency across the product family argues for the same map and shape.

## Decision

1. **Exit-code map.** The query run (one-shot, piped stdin, store open)
   adopts `neo4j-cli`'s closed enum:

   | Exit | `code` | Meaning |
   | --- | --- | --- |
   | 0 | | success, including an empty result |
   | 1 | `fatal_error` | runtime failure: store open/IO, capacity, query execution, internal |
   | 2 | `usage_error` | bad flag, missing argument (unchanged) |
   | 3 | `not_found` | reserved; not produced yet |
   | 4 | `auth_error` | reserved; not produced (no auth in the embedded engine) |
   | 5 | `conflict` | write-write conflict, or the store is locked by another opener |
   | 6 | `validation_error` | the statement is rejected: syntax, semantic or type error |
   | 7 | `rate_limited` | reserved; not produced |
   | 8 | `upstream_error` | reserved; not produced |

   The engine error variant decides the class (`Syntax`/`Semantic`/`Type` →
   6, `Conflict`/`Locked` → 5, everything else → 1). A batch that stops on
   its first failure, or runs on in the REPL, exits with the code of the last
   failing statement.
2. **Envelope.** In `json` and `jsonl` output modes a failure also prints one
   compact JSON object on stdout, after any rows already written:

   ```json
   {"error":{"code":"validation_error","exit_code":6,"kind":"syntax","message":"syntax error: …","retryable":false}}
   ```

   `code` and `exit_code` are the table above; `kind` is the engine variant
   (`syntax`, `semantic`, `type`, `execution`, `conflict`, `capacity`,
   `storage`, `locked`, `io`, `internal`); `retryable` is true only for
   `conflict` kind (a write-write conflict, which a retry can resolve). The
   `message` is the same sanitized text printed on stderr. Fields may be added;
   existing ones are never renamed or repurposed. Other modes (table, csv,
   markdown, cypher) print nothing extra on stdout.
3. **stderr is unchanged.** The human line (`syntax error: …`,
   `Failed to open CypherLite: …`) is printed exactly as before, in every
   mode, so existing stderr greps keep working.
4. **Scope.** Usage errors (exit 2) and the lifecycle commands (`clone`,
   `pull`, `info`, `skill`, `upgrade`, `telemetry`, `lab`) keep their current
   plain output and their 0/1/2 codes; their envelope is a follow-up.
   Interactive sessions are unchanged: a query error returns to the prompt.

## Alternatives considered

- **The checklist's map (validation 3, not-found 4).** Rejected: it disagrees
  with `neo4j-cli`, and agents work with both CLIs.
- **Envelope on stderr only.** Rejected: harnesses read stdout on success and
  stderr on failure, but pipelines that capture stdout (`| jq`) would see
  nothing; `neo4j-cli` mirrors to stdout and so do we.
- **Keep exit 1 for everything, add only the envelope.** Rejected: the exit
  code is the agent's control flow; one code for all failures defeats it.
- **A distinct `syntax_error` category.** Rejected: `neo4j-cli` files Cypher
  syntax errors under `validation_error`; the engine variant is available as
  `kind` for the finer distinction.

## Consequences

- **Breaking for scripts that compare the exit status to `1`:** a rejected
  statement now exits `6` and a locked store `5`. Zero/non-zero checks are
  unaffected. The change is called out in the CHANGELOG.
- Agents can branch on `code`/`exit_code` and read `retryable` without parsing
  English.
- A new engine error variant must be classified; the classifier match has no
  wildcard, so the compiler enforces it.
- `docs/api/cli/spec.md`, `docs/reference/cli-flags.md`, the README and the
  skill bundle document the map.
