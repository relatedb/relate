---
type: adr
area: cli
date: 2026-10-09
deciders: CypherLite maintainers
name: 0197-help-examples
status: proposed
tags:
  - help
title: Examples in `--help`, with an opt-out
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0194-cli-error-envelope-and-exit-codes]]"
---

# 0197. Examples in `--help`, with an opt-out

## Context

The agent-CLI audit (`docs/agent-cli-audit.md`, finding 10) found that no help
page has an example, and none lists the exit codes. An agent (or a person)
learns a command fastest from one working invocation. `neo4j-cli` ships many
examples per command; here the examples add length to every help page and every
help read costs context, so they must be few and skippable.

## Decision

1. **One or two examples per page.** The top-level `--help` and each command's
   `<command> --help` end with an `Examples:` section holding one or two
   invocations, each a `# comment` line above a `cypherlite ...` command line.
   The top level shows a plain read as JSON and a `--read-only` exploration with
   `--mode envelope`; each command shows its common case.
2. **One source.** The per-command examples live next to the command table
   (`CommandSpec::examples`) and the top-level pair in `examples.rs`, which also
   owns the `Examples:` rendering and the environment gate; the help text is built
   from them, so a page and its test cannot drift.
3. **They are tested.** A unit test pins the shape (one or two per page, a
   comment, a `cypherlite` command line, present in the page). An e2e test runs
   the examples that are safe to run (the top-level pair, `clone`, `info`)
   against a temp database and requires success. `pull` (network), `skill`
   (home directory), `upgrade` (network), `telemetry` (config) and `lab`
   (imports a directory) are checked for shape only.
4. **`CYPHERLITE_HELP_EXAMPLES=0` hides them.** `0`, `false`, `off` or `no`
   (case-insensitive) drops the `Examples:` section from every help page; any
   other value, or unset, shows it. Only the examples change: the synopsis, the
   flags, stdout versus stderr and the exit code are identical. The variable is
   read once, when the page is rendered, and is not a flag.
5. **No exit-code section in `--help`.** The audit also asked for exit codes in
 help. They are documented in the spec and README ([ADR-0194](./0194-cli-error-envelope-and-exit-codes.md)) and the usage line
   already points at `--help`; repeating the table on every page would grow the
   pages this ADR is trying to keep short.

## Alternatives considered

- **Many examples per command, as `neo4j-cli` does.** Rejected: each help read
  costs context for an agent, and most pages need one invocation.
- **A `--no-examples` flag.** Rejected: flags configure a query run ([ADR-0069](./0069-cli-lifecycle-commands.md)),
  and the reader of `--help` is usually not choosing flags yet; an environment
  variable can be set once for a session or a harness.
- **Opt-in (`CYPHERLITE_HELP_EXAMPLES=1`).** Rejected: the default should teach;
  the variable is for the reader who already knows.
- **A separate `cypherlite examples` command.** Rejected: a new command for what
  fits on the page a reader is already looking at.
- **Print the exit-code table in every help page.** Rejected, see decision 5.

## Consequences

- Help pages grow by two to four lines each, and an agent can drop them with one
  environment variable.
- A new command must supply one or two examples; the shape test fails otherwise.
- `docs/api/cli/spec.md`, the flag reference and the README document the
  variable.
