---
type: adr
area: cli
claim: cfd676072ea269494e530467659fef3242db80bd
name: 0404-agent-facing-cli-surface
status: accepted
tags:
  - agents
title: The agent-facing CLI surface — intent, principles, and the gap roadmap
REFERENCES:
  - "[[adr/0107-jsonl-default-for-piped-output]]"
  - "[[adr/0196-row-cap-for-agent-runs]]"
  - "[[adr/0239-format-flag-spelling-and-toon-mode]]"
  - "[[adr/0403-in-band-truncation-records]]"
---

# ADR-0404: The agent-facing CLI surface — intent, principles, and the gap roadmap

## Context

CypherLite's CLI is increasingly run by AI coding agents rather than
humans — the piped default is already the machine mode `jsonl`
([ADR-0107](./0107-jsonl-default-for-piped-output.md)), and [ADR-0196](./0196-row-cap-for-agent-runs.md) caps agent-harness runs at 100 rows. The
research baseline (DuckDB Agent Mode v2.0, clickhousectl, Turso
`tursodb --mcp`, MotherDuck skills; Oct 2026) measured what an
agent-facing CLI must do: DuckDB's headless A/B over 250 runs cut
read-back tokens −59% with zero correctness loss, and its stated
principle — *never let a cut look complete* — became [ADR-0403](./0403-in-band-truncation-records.md), this
program's first landed piece.

The prior agent-facing decisions were each local (a piped default, a
row cap, an envelope). This ADR states the cross-cutting intent so the
remaining gaps land as a coherent surface, not as disconnected tweaks.

## Decision

The CLI's agent-facing surface follows five principles:

1. **Machine-native by default, no flag needed.** A piped run gets
 machine output ([ADR-0107](./0107-jsonl-default-for-piped-output.md)'s `jsonl`), and an agent-harness run gets
 the row cap ([ADR-0196](./0196-row-cap-for-agent-runs.md)) — the agent's *deliberate* choice (an explicit
   `--mode`) always overrides detection, never the reverse.
2. **Records ride in-band; a cut never looks complete** ([ADR-0403](./0403-in-band-truncation-records.md)): the
   truncation counts render in every mode that can carry them, keyed or
   wrapped so they cannot be confused with data.
3. **The shared formatter stays surface-agnostic.** It renders facts
   (counts, types, notifications); surface-specific remedies — the
   `--max-rows` opt-out, flag spellings — stay in the surface that owns
   the flag (the CLI's stderr warning). A flag name never crosses into
   `cypherlite-formatter`, which http, bolt, wasm and load also build on.
4. **The human channels stay human.** Terminals keep tables, stderr
   warnings, and unchanged exit codes; agent behavior is gated on
   detection + non-terminal stdout, so human pipelines are untouched.
5. **Validate changes with a real harness, honestly.** The metric is
   tokens read from tool output in a headless two-arm A/B (DuckDB's
   methodology — not total cost, which the system prompt dominates), and
   both wins and non-wins get reported.

## Follow-ups this roadmap covers (each its own change, LABS-481)

- **Detection vocabulary + override ladder (G-2):** converge on the
  ecosystem's shared detection list (the `AI_AGENT`/`AGENT` vars and the
  `is-ai-agent` crate's ~30 harnesses), add explicit `--agent`/`--no-agent`
  override > env > detection precedence, a one-line startup announcement,
  and mode introspection.
- **Piped default A/B (G-3):** `toon` ([ADR-0239](./0239-format-flag-spelling-and-toon-mode.md), compact tabular) vs
  `jsonl` as the agent default, decided by measurement, not taste.
- **Byte budget (G-1 follow-up):** a rendered-bytes cap beside the row
  cap — row caps alone mispredict tokens on wide results (DuckDB's own
  follow-up data).
- **Order-independent result hash:** footer-vs-footer equality so
  agents compare two hashes instead of two dumps; the natural primitive
  for plan diffing (LABS-436).
- **Per-project state file (G-4):** an append-only, idempotent restore
  script (DuckDB skills' `state.sql` pattern) giving one-shot runs
  session continuity; needs its own design (which facts, who writes).
- **Optional (G-5):** compact schema one-liners, stderr progress and
  cost estimates (DuckDB's agents never referenced these mid-run —
  build only if nearly free).

Not CLI work but part of the same adoption surface: bundled library
skills in every language package (LABS-480), and the sandbox/managed-agent
testing matrix (LABS-479).

## Alternatives considered

- **Update [ADR-0196](./0196-row-cap-for-agent-runs.md) instead**: rejected — Accepted ADRs are immutable;
  0196 is one local decision (the cap), not the program's intent.
 Its Later-references note already tracks [ADR-0403](./0403-in-band-truncation-records.md).
- **No umbrella ADR, land each gap as it comes**: rejected — the gaps
  share principles (in-band, surface-agnostic facts, deliberate-choice
  precedence) that reviewers should hold them to; without the stated
  intent, each lands as an unrelated tweak and drifts.
- **Wait and write the ADR after G-2/G-3 land**: rejected — the
 principles are settled now (they are what [ADR-0403](./0403-in-band-truncation-records.md) implemented and
  what the research measured); the open items are roadmap, not decision
  debt, and each still lands with its own ADR when it decides something.

## Consequences

- Every LABS-481 follow-up cites this ADR's principles; deviations need
  a stated reason.
- The formatter-layer rule (principle 3) is enforceable in review: a
  flag spelling or surface-specific remedy appearing in
  `cypherlite-formatter` is a finding, not a preference.
- The A/B methodology (principle 5) becomes the gate for the piped
  default and any future output-format change for agents.
- This ADR adds no behavior; its first landed piece is [ADR-0403](./0403-in-band-truncation-records.md).

## Later references

- [ADR-0107](./0107-jsonl-default-for-piped-output.md) (piped machine default), [ADR-0196](./0196-row-cap-for-agent-runs.md) (row cap), [ADR-0239](./0239-format-flag-spelling-and-toon-mode.md) (toon),
 [ADR-0403](./0403-in-band-truncation-records.md) (in-band truncation records) — the decided pieces this
  program composes.
