---
type: adr
area: cli
claim: 714bdf09419b1f23b83941f221496affa22ff4e0
name: 0349-speedscope-export-mode-for-profile-plan-trees
status: accepted
tags:
  - observability
  - output
title: speedscope export mode for PROFILE plan trees
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0239-format-flag-spelling-and-toon-mode]]"
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0348-profile-metric-fields-db-hits-memory-bytes]]"
---

# ADR-0349: speedscope export mode for PROFILE plan trees

## Context

[ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)/[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) landed the structured `PlanNode` tree with per-operator
metrics, and [ADR-0348](./0348-profile-metric-fields-db-hits-memory-bytes.md) added `db_hits`/`memory_bytes` beside
`rows`/`time_ns`. The rendered text table answers "what ran", but not
"where did the time go, proportionally" — the question a flame graph
answers. speedscope (<https://www.speedscope.app>) is a de-facto standard,
openable in the browser with no local tooling, with a documented file
format (`https://www.speedscope.app/file-format-schema.json`) whose
evented-profile form maps onto a plan tree naturally: one frame per
operator, one open and one close event per operator, `at` values on one
nanosecond timeline.

The precedent is [ADR-0239](./0239-format-flag-spelling-and-toon-mode.md): a new public output mode in
`cypherlite_formatter::Mode` (`toon`) took its own ADR — the mode list is
a public protocol surface shared by the CLI's `--mode`/`--format` values,
`--help`, the spec flag tables, and telemetry's `OutputMode`.

## Decision

1. **New output mode `speedscope`** (`--mode speedscope`,
   `--format speedscope`, `-mode speedscope`): a `PROFILE` statement's
   plan tree exports as exactly one speedscope evented-profile JSON
   document on stdout. One frame per operator, named `"{operatorType} {id}"`
   with the tree's pre-order ids — the same ids the rendered table's `Id`
   column and the wire form's `args.Id` show. One `O` and one `C` event
   per operator; `unit: nanoseconds`; `exporter: cypherlite@<version>`.
2. **The timeline is an approximation, documented as one.** The PROFILE
   tree carries per-operator inclusive times only — no offsets, no
   exclusive times — so each parent's interval is its own inclusive time
   and its children are laid out sequentially within it. When the
   children's inclusive times sum past the parent's (whole-arm
   measurement can double-count shared work), the children's intervals
   are scaled down proportionally so every parent's interval contains
   its children's and the event order stays balanced — the property
   speedscope's importer requires.
3. **No silent empty export.** A plain query (no plan tree) or an
   `EXPLAIN` statement (a tree without PROFILE metrics) is an error —
   `speedscope mode requires a PROFILE statement: per-operator times are
   what it exports (...)` — on stderr, exit code 1. There is no
   synthesized empty profile.
4. **Docs, help text, and tests land in the same change** (AGENTS.md §2/§3):
   the hand-written `--help` text, the `docs/api/cli/spec.md` flag table
   and Speedscope export section, the formatter unit tests, the CLI
   speedscope e2e test, and the CHANGELOG.

## Alternatives considered

- **Sampled-profile export (Chrome trace format)** — rejected: the plan
  tree's clause-granularity inclusive times are not a sampled stack;
  synthesizing a trace would fabricate samples the engine never took.
  speedscope's evented form expresses inclusive intervals honestly.
- **Exclusive-time flame graph** — rejected: the dispatch loop measures
  whole clause arms; deriving per-operator exclusive times by subtracting
  children's would go negative on shared work and misattribute — the
  proportional scaling inside the documented approximation is the honest
  rendering of inclusive-only data.
- **A speedscope file under a CLI command instead of an output mode** —
  rejected: it is a projection of the query run's result, like `toon` and
 `envelope`, not a lifecycle action; [ADR-0069](./0069-cli-lifecycle-commands.md) keeps lifecycle verbs as
  commands and projections as modes.
- **Emit no document for non-PROFILE statements** — rejected: an empty
  stdout success would be a silent fallback; the typed error with exit 1
  names the fix.

## Consequences

- `Mode::Speedscope` joins `cypherlite_formatter::Mode`; the
  `--mode`/`--format` value list, `--help`, telemetry's `OutputMode`,
  and the spec tables grow one entry, all driven from the one `Mode::ALL`
  table.
- The exported document is loadable at <https://www.speedscope.app> with
  no local tooling; the approximation paragraph in
  `docs/api/cli/spec.md` is the contract for what the intervals mean.
- The speedscope mode is JSON, so the error-envelope rule (`is_json`)
  classifies it as JSON; a failure prints the machine error envelope,
  matching `--mode json`.
