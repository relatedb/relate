---
type: adr
area: cli
date: 2026-10-09
deciders: CypherLite maintainers
name: 0195-result-envelope-mode
status: proposed
tags:
  - output
title: "An `envelope` output mode: `{columns, rows, truncated}`"
REFERENCES:
  - "[[adr/0079-anonymous-usage-metrics]]"
  - "[[adr/0107-jsonl-default-for-piped-output]]"
  - "[[adr/0194-cli-error-envelope-and-exit-codes]]"
  - "[[adr/0403-in-band-truncation-records]]"
---

# 0195. An `envelope` output mode: `{columns, rows, truncated}`

## Context

The agent-CLI audit (`docs/agent-cli-audit.md`, finding 4) found that
`--json` prints a bare array of row objects, and `--jsonl` prints one object
per row. Both lose the column list: an empty result is `[]` or no output at
all, so an agent cannot tell "no rows" from "no output", cannot see the
columns of a result with no rows, and has nowhere to receive a truncation
signal (LABS-230). `neo4j-cli` prints
`{"columns":[…],"rows":[…],"truncated":false,"arrays_truncated":0}`.

Changing what `--json` prints would break every script that parses it, so the
envelope must be opt-in.

## Decision

1. A new output mode, spelled `envelope`, prints one JSON object per result
   set:

   ```json
   {"columns":["a","l"],"rows":[{"a":1,"l":[1,2]}],"truncated":false}
   ```

   `columns` lists the result columns in order, including for an empty
   result (`{"columns":["n"],"rows":[],"truncated":false}`); `rows` holds one
   object per row, built exactly as `--json` builds them; `truncated` is
   `false` here and is set by the row cap (LABS-230). The object is compact and
   is printed on one line, so a multi-statement run is JSON Lines of envelopes.
2. Selected with `--mode envelope`. There is no boolean shorthand yet; the
   output-flag vocabulary (`--format`, LABS-232) is a separate decision. `--json`,
   `--jsonl` and every other mode print exactly what they printed before.
3. The envelope mode is a machine-readable mode: a failure also prints the
 [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) error envelope on stdout, the same as `json` and `jsonl`.
4. No `arrays_truncated` field. `neo4j-cli` truncates long arrays inside
   cells; CypherLite has no such cap, and an always-zero field is noise. It
   can be added later as an additive field.
5. Telemetry ([ADR-0079](./0079-anonymous-usage-metrics.md)) is unchanged: the `output_mode` dimension reports
   `json` for this mode, so the closed value set does not grow.

## Alternatives considered

- **Change `--json` to the envelope.** Rejected: it breaks every existing
  consumer of the bare array.
- **Make the envelope the default for non-TTY output.** Rejected: [ADR-0107](./0107-jsonl-default-for-piped-output.md)
  made `jsonl` the piped default and scripts rely on it; the envelope is for
  callers that ask.
- **A new telemetry `output_mode` value.** Rejected: widens the closed set
 agreed in [ADR-0079](./0079-anonymous-usage-metrics.md) for a dimension nobody needs split further.
- **Include `arrays_truncated` for parity with `neo4j-cli`.** Rejected: it
  would always be zero.

## Consequences

- Agents get a stable shape for empty results and a place for the truncation
  flag without any change for existing users.
- Mode lists in `--help`, the README, the spec and the flag reference gain
  `envelope`.
- The formatter gains one `Mode` variant; every exhaustive match over `Mode`
  (the formatter itself, telemetry) is updated, and the compiler enforces it.

## Later references

- [ADR-0403](./0403-in-band-truncation-records.md) adds the `rowCap` key (`{"kept": N, "total": M}`, present when
  the CLI capped the rows) beside this ADR's `truncated` bool, which is
  unchanged.
