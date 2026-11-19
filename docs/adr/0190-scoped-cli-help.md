---
type: adr
area: cli
date: 2026-10-08
deciders: CypherLite maintainers
name: 0190-scoped-cli-help
status: accepted
tags:
  - help
title: |-
  Scoped CLI help: a command index, per-command help, and synopsis-scoped usage errors
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0192-cli-unknown-flag-suggestions]]"
  - "[[adr/0200-query-command-and-database-flag]]"
---

# 0190. Scoped CLI help: a command index, per-command help, and synopsis-scoped usage errors

## Context

LABS-202: every `cypherlite` usage error printed the full help — roughly 82
lines — which drowned the error message, and `cypherlite clone --help` itself
failed with that same dump instead of printing clone help. The CLI’s help is
hand-written (`surfaces/cypherlite-cli/src/flags.rs::usage_text`, [ADR-0069](./0069-cli-lifecycle-commands.md)),
and two earlier decisions made one-size-fits-all text the path of least
resistance: [ADR-0069](./0069-cli-lifecycle-commands.md) recognizes lifecycle verbs as positional commands, and
[ADR-0104](./0104-cargo-feature-bundles.md) compiles commands in and out per feature bundle (`full`, `agent`,
`minimal`), so the correct help depends on both the command and the build.
The decision below is already agreed; this ADR records it before it is
implemented, per the repository’s decision-log rule.

## Decision

1. **Top-level help is a command index.** `cypherlite --help` prints the
   usage lines

   ```
   cypherlite [OPTIONS] [DATABASE] [QUERY]
   cypherlite <COMMAND> [ARGS]
   ```

   then a `Commands:` section with a one-line entry for every compiled-in
   command — `clone`, `info`, `pull`, `skill`, `upgrade` (naming its `update`
   alias), `telemetry`, `lab` — then all query-run options as before, and
   ends with the footer

   ```
   Run 'cypherlite <COMMAND> --help' for details on a command.
   ```

   No lifecycle command’s flags appear in it; only the query run keeps full
   flag detail there.

2. **Per-command help.** `-h`/`--help` anywhere after a compiled-in command
   word prints that command’s help (synopsis plus its flags/actions) to
   stdout and exits 0. `cypherlite update --help` prints the `upgrade` help;
   `lab` help omits the hidden `neo` action; a help flag before the command
   word still prints top-level help. A command word whose feature is compiled
   out keeps the not-included error (exit 2).

3. **Usage errors are scoped.** A usage error (exit 2) prints
   `Error: <msg>`, the relevant command’s synopsis line(s) as
   `Usage: cypherlite …`, and
   `Run 'cypherlite <command> --help' for more information.` — with the
   canonical name `upgrade` for `update`. Top-level parse errors (unknown
   flag, unexpected positional, invalid `--namespace`) use the query-run
   synopsis and `Run 'cypherlite --help' for more information.` The two
   literal stderr shapes are

   ```
   Error: clone requires SOURCE and DEST
   Usage: cypherlite clone [--wal-dir DIR] [--to-snapshot-format F] SOURCE DEST
   Run 'cypherlite clone --help' for more information.
   ```

   ```
   Error: unknown flag: --bogus
   Usage: cypherlite [OPTIONS] [DATABASE] [QUERY]
   Run 'cypherlite --help' for more information.
   ```

4. **Errors that print no usage today keep their stderr byte-for-byte.** A
   missing flag value, an invalid `--mode`/`--param`/`--fetch-params` value,
   and `--command` combined with a positional query are unchanged.

5. **Per-command help emits the existing telemetry events**
   (`CYPHERLITE-CLI_STARTUP` with kind `help`, and `CYPHERLITE-CLI_HELP`) —
   no schema change.

6. **Each command synopsis is defined once in source** and shared by its help
   and its usage error, so the two cannot drift.

## Alternatives considered

- **A `cypherlite help [COMMAND]` command word** — rejected: it reserves a
 new first-positional word, and under [ADR-0069](./0069-cli-lifecycle-commands.md) any such word becomes
  unreachable as a database path without a `./` prefix or `--`; `--help`
  already covers the need.
- **Per-command flag blocks for `pull`, `clone`, and `skill` in the
  top-level help** — rejected: the index stops being short, and the issue
  asked for a short top-level list.
- **Hiding the storage-tuning flags behind a `--help-all` view** — rejected:
  it adds a new flag and makes the full query-run surface harder to discover;
  all query-run options stay in the top-level help.
- **Printing the full command help on error** — rejected: it is still much
  larger than needed; the error is the message and the synopsis is the
  context.
- **Error plus hint without a synopsis** — rejected: the synopsis is what
  tells the user the expected shape of the command they just misused.
- **Recognizing `--help` only at flag position (clap-style: an unknown flag
  before `--help` still errors)** — rejected: a help request should win
  wherever it appears in that command’s arguments. Trade-off recorded: a flag
  value literally spelled `--help` or `-h` now reads as a help request.

## Consequences

- Wrapper scripts and tests that grep stderr for the full usage text must
  change.
- The help text remains hand-written and is still the source of truth, so
  the documentation must mirror it: `docs/api/cli/spec.md`, the README, the
  embedded skill bundle, and the CHANGELOG land in the same change.
- Per-command help and usage errors share one synopsis per command, so they
  cannot drift.
- The `--help` pre-scan trade-off above is accepted: a flag value spelled
  `--help` or `-h` is read as a help request.
- The positional-command grammar of [ADR-0069](./0069-cli-lifecycle-commands.md) is unchanged: commands are
  still recognized only as the first argument, and a compiled-out command
  word still fails with exit 2.

## Later references

- [ADR-0192](./0192-cli-unknown-flag-suggestions.md) adds a "did you mean" line to unknown query-run flag errors.
- [ADR-0200](./0200-query-command-and-database-flag.md) changes the top-level synopsis to
  `cypherlite [OPTIONS] [--database PATH] [QUERY]`, and the `pull` and `info`
  synopses to take `--database PATH` instead of a positional database.
