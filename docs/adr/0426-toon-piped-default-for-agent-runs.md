---
type: adr
area: cli
claim: cf4f9f39333ff83afc21fde6cd55856327fc36b0
name: 0426-toon-piped-default-for-agent-runs
status: proposed
tags:
  - agents
  - output
title: Agent-detected piped runs default to `toon`
REFERENCES:
  - "[[adr/0107-jsonl-default-for-piped-output]]"
  - "[[adr/0196-row-cap-for-agent-runs]]"
  - "[[adr/0239-format-flag-spelling-and-toon-mode]]"
  - "[[adr/0403-in-band-truncation-records]]"
  - "[[adr/0404-agent-facing-cli-surface]]"
  - "[[adr/0407-agent-mode-override-ladder]]"
SUPERSEDES:
  - "[[adr/0107-jsonl-default-for-piped-output]]"
---

# ADR-0426: Agent-detected piped runs default to `toon`

## Context

The piped output default — what a run prints when stdout is not a
terminal and no `--mode` is given — is JSONL ([ADR-0107](./0107-jsonl-default-for-piped-output.md)). JSONL is
deterministic and machine-parseable, but it is token-heavy for the
consumer that now dominates piped runs: a coding agent reading the
CLI's output back through a Bash tool. Every line repeats the column
names; a wide result costs an agent hundreds of re-read tokens for
layout it does not need.

DuckDB's Agent Mode experiment (the external baseline for this work)
measured compact tabular output with typed headers as their winner:
−59% tool-output tokens on export runs with zero correctness delta —
and published the non-wins honestly. Our `toon` mode ([ADR-0239](./0239-format-flag-spelling-and-toon-mode.md)) is
exactly that shape: one `rows[N]{col,…}:` header plus one
comma-separated line per row. It is opt-in today.

 G-3 ([ADR-0404](./0404-agent-facing-cli-surface.md) principle 5) required a measured decision before
any flip: no default changes on taste. The instrument — the headless
agent A/B harness (`testing/cypherlite-agentbench`, #485) — runs a
fixed question set through headless Claude Code sessions, one arm per
output mode (injected through a PATH wrapper the agent never sees),
measuring the bytes of tool output the agent reads back, plus turns,
wall time, and answer correctness (word-level gate) as the honesty
clause.

The agent-mode ladder ([ADR-0407](./0407-agent-mode-override-ladder.md)) gives the flip a precise gate:
agent ergonomics are "in effect" when the mode is on — an explicit
`--agent-mode`/`CYPHERLITE_AGENT_MODE=1`, or a detected harness
([ADR-0196](./0196-row-cap-for-agent-runs.md)'s vocabulary, gated on non-tty stdout). Human pipelines —
CI scripts, `| jq` plumbing, anything without a harness present —
resolve the mode off and keep JSONL.

## Decision

When agent ergonomics are in effect ([ADR-0407](./0407-agent-mode-override-ladder.md)) and no explicit
`--mode` is given, a piped run's default output mode is `toon`
([ADR-0239](./0239-format-flag-spelling-and-toon-mode.md)). The precedence, highest first:

1. an explicit `--mode`/`-mode` flag — always wins, both directions
   (`--mode jsonl` pins the old default for an agent that wants it)
2. a terminal stdout — the human table ([ADR-0196](./0196-row-cap-for-agent-runs.md): a terminal is a
   human's; the agent mode never rewrites it)
3. piped + agent mode on — `toon`
4. piped + agent mode off — JSONL ([ADR-0107](./0107-jsonl-default-for-piped-output.md), unchanged)

An explicit `--agent-mode` asks for agent ergonomics even on a
terminal — but the terminal still gets the table: the flag changes
machine defaults (this ADR, the row cap's default), not the human
view.

The in-band truncation records ([ADR-0403](./0403-in-band-truncation-records.md)) ride the mode unchanged:
`toon` carries the keyed terminal line, so a capped result still never
reads as complete.

## Alternatives considered

- **Keep JSONL everywhere; document `toon` for agents.** Rejected:
  agents do not read documentation before running a binary — the
  measurement exists precisely because defaults, not docs, decide
  what an agent's run emits. Opt-in modes do not change what the
  unmodified agent session actually reads.
- **Flip the piped default to `toon` unconditionally.** Rejected:
  JSONL is the right shape for human pipelines — `| jq` plumbing and
  CI scripts parse lines as JSON objects; a human-authored script
  with no harness present must not wake up to tabular output. The
 detection gate ([ADR-0407](./0407-agent-mode-override-ladder.md)) is what keeps the blast radius to runs
  that already opted into agent ergonomics.
- **Wait for a longer campaign before deciding.** The 3-rep campaign
  below is the deciding measurement and it confirmed the single-rep
  signal (wide-result bytes win, correctness level); the A/B reruns
  cheaply on any future claude version. Rejected as a blocker: the
  direction is reversible by one default-mode line, and the explicit
  `--mode jsonl` escape hatch is documented in the announcement and
  help.

## Consequences

- An agent session (a detected harness, or `--agent-mode`) that runs
  the CLI piped without a `--mode` now reads `toon` instead of JSONL:
  fewer tool-output bytes, one typed header instead of per-row
  column names.
- `--mode jsonl` pins the previous behavior explicitly — the one-flag
  escape hatch for any agent pipeline that wants objects.
- The A/B harness's `baseline` arm (pass-through wrapper) must pin
  `--mode jsonl` once this lands, or both arms resolve to the same
  default; the harness is updated in the same change.
- `docs/api/cli/spec.md` and the agent-mode help entry change with
  the default table.
- [ADR-0107](./0107-jsonl-default-for-piped-output.md)'s piped default statement is superseded for agent-mode
  runs only; its JSONL contract for human pipelines stands.

## Measured evidence

The headless A/B (`cypherlite-agentbench`, 5 questions × 3 reps × 2
arms; metric = tool-output bytes the agent read back; distributions as
p25/median/p75, because the run-to-run spread is larger than the
arms' differences on small results — a bare median hides exactly the
shape that decides):

| arm | tool-output bytes | input tokens | turns | seconds | correct |
| --- | --- | --- | --- | --- | --- |
| `jsonl` (baseline) | 14/19/2185 | 3333/10571/17495 | 2/2/4 | 11.1/16.3/33.6 | 12/15 |
| `toon` | 22/25/2093 | 2925/9709/10581 | 2/2/3 | 6.1/6.6/11.4 | 12/15 |

The wide result alone (a 120-row listing — the shape that costs an
agent the most re-read bytes):

| arm | tool-output bytes (p25/med/p75) |
| --- | --- |
| `jsonl` | 2185/2475/3437 |
| `toon` | 1494/1494/2275 |

Read honestly, the distributions cut both ways:

- **The win is the wide tail.** On the wide result toon reads ~40%
  fewer bytes at every percentile — the column names appear once in
  the `rows[N]{cols}:` header instead of on every row — and toon's
  *worst* run (2275) sits at jsonl's *best* (2185). The win scales
  with result width; DuckDB's published −59% came from the same
  export-shaped runs.
- **The non-win is the small result.** At the overall median the
  bytes move the *wrong* way — 19 → 25 — because nine of fifteen runs
  are tiny results where toon's header is pure overhead (~6–8 bytes).
  Under the agent-mode row cap (100 rows), most reads are small, so
  the median of a mixed workload is not where this flip pays.
- **Correctness is level** (the three misses are one question failing
  in both arms — a prompt artifact), **turns are level**, and the
  total input tokens are flat at every percentile — see the anatomy
  below for why that flatness is expected and why it is not the whole
  cost story.
- **Wall seconds favored toon** across the distribution (16.3 → 6.6
  median), but the A/B does not control for model-load variance, so
  this is reported as observed, not claimed.

The decision rule ([ADR-0404](./0404-agent-facing-cli-surface.md) principle 5) — a material tool-output-bytes
win **with correctness held** — is met on the wide tail and only there.
Whether a tail-shaped win justifies a default flip for all agent runs
(tiny results included) is the judgment this Proposed ADR puts to
review: the case is real but concentrated, the counter-cost is ~6 bytes
per small result, and `--mode jsonl` is the escape hatch either way.

## Anatomy of the context

Why the input-token columns are flat, and why that is not the whole
cost story. Each headless session's input tokens decompose into:

| Slice | Size (typical) | Cache | Price |
| --- | --- | --- | --- |
| fixed context — system prompt, tool schemas, skill | ~14k tokens (measured: a "say ok" session reads 14,006 input tokens before any query) | re-read every turn, prompt-cache hit | ~0.1x, fast |
| conversation history — prior turns and their results | grows with turns | prompt-cache hit | ~0.1x |
| fresh input — the tool result the last turn just produced | 14–3437 bytes ≈ 4–900 tokens | **cache miss** | full price |

The flat input medians are the fixed context dominating the *count*.
But the count hides the price structure: the cached base is the
cheap part of every turn, while the tool output is the only slice the
CLI emits fresh each turn — the session's most expensive tokens per
unit are exactly the ones this ADR controls. The measured win
(~950 bytes on the wide result) is ~250–300 full-price tokens the
model never has to read; against a 14k-token cached base it is
invisible in the totals, and decisive in the slice that is actually
billed at full price and parsed by the model. (Cache-write tokens for
the fresh slice appear once; later turns re-read it as cache hits.)
