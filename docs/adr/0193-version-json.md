---
type: adr
area: cli
date: 2026-10-09
deciders: CypherLite maintainers
name: 0193-version-json
status: proposed
tags:
  - commands
  - output
title: "`--version` honors the JSON output mode"
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0432-version-license-statement]]"
---

# 0193. `--version` honors the JSON output mode

## Context

The agent-CLI audit (`docs/agent-cli-audit.md`, finding 11) found that
`cypherlite --version --json` silently prints the plain text
`CypherLite v0.11.6`: the output flag is accepted and ignored. An agent that
asked for JSON gets a string it has to regex. The checklist (§1.1) asks for an
optional JSON form.

## Decision

1. `--version` prints one JSON object on stdout when the resolved output mode
   is `json` (`--json`, `-json`, `--mode json`, in either argument order):

   ```json
   {"arch":"aarch64","name":"cypherlite","os":"macos","version":"0.11.6"}
   ```

   Keys are sorted alphabetically and their order is not part of the contract.
   `version` is the crate version without the `v` prefix; `os` and `arch` are
   Rust's `std::env::consts::{OS, ARCH}`. The object is compact, one line.
2. Every other mode, and no mode, keeps the text form `CypherLite v<version>`
   byte for byte.
3. No `commit` or `build_date` field. Both need a build script, which the CLI
   crate does not have, and a build date makes builds non-reproducible. They
   can be added later as additive fields.
4. `--version` still reads no config and opens no database, and `--help`
   still wins when both are given.

## Alternatives considered

- **A separate `cypherlite version` command.** Rejected: [ADR-0069](./0069-cli-lifecycle-commands.md) reserves
  commands for lifecycle actions, and `--version` already exists.
- **Always print JSON.** Rejected: breaks every script parsing the text.
- **Include `commit` and `build_date` now.** Rejected: needs `build.rs` and
  git at build time, and breaks reproducible builds, for fields no consumer
  has asked for.

## Consequences

- Agents get a parseable version without a regex.
- The text form is unchanged, so no existing caller is affected.
- `docs/api/cli/spec.md`, `docs/reference/cli-flags.md` and the help text
  document the behavior.

## Later references

- [ADR-0432](./0432-version-license-statement.md) states the binary's license on `--version`: the text form gains
  a `License: Apache 2.0 (binary)` line under the `CypherLite v<version>`
  line, and the JSON object gains a `license` member with the same value.
  The text form is no longer exactly one line: Decision point 2's
  byte-for-byte guarantee now covers the `CypherLite v<version>` line
  itself, not the whole output. The decision text above is preserved as
  originally written.
