---
type: adr
area: cli
date: 2026-09-26
deciders: CypherLite maintainers
name: 0261-upgrade-skill-refresh-ownership
status: accepted
tags:
  - skill
title: Upgrade skill-refresh ownership and legacy adoption
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0262-skill-install-adopts-recognizable-bundle]]"
---

# 0261. Upgrade skill-refresh ownership and legacy adoption

- **Renumbered**: from 0110 (shared-number cleanup)

## Context

[ADR-0069](./0069-cli-lifecycle-commands.md) §7 made `cypherlite upgrade` refresh the installed agent-skill bundle
after a successful self-replace. The pure decision (`skill_refresh_decision`)
asks `Update installed cypherlite skill? [Y/n]` on an interactive TTY,
auto-accepts with `--yes`, and prints `run: cypherlite skill install`
otherwise; on yes it spawns the **new** binary as `cypherlite skill install`
(with inherited stdio) via `spawn_skill_install` in
`surfaces/cypherlite-cli/src/upgrade_cmd.rs`.

That flow predates cypherlite's skill ownership marker. LABS-84 introduced
`MARKER_FILE = ".cypherlite-installed"` in
`surfaces/cypherlite-cli/src/skill_cmd.rs` and the `ensure_replaceable` /
`is_owned` pre-flight: a destination that exists, is a symlink, or lacks a
regular-file marker is treated as user-authored/third-party content and
refused with `SkillError::NotOwned` unless `--force` is passed. The marker
first shipped in **v0.7.1**, so a bundle installed by **≤0.7.0** — or copied
by hand — carries none.

The two decisions therefore compose badly. `upgrade` detects the agent, asks
the blind `Update installed cypherlite skill? [Y/n]`, and on yes spawns
`cypherlite skill install` **without** `--force`. `ensure_replaceable` refuses
with `NotOwned` ("no .cypherlite-installed marker present … pass --force to
override"), the child exits non-zero, and `spawn_skill_install` prints
`warning: \`cypherlite skill install\` exited with …; the binary upgrade
itself succeeded`. The user answered yes and got an Error. The remediation
flag exists only on the standalone `skill install` command and is unreachable
from inside `upgrade`; the printed hint names a second manual command instead.

The root cause is that the refresh could not tell an *old cypherlite skill*
(one this project wrote, before it started stamping a marker) apart from
*genuinely foreign content* that merely occupies the same path. The decision
below teaches the refresh to recognize its own legacy bundle by content, then
updates anything it recognizes without a prompt, leaves anything it does not
recognize strictly untouched, and prompts only when nothing is installed yet.

## Decision

After a successful self-replace, `skill_refresh` **pre-flights every detected
agent's `<skills dir>/cypherlite`** and classifies it. If no supported agent
is detected, the step is skipped silently. The classification is a pure
function so every branch is unit-testable, and it reuses the same marker rules
as `ensure_replaceable` / `is_owned`:

- **Missing** — the directory is absent (`NotFound`). An unreadable or
  otherwise-uninspectable destination is **not** Missing; any metadata error
  other than `NotFound` classifies the target as Foreign (fail closed), so an
  unknown path is never silently treated as safe to create fresh.
- **Owned** — the directory exists and `<dir>/.cypherlite-installed` exists as
  a **regular file**. Symlinks are not followed, so a symlinked marker does not
  count.
- **Legacy** — no marker, but `<dir>/SKILL.md` is a real regular file whose
  YAML frontmatter (**a**) declares `name: cypherlite` **and** (**b**) contains
  a `version:` line. The version value may be a concrete version (e.g.
  `0.7.0`) or the literal `{{VERSION}}` placeholder from a pre-injection
  build; either counts as a machine-written cypherlite bundle. This is the
  content signal that identifies a **v0.7.0-era or hand-copied cypherlite
  skill** as ours.
- **Foreign** — the directory exists but is neither Owned nor Legacy, or its
  metadata cannot be inspected (any error other than `NotFound`). This includes
  a symlinked destination, a destination carrying only a symlinked marker, a
  `SKILL.md` that declares `name: cypherlite` but has **no** `version:` line,
  and an unreadable destination. Refusing an unreadable destination is
  deliberate and fail-closed: an unreadable path is never assumed to be ours or
  absent.

**Installed = Owned ∪ Legacy.** A legacy cypherlite skill is recognized by its
own `SKILL.md` and is safe to update. This is the "legacy adoption": the
marker remains the strict ownership proof for the standalone command, but the
post-upgrade refresh adopts a recognizable unmarked bundle instead of
refusing it. Standalone `cypherlite skill install|remove` keeps its strict
marker refusal from LABS-84 — recognition applies **only** to the refresh that
follows a successful `upgrade`, never to the user-invoked command.

**Why `name` + `version` rather than `name` alone.** `inject_version` has
stamped `version: <CARGO_PKG_VERSION>` into the installed `SKILL.md`
frontmatter since **v0.4.0** (confirmed by
`git show v0.4.0:crates/cypherlite-cli/src/skill_cmd.rs`), while the
`.cypherlite-installed` ownership marker only arrived in **v0.7.1**
(confirmed by `git show v0.7.1:...`). So the entire pre-0.7.1 population — the
legacy case this issue is about — carries both lines, and the machine-written
`version:` line is present in every genuine bundle while being very unlikely
to appear in a hand-authored `name: cypherlite` directory. Requiring both
lines makes a false positive materially less likely than matching `name:`
alone.

**Installed skills always update — there is no staleness skip.** The
`version:` line is an *identification* signal, not a gate. `cypherlite
upgrade` only proceeds past the version check when the release is newer than
the running binary, so the installed skill is always behind the bundle the new
binary carries; a skip-when-equal branch would be near-dead and would only
complicate the classification/decision matrix.

The refresh then behaves as follows:

1. **Foreign targets are never touched.** Each Foreign target is reported with
   the `foreign_note` line and the `foreign_remediation` line. A `--force`
   flag never reaches a Foreign directory from inside `upgrade`.
2. **If any non-foreign target is installed (Owned or Legacy), auto-update
   with no prompt.**
   - With no Foreign target present: one `skill install` spawn, passing
     `--force` iff any target is Legacy (the common case — both the marker
     and the frontmatter name are cypherlite's own, so forcing only ever
     replaces our content).
   - With a Foreign target present: **one `skill install --agent <name>`
     spawn per non-foreign agent**, passing `--force` iff *that* agent is
     Legacy. Targeting each agent individually keeps the forced replacement
     scoped to our own directories so a force never reaches the Foreign dir.
3. **If no non-foreign target is installed (all non-foreign targets are
   Missing), ask before installing.**
   - Interactive: print `install_prompt` (`[Y/n]`, yes is the default; empty
     input means yes), then install the non-foreign agents — per-agent when a
     Foreign target exists, otherwise one spawn.
   - `--yes`: install the non-foreign agents without asking.
   - Non-interactive without `--yes`: print `hint_install` and install
     nothing.

The upgrade exit code stays **0** in every post-replace path. A failed skill
install never undoes the binary upgrade: it warns and the upgrade still
reports success.

The four pinned message templates below are the user-facing contract for the
refresh. The `{path}` placeholder is interpolated at runtime; the strings are
otherwise verbatim, including the trailing space on `install_prompt`:

- `install_prompt`: `Install the cypherlite skill? [Y/n] `
- `foreign_note`: `note: {path} is not a recognizable cypherlite skill (no .cypherlite-installed marker, and SKILL.md does not declare name: cypherlite with a version line); leaving it untouched`
- `foreign_remediation`: `to replace it with the cypherlite skill bundle, run: cypherlite skill install --force`
- `hint_install`: `run: cypherlite skill install`

## Alternatives considered

- **Treat any existing `<skills dir>/cypherlite` as installed with no content
  detection.** Rejected: it silently overwrites user-authored content that
  happens to share the name, since `install_bundle` deletes the destination
  during replacement.
- **Always-per-agent refresh (spawn `--agent` for every target on every
  upgrade).** Rejected: it emits several spawn lines per upgrade for a case
  the single spawn already handles; the per-agent fan-out is reserved for the
  narrow case where a Foreign target coexists and forcing must stay scoped.
- **An `upgrade --force` flag passthrough to `skill install`.** Rejected:
  `foreign_remediation` and `hint_install` already cover scripted use without
  new CLI surface, and a passthrough would let scripts force directories
  without any detection.
- **Require only `name: cypherlite` without a `version:` line.** Rejected: the
  version line has been machine-stamped since v0.4.0 while the ownership
  marker only arrived in v0.7.1, so every genuine legacy bundle carries both;
  matching on `name:` alone is a strictly weaker ours-signal that would
  recognize a hand-authored `name: cypherlite` directory as ours and risk
  overwriting it.
- **Use the `version:` line for a staleness skip instead of always updating.**
  Rejected: `upgrade` only proceeds when the release is newer than the running
  binary, so the installed skill is always behind and the skip-when-equal
  branch is near-dead; it adds classification/decision-matrix complexity for
  no observable behavior.
- **Keep the informed default-No overwrite prompt for unmarked-but-recognizable
  installs.** Rejected: a skill that declares `name: cypherlite` together with
  a `version:` line in its `SKILL.md` is ours to update, so prompting for it
  is exactly the friction the issue reported; the prompt is now reserved for
  an all-Missing set.
- **Auto-force without recognition (force any directory that exists).**
  Rejected: it would clobber genuinely foreign content; force must only ever
  follow recognition (Owned or Legacy).

## Consequences

- `cypherlite upgrade` no longer produces a spurious `NotOwned` error after
  the user answers yes: the refresh knows which directories are its own, and
  every post-replace path exits 0.
- Upgrading users whose skill bundle predates v0.7.1 (or was copied by hand)
  are auto-refreshed with no prompt once their bundle is recognized by the
  `name: cypherlite` **and** `version:` lines in its `SKILL.md`, which is the
  legacy-adoption path this ADR adds.
- A directory that merely shares the name `cypherlite`, or that declares
  `name: cypherlite` without a machine-written `version:` line, is never
  overwritten by `upgrade`: it is reported with `foreign_note` and the user is
  pointed at `cypherlite skill install --force` if they want to replace it.
- `skill_cmd.rs`'s marker rules become shared, load-bearing policy for
  `upgrade`'s recognition step as well; changing the marker or the
  `name: cypherlite` + `version:` frontmatter convention now requires updating
  both call sites, the CLI tests, and this ADR, and a divergence is a bug.
- No new CLI flag is added. Scripted upgrades keep the existing `--yes`
  contract, now meaning "install the skill when none is installed yet" rather
  than "overwrite whatever is there".
- The four templates become the user-facing contract for the refresh; changing
  their wording requires updating the CLI tests, `--help`/docs where the
  refresh is described, and this ADR.
- Standalone `cypherlite skill install` / `skill remove` behavior is unchanged
  (LABS-84's marker refusal and `--force` requirement still guard it), so
  scripts and users that invoke the skill command directly see no difference.

## Later references

- [ADR-0262](./0262-skill-install-adopts-recognizable-bundle.md): the parent-side classification this ADR relies on left a
  cross-process TOCTOU window — the refresh classified a directory then spawned
  `skill install --force`, and the child trusted `--force` without re-deriving
 ownership. [ADR-0262](./0262-skill-install-adopts-recognizable-bundle.md) moves the recognizable-bundle decision into the replacing
  process: `skill install` adopts a recognized bundle itself, the refresh stops
  passing `--force`, and one shared recognizer lives in `crate::skill` so
  `skill_cmd` and `upgrade_cmd` cannot drift. `skill remove` stays strict.
- [ADR-0632](./0632-skill-refresh-scoped-to-installed-agents.md): the §2 auto-update spawn is scoped — the one unfiltered `skill install` spawn now runs only when every non-foreign target is installed; a mixed set (installed and missing together) spawns `skill install --agent <name>` per installed agent and leaves the missing targets strictly untouched — their install path is §3's all-missing first-install flow or the explicit command, never the unfiltered child. The `--yes` help wording is aligned with its pinned first-install meaning.
