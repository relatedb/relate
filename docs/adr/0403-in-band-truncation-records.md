---
type: adr
area: cli
claim: 60a2f8d11178c3f6f875f0697572e648cbfab736
name: 0403-in-band-truncation-records
status: accepted
tags:
  - agents
  - output
title: In-band truncation records — a capped result never reads as complete
REFERENCES:
  - "[[adr/0107-jsonl-default-for-piped-output]]"
  - "[[adr/0195-result-envelope-mode]]"
  - "[[adr/0196-row-cap-for-agent-runs]]"
---

# ADR-0403: In-band truncation records — a capped result never reads as complete

## Context

[ADR-0196](./0196-row-cap-for-agent-runs.md) caps agent-driven runs at 100 rows and warns on **stderr**:
`warning: result truncated to N rows (the statement returned M); pass
--max-rows 0 for all rows`. The warning is lost on every machine consumer
that reads only stdout — and the CLI's piped default is the machine mode
`jsonl` ([ADR-0107](./0107-jsonl-default-for-piped-output.md)), the exact mode agents run. There the statement's output
simply stops at the cap: a line-based consumer (an agent, a `jq -c`
pipeline) cannot tell a capped result from a complete one.

DuckDB's Agent Mode (v2.0, Oct 2026) established the same lesson
independently: their headless A/B over 250 runs measured the token win
(−59% read-back on export runs) but the property they call out as the
mode's principle is that "it never lets a cut look complete" — explicit
in-band truncation markers with the omitted count and the true total.
Agents never mistook a capped table for a complete one in 250 runs.

The row cap itself (detection, defaults, opt-outs) is [ADR-0196](./0196-row-cap-for-agent-runs.md)'s and is
unchanged. What was missing is the record of the cut in the result stream
itself.

## Decision

The truncation fact rides **in-band**, in every mode that can carry it
without breaking the mode's own format:

| Mode | The record |
| --- | --- |
| `jsonl` | one keyed terminal line after the rows (and plan line): `{"rowCap":{"kept":N,"total":M}}` — keyed like the plan line, so it can never be confused with a data row |
| `envelope` | `"truncated": <bool>` (the [ADR-0195](./0195-result-envelope-mode.md) key, unchanged) plus `"rowCap": {"kept": N, "total": M}` when cut, omitted when not — the `plan` key's presence policy |
| `json` | when cut, the rows are wrapped: `{"data": [...], "rowCap": {...}}` — the same wrapper shape a `PROFILE` statement already uses; uncut output (a bare array) is unchanged, so the shape change itself is part of the loudness |
| `toon` | a terminal `rowCap:` entry with the counts as one inline compact JSON value, following TOON's nested-value rule — beside the `plan:`/`notifications:` entries |
| `markdown` | one footer line after the table: `> truncated: N of M rows shown` — counts only; the opt-out that names `--max-rows` stays in the CLI's stderr warning, the channel the flag's owner controls |
| `csv`, `cypher`, `table` | unchanged: the [ADR-0196](./0196-row-cap-for-agent-runs.md) stderr warning only — a record line would corrupt the strict formats' streams, and the human table mode's reader sees stderr |

The counts (`kept`, `total`) are carried once — a `RowCap` bundle on the
formatter's `StatementMeta`, replacing the old `truncated: bool` (derived
from the bundle's presence, so the two spellings cannot drift). The
stderr warning stays in every mode: it is the human's channel and the
only one `csv`/`cypher`/`table` have.

## Alternatives considered

- **Keep stderr only** (status quo): rejected — the piped default is
  `jsonl`, the mode whose consumers read stdout; the warning never
  reaches them.
- **Change the `jsonl` row lines to carry a truncation flag**: rejected —
  the flag would repeat on every row line (token cost, exactly what the
  cap exists to avoid) or sit on the last row line (indistinguishable
  from a data row).
- **Append a bare JSON object to the `json` array**: rejected — invalid
  JSON; the wrapper object is the only shape-preserved option.
- **Type-change `"truncated"` in the envelope from bool to an object**:
  rejected — breaks every existing consumer of the published key;
  additive `rowCap` keeps them working.
- **Cap in the engine (MCP/binding results too)**: rejected for this ADR
  — the engine returns complete results by contract; the cap is a CLI
 presentation concern ([ADR-0196](./0196-row-cap-for-agent-runs.md)). The engine-side `response_envelope`
  is unchanged.

## Consequences

- An agent reading any machine mode can tell a capped result from a
  complete one from the stream alone, and knows both counts and the
  opt-out.
- `--mode json` consumers must handle the wrapper-object shape when
  truncated — documented in the CLI spec beside the existing PROFILE
  wrapper, and the shape difference is itself the signal.
- The byte budget and the order-independent result hash (DuckDB's
  further truncation semantics) are deliberately **not** in this ADR;
  they are follow-ups on LABS-481 once the in-band record has landed.
- The formatter's `StatementMeta` field is renamed (`truncated` →
  `row_cap`), a breaking change to the formatter crate's Rust API — the
  formatter is a workspace surface, its consumers (the CLI, the
  formatter tests and example) move in the same change.
