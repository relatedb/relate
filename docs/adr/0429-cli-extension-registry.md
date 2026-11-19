---
type: adr
area: cli
claim: 73fb7641739a82cdfd8a6c2b8979872d23ad8e9b
name: 0429-cli-extension-registry
status: proposed
tags:
  - extensions
title: The CLI extension registry — manifest, discovery, and the `run` mount
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0079-anonymous-usage-metrics]]"
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0157-fs-to-graph-import]]"
  - "[[adr/0230-command-did-you-mean-on-parse-errors]]"
  - "[[adr/0279-lab-command-namespace]]"
  - "[[adr/0286-fs-to-graph-explicit-db-exits]]"
  - "[[adr/0368-one-product-binary-serve-absorbs-bins]]"
  - "[[adr/0383-lab-database-flag]]"
  - "[[adr/0392-git-import-per-batch-commit]]"
  - "[[adr/0394-fresh-store-import-create-edges]]"
  - "[[adr/0430-cli-extension-wire-protocol]]"
  - "[[adr/0431-cli-extension-distribution]]"
  - "[[adr/0434-first-party-extension-crate-group]]"
---

# ADR-0429: The CLI extension registry — manifest, discovery, and the `run` mount

## Context

Graph-builder commands — importers, converters, scrapers: anything that
walks an external source and writes parameterized batches into a store —
had one home, the `lab` namespace ([ADR-0279](./0279-lab-command-namespace.md)), and every new action was a
compiled-in CLI change. The extension program (LABS-466, "one spine,
three strata") replaces that: userland commands install as packages and
mount through one extension system whose spine — manifest, registry,
discovery, trust, telemetry, help integration — is shared by every
stratum. This ADR is the spine's registry half for the first stratum
that needs it, command-kind extensions (LABS-470, P1): the manifest
schema, discovery, the namespace and collision refusals, the trust rule,
the bundle placement, and the mount. The wire the hosted child speaks is
[ADR-0430](./0430-cli-extension-wire-protocol.md), minted beside this one.

The seam this builds on is merged: P0 (#502, LABS-467) put the three
built-in `lab` actions behind one static descriptor registry in
`surfaces/cypherlite-cli/src/lab_cmd.rs` — one `LabAction` row per
action (word, visibility, flag list, synopsis, help section, parse fn),
a plain-data table that word dispatch, `--help`, the bare-`lab` error
and did-you-mean all derive through. The refactor was behavior-preserving
on purpose: LABS-470's manifest-loaded actions join that seam, so `run`
grammar, help and error conventions reach extensions without a second
enumeration of the action set.

The user-facing surface is settled (LABS-552, UX discussion 2026-10-27):
command-kind extensions mount as `cypherlite run <name> [flags]`; the
top-level grammar stays closed — installed action names never enter
`COMMAND_WORDS`, no collision policy is needed in either direction, and
top-level `--help` stays byte-stable. The deciding tickets are LABS-470
(this phase) and LABS-552 (the mount the host implements against).

## Decision

### 1. The mount: `run`, the one new reserved word for command-kind

`cypherlite run <name> [flags]` is the command-kind mount point. `run` is
a new first-positional command word in the hand-rolled parser, in the
[ADR-0069](./0069-cli-lifecycle-commands.md) shape (recognized only as the first argument), reserved in every
build ([ADR-0104](./0104-cargo-feature-bundles.md)): a build without the `extensions` feature prints the
shared not-included error (exit 2) instead of falling through to a
database named `run`, and the `./run` / `-- run` escapes keep that path
reachable in every build. The dynamic installed-actions section lives on
`run --help` and the `run` usage error — never the top-level page, which
stays byte-stable across installs. Installed names never enter
`COMMAND_WORDS` or any built-in listing, so no bidirectional collision
policy exists at the top level. The `extension` lifecycle verb
(`install | list | remove`) is the program's other reserved word and has
landed with the distribution work ([ADR-0431](./0431-cli-extension-distribution.md)), recorded there.

### 2. The manifest: JSON under the XDG config dir

One extension is one package directory:
`$XDG_CONFIG_HOME/cypherlite/extensions/<name>/` (falling back to
`~/.config/cypherlite/extensions/` — the `config_dir` precedent the
telemetry config already uses, [ADR-0079](./0079-anonymous-usage-metrics.md) §3), holding `extension.json`
and the executable. The manifest is JSON because `serde`/`serde_json`
are already workspace dependencies of the CLI and the same parser
decodes the wire ([ADR-0430](./0430-cli-extension-wire-protocol.md)); TOML would add an external dependency for
zero schema benefit (AGENTS.md R6).

Manifest fields (v1):

- `name` — the mount name, the `<name>` of `run <name>`;
- `version` — the package's own version;
- `kind` — the stratum, see §3;
- `protocol` — the per-mount-point wire version (§4 and [ADR-0430](./0430-cli-extension-wire-protocol.md));
- `flags` — the flag table: one entry per flag with name, kind
  (`string` | `integer` | `boolean` | `repeatable`) and default; the
  host parses argv from this table;
- an optional positional (like the importers' DIR);
- `requires-fresh-store` — the [ADR-0394](./0394-fresh-store-import-create-edges.md) fresh-store check;
- commit policy: `whole-import` ([ADR-0157](./0157-fs-to-graph-import.md)) or `per-batch` ([ADR-0392](./0392-git-import-per-batch-commit.md)) —
  the `CommitPolicy` the host's phase runner takes;
- `hidden` — [ADR-0279](./0279-lab-command-namespace.md)'s visible/hidden policy ported to the `run`
  listing: a hidden extension still parses and runs, appears in no
  listing, and its flags never reach did-you-mean;
- the executable — a path relative to the package directory; a path
  that escapes the package directory is a load refusal.

### 3. `kind` is a closed vocabulary from day one

`command` is the only valid kind in v1. The vocabulary is closed, not
open-ended: later strata arrive as new values — `callables` (P2,
LABS-468), `starter` (P3, LABS-533), `engine-plugin` (P4, LABS-472) —
and an unknown kind or an unknown field fails closed at load, loudly,
with the offending value named (the code-shape rules: no silent
fallback, no skipped entry). A manifest that says `kind: "callables"`
today is a refusal, not a future-proofed install.

### 4. `protocol` is per-mount-point

`protocol` versions the wire at the mount point, not a global format
version: each kind's protocol evolves at its own mount point (`command`
usage rides `run`; callables ride `CALL`; engine plugins ride open
time), so a bump at one mount point cannot break another. The registry
pins the field; [ADR-0430](./0430-cli-extension-wire-protocol.md) pins the records and the mismatch rule.

### 5. The host parses argv from the flag table

The child never parses raw argv ([ADR-0430](./0430-cli-extension-wire-protocol.md) §6). Because the grammar is
host-side, every CLI convention applies to extensions for free: help
text rendered from the table, did-you-mean over the same flag list
([ADR-0230](./0230-command-did-you-mean-on-parse-errors.md)), the usage-error exit-2 shape, and `--database` handling —
the host resolves `-d`/`--database` itself ([ADR-0383](./0383-lab-database-flag.md)) and `--db` stays
rejected with the removed-spelling wording. A flag whose value does not
parse as its kind is a usage error (exit 2) naming the flag and the
value.

### 6. Discovery: the XDG directory only

The registry walks `$XDG_CONFIG_HOME/cypherlite/extensions/` and nothing
else — never the CWD, never a project-local directory. That is the
drive-by-execution guard: the grammar a `cypherlite` invocation accepts
is a property of the user's machine, never of the directory the command
happens to run in, so cloning a checkout can never add executables to
the grammar. Load-time refusals: a name colliding with a reserved word
(`COMMAND_WORDS`) or another installed extension — the built-in `lab`
action words went with the `lab` surface ([ADR-0431](./0431-cli-extension-distribution.md) §12) — refuses the
load with a pinned error naming both sides of the
collision; the refusal is reported, never first-wins, never silent.

### 7. Trust sits at install time, never silently at runtime

Discovery lists what is already installed; it never fetches. A package
becomes executable only through an explicit install act (P3, LABS-469:
the signed first-party channel reusing the `cypherlite upgrade`
ureq/sha2/ssh-key machinery, or an explicit `--url`/`--from` trust act
for third-party). An uninstalled action fails closed with the pinned
discovery text (LABS-552 §4):

```text
no installed action '<name>' — first-party extension available: cypherlite extension install <name>
```

The install verbs have landed ([ADR-0431](./0431-cli-extension-distribution.md) §3) and the error text is the
contract still: the host ships with zero installed extensions and every
unknown name fails closed; the channel lists a package only after its
first release dispatch ([ADR-0431](./0431-cli-extension-distribution.md) §1).

### 8. Bundle placement: `extensions` at full + agent

The host code is a new `extensions` Cargo feature ([ADR-0104](./0104-cargo-feature-bundles.md)), placed in
the `full` (default) and `agent` bundles, not `minimal` (placement per
LABS-552): agent-facing builds see the mount; the minimal build keeps
the smallest query-engine surface. The argv grammar stays identical in
every build (§1). The importer extraction ([ADR-0431](./0431-cli-extension-distribution.md) §11) replaces
`lab` in those bundles — `extensions` takes the place the `lab`
feature held — and the `lab` word retires with the surface as a clean
break, no stub (the 2026-10-28 user direction, [ADR-0431](./0431-cli-extension-distribution.md) §12).

### 9. Telemetry: command `run`, action "extension"

The report says command `run`, action `extension` — never a user-chosen
name ([ADR-0079](./0079-anonymous-usage-metrics.md) anonymity), which also sidesteps `Event::Command`'s
`&'static str` requirement: the installed name is data, not a static.

## Alternatives considered

- **dlopen C-ABI plugins for CLI actions.** Rejected: an in-process ABI
  means ABI versioning and a platform matrix (symbol resolution across
  macOS/Linux/Windows; no stable Rust ABI) for zero benefit —
  command-kind actions never touch engine vocabulary, they walk a
  source and emit rows. The vtable ABI stays the P4b engine-plugin
  mechanism (LABS-472), where index providers genuinely need in-process
  calls.
- **WASM-everything.** Rejected: a heavy dependency and a wasm32 author
  toolchain for extension writers, and command-kind actions need fs/git
  access — the capability grants that let a WASM module walk a checkout
  re-open the sandbox hole WASM was meant to close.
- **exec-full-command, git-style (`cypherlite-x-foo` on PATH).**
  Rejected: the child would own the store, losing the `--database`
 convention ([ADR-0383](./0383-lab-database-flag.md)), the `:memory:` default with REPL handoff
 ([ADR-0286](./0286-fs-to-graph-explicit-db-exits.md)), and the summary/telemetry/checkpoint uniformity; git's
  helper model solves discovery for a tool with no single host lifecycle
  to preserve, which is not our case.
- **Direct top-level mounting of installed actions
  (`cypherlite <name>`).** Rejected: an open grammar contract —
  installed names would collide with reserved words and future built-ins
  in both directions, and top-level `--help` would become
  environment-dependent. The closed `run` grammar (LABS-552) keeps the
  top level byte-stable across installs.
- **TOML manifests.** Rejected: an external dependency for a schema the
  in-tree `serde_json` already decodes, and one parser then serves both
  the manifest and the wire.
- **CWD/project-local discovery (a `.cypherlite/` in the checkout).**
  Rejected as the drive-by-execution hole: convenience for one project
  buys silent code execution for every clone that runs the CLI in it.

## Consequences

- The host lands as an `extensions/` module in `cypherlite-cli`
  (registry, manifest, protocol, host), under the file-shape rules;
  fixture extensions exercise it in CI (a Rust fixture and a POSIX `sh`
  fixture, proving the language-agnosticism the wire buys; the sh one
  gated to unix lanes), with collision and malformed-manifest tests
  pinning every refusal text.
- The manifest-loaded action rows join the P0 seam's descriptor shape;
 the extraction ([ADR-0431](./0431-cli-extension-distribution.md) §11) then retires the built-in half with the
  surface itself — `fs-to-graph` and `git-to-graph` move into
 first-party extension crates ([ADR-0434](./0434-first-party-extension-crate-group.md)), the hidden `neo` is deleted
  entirely (never extracted), and the registry's one source is the XDG
 walk. [ADR-0279](./0279-lab-command-namespace.md)'s Later references note (its namespace superseded by
 `run`) and [ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md)'s multi-artifact dimension note land with the
  extraction.
- Every manifest value (a new kind, a new flag kind, a new commit
  policy) is a schema bump: the host fails closed on the unknown value
  until the change that teaches it, and old hosts refuse new manifests
  loudly instead of partially.
- Docs rule §2 applies to the new public surface:
  `docs/api/extensions/` (spec, description, examples that run in CI),
  the `docs/api/cli/spec.md` section and bundle-table row, and the
  skill-bundle reference plus SKILL.md row (the `BUNDLE_FILES`
  drift-guard enforces it).
- An Oplane threat model runs on the implementation diff before merge
  (subprocess spawn + untrusted stream parsing), per LABS-466's rules.
