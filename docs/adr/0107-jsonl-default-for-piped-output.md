---
type: adr
area: cli
date: 2026-09-26
deciders: CypherLite maintainers
name: 0107-jsonl-default-for-piped-output
status: proposed
tags:
  - output
title: JSONL default output when stdout is not a terminal
REFERENCES:
  - "[[adr/0058-interactive-shell-surface-reduction]]"
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0079-anonymous-usage-metrics]]"
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0407-agent-mode-override-ladder]]"
  - "[[adr/0426-toon-piped-default-for-agent-runs]]"
---

# 0107. JSONL default output when stdout is not a terminal

## Context

The `cypherlite` CLI runs one query and renders its result through
`cypherlite-formatter`. The output mode is chosen by the explicit flags
(`--mode`/`-mode`, `--json`, `--jsonl`, `--csv`, `--markdown`), and when none
is given the code hardcodes a table. Two sites bake in that default:

- `surfaces/cypherlite-cli/src/main.rs` —
  `telemetry::OutputMode::from(mode.as_ref().unwrap_or(&Mode::Table))`, which
  reports the run's output mode to telemetry.
- `surfaces/cypherlite-cli/src/main.rs` —
  `Formatter::new(opts.mode.unwrap_or(Mode::Table))`, which builds the
  formatter that writes stdout.

Only one terminal check exists today, and it is on the wrong stream for this
decision: `is_terminal()` is `io::stdin().is_terminal()`, used to decide
whether a no-query run is an interactive REPL ([ADR-0058](./0058-interactive-shell-surface-reduction.md)) or a batch read of
piped stdin. There is no detection of whether **stdout** is a terminal.

`Mode::Jsonl` already exists in `cypherlite-formatter` (one JSON object per
result row, no header, no decoration) and is already reachable through the
explicit flags, so no new formatter or wire format is introduced here. The
gap is only the default.

The surrounding decisions constrain the shape of the fix:

- [ADR-0058](./0058-interactive-shell-surface-reduction.md) reduced the interactive shell surface and deliberately kept the
  one-shot and pipe flags (`--json`, `--csv`, `--mode`) as the single way to
  select output in scripts; this ADR changes the default, not that flag
  surface.
- [ADR-0069](./0069-cli-lifecycle-commands.md) fixes the CLI grammar — flags configure a query-engine run and
  never encode lifecycle actions. Choosing an output default is not a new
  flag and does not touch the command grammar.
- [ADR-0079](./0079-anonymous-usage-metrics.md) records anonymous usage metrics, including the `output_mode`
  dimension. Whatever the default becomes, that dimension must report the
  mode actually used, not the table default it used to assume.
- [ADR-0104](./0104-cargo-feature-bundles.md) puts output modes in the core query-engine run that is present in
  every feature bundle (`full`, `agent`, `minimal`), so the new default must
  work identically in all of them.

The current behavior forces every piped or scripted consumer to pass a flag:
without one they receive a human-formatted table, which is the wrong default
for non-interactive use and the reason for Linear LABS-98.

## Decision

The CLI's default output mode is **Table when stdout is a terminal and Jsonl
otherwise**; an explicit output-mode flag always wins.

## Alternatives considered

- **Keep Table as the unconditional default** (require an explicit flag for
  machine output). Rejected: piped and scripted consumers must pass a flag and
  otherwise get human-formatted output by default, which is exactly the
  behavior LABS-98 asks to remove.
- **Detect on "stdin piped OR stdout piped"** (treat either stream being
  non-terminal as non-interactive). Rejected: piping a query in while watching
  a terminal (`echo '...' | cypherlite db`) would unexpectedly change the
  output format even though the result is being read on a terminal.
- **Use JSONL for every non-REPL run regardless of stdout** (any run with a
  `-q` query or piped stdin emits JSONL). Rejected: it breaks the one-shot
  `cypherlite db -q '...'` interactive use, where users expect a table on a
  terminal.
- **Opt-in environment variable such as `CYPHERLITE_OUTPUT`** (leave the
  default as Table and let scripts opt in). Rejected: it adds configuration
  surface and still requires the user to act, whereas the convention should
  match the standard "machine-readable when not a TTY" behavior.

## Consequences

- Piped or redirected stdout now yields JSONL with no flag; the common
  `cypherlite db -q '...' | jq` pipeline works out of the box.
- Explicit output-mode flags (`--mode`/`-mode`, `--json`, `--jsonl`, `--csv`,
  `--markdown`) are unchanged and always override the terminal-based default.
- The telemetry `output_mode` dimension ([ADR-0079](./0079-anonymous-usage-metrics.md)) will shift to `jsonl` for
  piped default runs, because it now reports the mode actually used; the
  emission site must compute the same resolved mode as the formatter.
- The two hardcoded `Mode::Table` fallbacks in
  `surfaces/cypherlite-cli/src/main.rs` are replaced by one resolved default
  driven by `io::stdout().is_terminal()`; `is_terminal()` (stdin) keeps its
  current REPL/batch role unchanged.
- Existing end-to-end tests that assert table-by-default for piped/redirected
  stdout must be updated to expect JSONL, and TTY-based tests must cover the
  Table default.
- The hand-written `--help` text and `docs/api/cli/spec.md` must document the
  new default in the same change that implements it (AGENTS.md §3).

## Later references

- [ADR-0426](./0426-toon-piped-default-for-agent-runs.md) supersedes the piped default for agent-mode runs only (when [ADR-0407](./0407-agent-mode-override-ladder.md)'s ladder is in effect): a piped agent run defaults to `toon`. Human pipelines — no harness present, agent mode off — keep this ADR's JSONL contract unchanged.
