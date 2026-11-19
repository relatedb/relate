---
type: adr
area: cli
date: 2026-10-09
deciders: CypherLite maintainers
name: 0196-row-cap-for-agent-runs
status: proposed
tags:
  - agents
  - output
title: "A row cap for agent-driven runs: `--max-rows`"
REFERENCES:
  - "[[adr/0195-result-envelope-mode]]"
  - "[[adr/0403-in-band-truncation-records]]"
  - "[[adr/0404-agent-facing-cli-surface]]"
  - "[[adr/0407-agent-mode-override-ladder]]"
---

# 0196. A row cap for agent-driven runs: `--max-rows`

## Context

The agent-CLI audit (`docs/agent-cli-audit.md`, finding 3) found that
`UNWIND range(1,100000) AS i RETURN i` with `--json` writes 2.4 MB to stdout.
An agent harness injects a command's stdout into the model context, so one
unbounded `MATCH (n) RETURN n` can fill the window. `neo4j-cli` caps output at
100 rows by default, warns on stderr and sets `truncated: true`.

Capping every non-TTY run would change what existing pipelines receive, so the
cap must not apply to them silently. Whether a human is at a terminal is not a
good signal either: agent harnesses run commands with piped output and no TTY,
and so do scripts.

## Decision

1. **`--max-rows N`** limits the rows printed per statement. `0` means
   unlimited. Any explicit value, in any run, is honored.
2. **Default cap: 100 rows, only under an agent harness.** With no
   `--max-rows` and no `CYPHERLITE_MAX_ROWS`, the cap is 100 when an agent
   harness is detected and stdout is not a terminal; otherwise it is
   unlimited. Interactive shells and terminals are never capped by default,
   and scripts outside a harness see no change.
3. **Detection** is by environment variable, the list `neo4j-cli` uses (which
   follows `unjs/std-env`): `CLAUDECODE`, `CLAUDE_CODE`, `REPL_ID`,
   `GEMINI_CLI`, `CODEX_SANDBOX`, `CODEX_THREAD_ID`, `OPENCODE`,
   `AUGMENT_AGENT`, `GOOSE_PROVIDER`, `CURSOR_AGENT` (any non-empty value),
   plus case-insensitive substrings `devin` in `EDITOR`, `kiro` in
   `TERM_PROGRAM`, and `.pi/agent` in `PATH`.
4. **`CYPHERLITE_MAX_ROWS=N`** sets the default for a session or a script the
   same way `CYPHERLITE_MEMORY_LIMIT` does; a bad value exits 2 like a bad
   flag. The flag overrides the variable.
5. **A capped result announces itself.** The first `N` rows print, and
 - the `envelope` mode ([ADR-0195](./0195-result-envelope-mode.md)) sets `"truncated": true`;
   - a stderr line names the statement and the opt-out:
     `warning: result truncated to 100 rows (the statement returned 100000); pass --max-rows 0 for all rows`;
   - the exit code is unchanged (`0`): truncation is not a failure.
6. **The cap limits output, not engine work.** The engine materializes a
   statement's full result before the CLI prints it; the cap protects the
   reader's context, not memory. A server-side `LIMIT` remains the way to bound
   the work.

## Alternatives considered

- **Cap every non-TTY run.** Rejected: it silently truncates existing
  pipelines.
- **Cap only on a flag.** Rejected: an agent has to know to ask, and the agents
  that do not are the ones that fill their context.
- **Truncate by bytes.** Rejected: a row count is what `neo4j-cli` and agents
  reason about; bytes cut JSON mid-object.
- **Stream and stop the query at the cap.** Rejected for now: the engine has no
  row-limit pushdown through its public API; it would be a separate engine
  decision.
- **Spill automatically above a threshold, or add `--output-file` here.**
  Rejected: a surprise file is worse than a visible warning, and an explicit
  output file has its own safety questions (exclusive create, mode, which
  statement it holds). It is a separate decision; until then `--max-rows 0`
  with a shell redirect writes the full result.

## Consequences

- Under an agent harness a large result is bounded by default, visibly, and
  the full result is one flag away (`--max-rows 0`).
- No change for humans or for scripts outside a harness.
- A harness we do not list is simply not capped until its variable is added;
  `CYPHERLITE_MAX_ROWS` is the escape hatch.
- Detection is a documented heuristic and is exposed in the spec, with the list
  kept in one place (`row_cap.rs`).

## Later references

- [ADR-0403](./0403-in-band-truncation-records.md) moves the record of the cut **in-band** (a `rowCap` record in
  every machine mode, so a capped result never reads as complete). This
  ADR's detection, defaults, opt-outs, and the stderr warning are
  unchanged — the warning remains the only channel for the `csv`,
  `cypher` and `table` modes.
- [ADR-0404](./0404-agent-facing-cli-surface.md) states the agent-facing CLI program this cap belongs to —
  its principles govern the remaining agent-surface gaps (LABS-481).
- [ADR-0407](./0407-agent-mode-override-ladder.md) extends the detection vocabulary (the generic `AI_AGENT`/`AGENT`
  markers and the Copilot trio) and tops detection with an override ladder
  (`--agent`/`--no-agent` > `CYPHERLITE_AGENT_MODE=1/0` > detection); the
  cap now consumes the resolved mode as one decided fact.
