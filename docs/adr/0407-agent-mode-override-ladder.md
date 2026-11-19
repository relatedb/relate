---
type: adr
area: cli
claim: 511fb4ed63a18e3a43cf7749b78e0acb55dfc972
name: 0407-agent-mode-override-ladder
status: accepted
tags:
  - agents
title: Agent-mode override ladder and the shared detection vocabulary
REFERENCES:
  - "[[adr/0196-row-cap-for-agent-runs]]"
  - "[[adr/0404-agent-facing-cli-surface]]"
---

# ADR-0407: Agent-mode override ladder and the shared detection vocabulary

## Context

[ADR-0196](./0196-row-cap-for-agent-runs.md) detects agent harnesses through a hand-maintained presence list
and applies the row cap when detection fires with stdout not a terminal.
There is no way to force the agent ergonomics on or off, and the list
pre-dates the ecosystem's convergence: the cross-harness `AI_AGENT`
proposal (the `is-ai-agent` crate, adopted by `clickhousectl`) added
generic markers (`AI_AGENT`, `AGENT`) and the GitHub Copilot trio to the
vocabulary every other harness already spelled.

DuckDB Agent Mode's override lesson (the [ADR-0404](./0404-agent-facing-cli-surface.md) research baseline):
explicit user choice must always beat detection, both directions, with
documented precedence — flag, then env var, then detection.

This is G-2 of [ADR-0404](./0404-agent-facing-cli-surface.md)'s roadmap.

## Decision

1. **The detection vocabulary extends** to the generic markers `AI_AGENT`
   and `AGENT` and the Copilot trio (`COPILOT_AGENT`, `COPILOT_CLI`,
 `COPILOT_AGENT_SESSION_ID`), joining [ADR-0196](./0196-row-cap-for-agent-runs.md)'s list and its substring
   rules (`devin`/`kiro`/`.pi/agent`). The list stays one hand-maintened
   table in `agent_vars.rs` — the e2e harness mounts the same file, so
   there is exactly one copy.
2. **One resolution ladder** (in `agent_vars::effective_agent_mode`,
   consumed as a single decided fact by the row cap and the
   announcement): the explicit `--agent-mode`/`--no-agent-mode` flag, then
   `CYPHERLITE_AGENT_MODE=1/0`, then detection gated on stdout not
 being a terminal ([ADR-0196](./0196-row-cap-for-agent-runs.md)'s gate). An explicit act is
   **unconditional** — `--agent` applies the cap even on a terminal —
   because the user asked for agent ergonomics; detection never
   overrides a terminal, because a terminal is a human's.
3. **An invalid `CYPHERLITE_AGENT_MODE`** (neither 1 nor 0) exits 2
   before any statement runs — the same contract as
   `CYPHERLITE_MAX_ROWS`.
4. **The announcement policy, fitted to a per-command CLI**: the two
   explicit on-states print one stderr line naming the trigger
   (`cypherlite agent mode on: --agent-mode (--no-agent-mode to turn off)`);
   detection never announces. DuckDB's CLI is session-oriented, so one
   banner per process is cheap; ours runs one process per command, and
   a per-command banner in an agent transcript is exactly the token
   cost DuckDB's own data warns about (their three-line banner alone
   cost 10% of the saved tokens). The deliberate acts confirm
   themselves; the ambient case stays quiet and lets the row-cap
   warning speak when it actually cuts.

## Alternatives considered

- **Adopt the `is-ai-agent` crate instead of extending the list** —
  rejected: it canonicalizes the same env-var list our module already
  owns; a dependency for a static vocabulary adds a supply-chain edge
  to every CLI build while the hand list is one tested table (the e2e
  mount guarantees a single copy). Revisit if the crate's harness
  identification (which agent, not just whether) becomes load-bearing.
- **Announce on detection too** (DuckDB's shape) — rejected for the
  per-command reason above; see the decision.
- **Make `--agent-mode` also change the output mode** (e.g. the compact
  `toon` default) — rejected here: that is G-3, gated on the headless
 A/B measurement [ADR-0404](./0404-agent-facing-cli-surface.md) prescribes; landing it unmeasured would
  trade a measured win for a taste.
- **A dedicated `--agent-status` introspection flag** — rejected: the
  announcement plus `--help`'s ladder description cover the need; a
  status flag is surface area without a user.

## Consequences

- The row cap's default is "agent mode is on" — the cap policy in
  `row_cap.rs` consumes the decided mode and no longer knows how it was
  resolved.
- `CYPHERLITE_AGENT_MODE` joins the e2e harness's ambient scrub set (a
  test child never inherits the host's override).
- The flag table, `--help`, the spec's flag and env tables, the README,
  and the agent-safety skill reference all name the ladder — the CLI
  docs rule.
- Future agent ergonomics (the G-3 output default, the byte budget, the
  result hash) resolve through the same ladder and the same decided
  `AgentMode` fact — they must not re-derive detection.

## Later references

- [ADR-0196](./0196-row-cap-for-agent-runs.md): the detection gate this ladder tops; its vocabulary is
  extended here (the one list, more names).
- [ADR-0404](./0404-agent-facing-cli-surface.md): the program's principles (machine-native by default,
  deliberate choice beats detection, honest measurement) — this is its
  G-2 landing.
