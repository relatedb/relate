# 0279. The `lab` command namespace

- **Status**: Accepted
- **Renumbered**: from 0156 (shared-number cleanup)
- **Date**: 2026-10-01
- **Deciders**: CypherLite maintainers
- Later references: ADR-0431 §11–§12 retires the namespace outright — the
  importers became first-party extension crates under the `extensions/`
  group (ADR-0434), the hidden `neo` egg is deleted entirely, and the
  `lab` command word is gone with no stub, superseded by `run`.

## Context

The `cypherlite` CLI parses argv by hand and, per ADR-0069, intercepts
exit-after **lifecycle verbs** only as the first argument (`skill`,
`upgrade`/`update`, `telemetry`, `pull`). ADR-0104 adds compile-time
capability as Cargo feature bundles: a command word whose feature is compiled
out is still recognized as a command and prints
`Error: the '<command>' command is not included in this build of
cypherlite` (exit 2), so the argv grammar is identical in every bundle.

Two kinds of command do not fit the existing surface:

- **Experimental commands** that manipulate a graph from outside the query
  engine (import a filesystem, generate a synthetic graph). They are
  exit-after commands, so they belong in the positional-command grammar, but
  they are not lifecycle verbs about the installed tool and should not be
  spelled as one.
- **Playful commands** that exist for their own sake. They must work when
  someone knows they are there, but they are deliberately not advertised:
  listing them in `--help` and in the spec tables would turn an easter egg
  into a supported feature with an implied compatibility promise.

Both needs point at the same shape: a single command word that names a
**namespace** of actions, separate from the lifecycle verbs. `lab` is that
namespace. Its first visible action is `fs-to-graph` (its contract is
ADR-0157); its first hidden action is `neo`.

## Decision

1. **`lab` is a new first-positional command word** in the hand-rolled
   `cypherlite` parser, alongside the ADR-0069 lifecycle verbs. Its shape is
   `cypherlite lab <action> [--flags]`. A database path literally named `lab`
   stays reachable via a `./` prefix or after `--`, exactly as ADR-0069
   provides for `skill`, `upgrade`, and `update`.
2. **`lab` is reserved in every build, including builds where its feature is
   compiled out** (ADR-0104). A build without the `lab` feature
   recognizes `cypherlite lab …` as a command and prints the shared
   not-included error with exit 2; it never falls through to opening a
   database named `lab`. The escape hatch (`./lab` or `-- lab`) is identical
   in every build.
3. **Actions are either visible or hidden.**
   - A **visible** action is listed in the hand-written `--help` text and in
     the command tables of `docs/api/cli/spec.md`, and is covered by tests.
   - A **hidden** action works, but is deliberately never listed in `--help`
     or in any spec table. It is an easter egg: not part of the documented
     surface, and not owed compatibility or a changelog entry. Hidden actions
     still get tests, because no behavior ships without tests (AGENTS.md §4).
   - The first visible action is `fs-to-graph` (ADR-0157). The first hidden
     action is `neo`.
4. **The `lab` Cargo feature ships in the `full` bundle only.** It is not in
   `agent` or `minimal`, so sandbox and query-only builds carry neither the
   experimental commands nor their dependencies. The feature table and bundle
   table in `docs/api/cli/spec.md` record this, per ADR-0104.
5. **`--help` lists only visible actions that are compiled in.** A build with
   the `lab` feature lists `lab fs-to-graph` and never `lab neo`; a build
   without the feature lists `lab` only as a recognized, not-included
   command (or omits it, matching how ADR-0104 handles other gated commands).

## Alternatives considered

- **Extend an existing lifecycle verb** (for example `cypherlite skill
  fs-to-graph`, or a new action on `pull`). Rejected: lifecycle verbs
  describe the installed tool — installing a skill, upgrading the binary,
  toggling telemetry, pulling a remote graph. An experimental or playful
  graph command is a different category, and hanging it off `skill` would
  make the verb's meaning depend on its action and mislead readers of the
  grammar.
- **A `--lab` flag** (for example `cypherlite --lab fs-to-graph`). Rejected:
  ADR-0069 settled that flags **configure a query-engine run** and never
  encode a lifecycle action; `--skill install` was the mistake that rule
  exists to prevent. A `--lab` flag would reintroduce exactly that
  command-as-flag shape and put a command word in the flag namespace where
  the database path already lives.
- **Make every action visible.** Rejected: the hidden-action affordance is
  the point. Listing `neo` in `--help` and the spec tables would advertise an
  easter egg as a supported feature, invite bug reports and compatibility
  expectations, and leave no room for a command that is meant to be
  discovered rather than documented.
- **A separate `cypherlite-fun` binary.** Rejected: a second binary means a
  second `[[bin]]`, separate help text, separate docs, and a second name
  scripts and skills must know; ADR-0104 already rejected per-bundle binaries
  for the same reason. The namespace achieves the separation inside the one
  `cypherlite` name and one parser, and the `lab` feature keeps it out of
  builds that do not want it.

## Consequences

- The ADR-0069 positional-command grammar gains `lab`. `main.rs` must
  recognize `lab` in every build; only the dispatch target changes (the real
  dispatcher, or the shared not-included error with exit 2).
- `docs/api/cli/spec.md` gains a `lab` command entry, a `lab` feature row,
  and `lab` in the `full` bundle row, and documents only the visible
  `fs-to-graph` action. Hidden actions appear in no table.
- `cypherlite-cli` gains a `lab` Cargo feature and `full` gains it. Tests for
  `lab` actions are gated on the feature; the compiled-out path is tested in
  `agent` and `minimal`.
- `docs/api/cli/spec.md` (the `lab` command entry, feature and bundle rows,
  and the `fs-to-graph` usage), `AGENTS.md` (the commands-vs-flags grammar
  rule), and `docs/reference/cli-flags.md` (the lifecycle-command table and
  the reserved-word note) describe `lab fs-to-graph` as a visible command, so
  agents and scripts can discover it.
- Adding a later `lab` action means deciding, explicitly, whether it is
  visible or hidden; a hidden action that is later promoted to visible is a
  documentation change, not a new decision, and needs no ADR.
- The hidden-action policy is a contract: hidden actions are not covered by
  semver, the changelog, or the client-facing documentation rules. Changing
  or removing one is not a breaking change. This ADR is the record that the
  policy is intentional.
