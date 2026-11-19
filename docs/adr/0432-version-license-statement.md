---
type: adr
area: cli
claim: 2f2f4ec7cb130e681f715d3aff0b20766380a9a1
name: 0432-version-license-statement
status: accepted
tags:
  - licensing
title: The --version output states the binary's license
REFERENCES:
  - "[[adr/0055-apache-2-license]]"
  - "[[adr/0193-version-json]]"
---

# ADR-0432: The --version output states the binary's license

## Context

`--version` reports the identity of the `cypherlite` binary. [ADR-0193](./0193-version-json.md)
fixed its two shapes: the text line `CypherLite v<version>` in every mode
other than `json`, and one compact JSON object of binary facts (`name`,
`version`, `os`, `arch`) when the resolved output mode is `json`. Neither
shape says under which license the binary is offered: a distributor or
agent that ships the executable has to come back to the source repository
(and [ADR-0055](./0055-apache-2-license.md)) to learn it is Apache-2.0.

LABS-558 asks `--version` to state the license: the **name** only — never
the license text — and as a statement about the **binary**, the artifact
whose facts `--version` already reports.

One pinned guarantee is what changes: [ADR-0193](./0193-version-json.md) Decision point 2 — "Every
other mode, and no mode, keeps the text form `CypherLite v<version>` byte
for byte." The text form gains a second line, so the output as a whole is
no longer byte-identical. The 0193 decision text stays as written (the
decision-log rule); its file gains a Later references note pointing here.

## Decision

1. In every mode other than `json`, and with no mode, the `--version`
   output is two lines. The first line stays `CypherLite v<version>`
   unchanged, byte for byte. The second line is exactly
   `License: Apache 2.0 (binary)`.

   ```text
   CypherLite v0.13.0
   License: Apache 2.0 (binary)
   ```

2. In `json` mode the compact one-line object gains the member `license`
   with the value `Apache 2.0 (binary)`. Key order stays alphabetical and
 outside the contract ([ADR-0193](./0193-version-json.md) point 1):

   ```json
   {"arch":"aarch64","license":"Apache 2.0 (binary)","name":"cypherlite","os":"macos","version":"0.13.0"}
   ```

3. The license text is never printed: the output states the name, qualified
   `(binary)`, and never the text. Where the text is distributed is tracked
   separately — PR #45 removed the `LICENSE` file from this repository, and
   restoring it is not part of this decision.
4. The statement applies to the binary — the `cypherlite` executable. It
   does not assert the license of anything else that ships beside it.

## Alternatives considered

- **Print the full license text.** Rejected. LABS-558 asks for the name
  only; the Apache-2.0 text is kilobytes of noise on every version check,
 and it belongs with the source distribution ([ADR-0055](./0055-apache-2-license.md)), which `--version`
  does not carry. What that distribution ships today is tracked separately:
  PR #45 removed the `LICENSE` file, and restoring it is not part of this
  decision.
- **A `--license` flag instead of riding `--version`.** Rejected. The
  version check is the one call every caller already makes; a separate
  flag has to be discovered before it reaches anyone.
- **The `license` member in JSON mode only.** Rejected. Text mode is the
  default form; the statement would be invisible to every caller that has
  not opted into JSON.
- **The bare name `Apache 2.0`, unqualified.** Rejected. It would read as
  a claim about the whole distribution; the statement is scoped to the
  binary, and the `(binary)` qualifier carries that scope inside the value,
  where both output shapes show it.

## Consequences

- The text form is no longer exactly one line. Callers that locate the
  version line (substring, regex) keep working; callers that pin the whole
  `--version` stdout byte for byte must expect the
  `License: Apache 2.0 (binary)` line under it —
  `surfaces/cypherlite-cli/tests/e2e_version.rs` is the file, and
  `test_version_text_prints_version_and_license_lines` the test that
  pins the whole stdout.
- [ADR-0193](./0193-version-json.md) Decision point 2 now covers the `CypherLite v<version>` line
  itself, not the whole text output; the 0193 file gains the Later
  references note recording this.
- Everything else in the [ADR-0193](./0193-version-json.md) contract is unchanged: `--version` reads
  no config, opens no database, and `--help` still wins.
- The hand-written `--help` text and the `--version` row in
  `docs/api/cli/spec.md` change in the same change as the implementation
  (the CLI documentation rule, and the LABS-558 note).
