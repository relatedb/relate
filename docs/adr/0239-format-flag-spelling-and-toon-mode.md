---
type: adr
area: cli
name: 0239-format-flag-spelling-and-toon-mode
status: accepted
tags:
  - output
title: "`--format` output-flag spelling and the `toon` output mode"
REFERENCES:
  - "[[adr/0192-cli-unknown-flag-suggestions]]"
  - "[[adr/0226-ontology-mappings]]"
  - "[[adr/0280-inf-nan-variable-semantics]]"
---

# ADR-0239: `--format` output-flag spelling and the `toon` output mode

## Context

(Numbered [ADR-0226](./0226-ontology-mappings.md) when written; renumbered to avoid a collision
with the ontology ADR [ADR-0226](./0226-ontology-mappings.md) that landed on main first.)

LABS-232 (agent-CLI audit, `docs/agent-cli-audit.md` finding 5). Other CLIs
an agent composes with — neo4j-cli, and the agent checklist itself — spell
the output selector `--format json|table|toon`. The cypherlite CLI only
accepted `--mode` (plus the `--json`/`--csv`/`--jsonl`/`--markdown`
shortcuts), so `--format` and `-o` hit the unknown-flag error, and the
edit-distance did-you-mean matcher ([ADR-0192](./0192-cli-unknown-flag-suggestions.md)) could not bridge
`--output`/`--format` to `--mode` — the spellings are too far apart.

Separately, agents piping results into a model context want a compact
tabular notation. TOON (Token-Oriented Object Notation) encodes a uniform
row set as a header line naming the columns once plus one line per row,
dropping the per-row key repetition of JSON. A query result is exactly a
uniform row set, so it maps onto TOON's tabular-array form directly.

## Decision

1. **`--format` is a documented alias of `--mode`.** Same value grammar,
   same error text (`Error: invalid mode '<value>' …`), same precedence
   (last output flag wins). It is a first-class double-dash spelling listed
   in `--help` and the spec flag table — not a legacy single-dash alias.
2. **New output mode `toon`.** A result set renders as one header line
   `rows[N]{col1,col2,…}:` followed by N two-space-indented lines of
   comma-separated values. Scalars follow TOON: integers, floats, booleans
   and `null` print bare (non-finite floats print as their JSON strings
 `"NaN"`/`"Inf"`/`"-Inf"`, matching the other machine modes, [ADR-0280](./0280-inf-nan-variable-semantics.md));
   strings print bare unless they are empty, contain a comma, quote,
   backslash, or control character, have leading/trailing whitespace, or
   would re-read as a number/boolean/null — those print JSON-quoted.
   Nested values (lists, maps, nodes, relationships, paths, temporal
   values) print as inline compact JSON, the same projection `jsonl` uses.
   `--noheader` does not apply: the column list is structural. A `--toon`
   shortcut flag joins the other output shortcuts.
3. **`--output` and `-o` get a hard-coded did-you-mean** pointing at
   `--format`, as a small alias table consulted before the edit-distance
 matcher ([ADR-0192](./0192-cli-unknown-flag-suggestions.md)). They remain errors — accepting them silently would
   grow a second spelling family — but the suggestion names the real flag.
4. **Docs, help text, and tests land in the same change** (AGENTS.md §2/§3):
   the hand-written `--help` text, `docs/api/cli/spec.md` flag tables,
   formatter unit tests, CLI parse/e2e tests, and the CHANGELOG.

## Alternatives considered

- **Accept `--output`/`-o` as live aliases too** — rejected: two alias
  families for one knob is exactly the drift the did-you-mean list exists
  to absorb; the suggestion text teaches the real spelling at zero
  maintenance cost.
- **Full upstream TOON spec (nested tabular detection, key folding,
  length markers on every array)** — rejected: a query result is already a
  flat uniform row set; the tabular-array form covers it. Nested values
  stay inline JSON rather than recursively re-encoding, keeping the
  encoder small and the output deterministic.
- **Depend on an external TOON crate** — rejected (R6-shaped review call):
  the encoding here is ~60 lines against the existing
  `cypher_value_to_json` projection; a new dependency buys spec surface
  this CLI does not use.
- **Add `toon` without `--format`** — rejected: the audit finding is the
  spelling mismatch; fixing only the mode leaves `--format` users at the
  unknown-flag error.

## Consequences

- `Mode::Toon` joins `cypherlite_formatter::Mode`; the `--mode`/`--format`
  value list, `--help`, telemetry's `OutputMode`, and the spec tables grow
  one entry, all driven from the one `Mode::ALL` table.
- `--format` parses in every bundle, exactly like `--mode`.
- TOON output is line-oriented and stable enough for golden tests, but it
  is a lossy-for-humans projection of graph values (nodes/relationships
  render as inline JSON); lossless structure remains `json`/`jsonl`/
  `envelope`.
- The error-envelope rule (`is_json`) classifies `toon` as non-JSON: a
  failure under `--mode toon` prints the human stderr line only.
