---
type: adr
area: cli
claim: 63d85d5662afbd985fa163f1f90095f5f27b20d2
date: 2026-11-20
deciders: Relate maintainers
name: 0632-skill-refresh-scoped-to-installed-agents
status: accepted
tags:
  - skill
title: Post-upgrade skill refresh is scoped to installed agents
REFERENCES:
  - "[[adr/0261-upgrade-skill-refresh-ownership]]"
  - "[[adr/0262-skill-install-adopts-recognizable-bundle]]"
---

# 0632. Post-upgrade skill refresh is scoped to installed agents

## Context

[ADR-0261](./0261-upgrade-skill-refresh-ownership.md) gives the post-upgrade
skill refresh a consent matrix: an installed bundle (Owned or Legacy)
auto-updates with no prompt, while an all-missing set installs only with
consent — a `[Y/n]` prompt interactively, `--yes` anywhere, a hint and
nothing else non-interactively.

The auto-update branch spawns one plain `skill install` whenever no Foreign
target coexists, and that child installs into **every detected agent**, not
only the ones carrying the bundle. So on a machine where the skill is
installed for one agent, the next `relate upgrade` installs it into every
other detected agent with no prompt at all — the any-installed branch
short-circuits the consent matrix the same decision defines. A partial
install silently becomes a full one.

Separately, the `--yes` help line — "Auto-accept the post-upgrade skill
refresh" — undersells the pinned meaning: [ADR-0261](./0261-upgrade-skill-refresh-ownership.md)'s
consequences define `--yes` as *install the skill when none is installed
yet*, which "refresh" does not convey. A user reading the flag cannot tell
that headless `--yes` installs a bundle that was never there.

## Decision

**The auto-update spawn set covers installed targets only.** When the
non-foreign set is mixed — at least one Installed and at least one Missing
target — the unconditional spawns are one `skill install --agent <name>` per
**installed** target. The single unfiltered spawn survives only where it
cannot reach anything the plan did not already decide: every non-foreign
target installed (an update-only run), or every non-foreign target missing
with consent just granted (the all-missing install). A Foreign target
coexisting keeps [ADR-0261](./0261-upgrade-skill-refresh-ownership.md)'s
per-agent fallback for both partitions.

**A mixed set touches nothing but the installed targets.** The set of
agents carrying the skill is the user's curation: `upgrade` maintains it
and never expands it. When at least one target is installed, the missing
targets are left strictly untouched — no prompt, no install, no hint,
`--yes` included. The consent matrix ([ADR-0261](./0261-upgrade-skill-refresh-ownership.md) §3)
survives unchanged for the one set shape it was designed for: nothing
installed anywhere — the first-install flow (a `[Y/n]` prompt
interactively, `--yes` anywhere, a hint and nothing else
non-interactively). Agents therefore gain the skill only through that
flow or an explicit `relate skill install`.

**`--yes` says what it does.** The help line and the spec row state the
first-install meaning — install when no detected agent has the skill,
installed bundles updating regardless — instead of "refresh".

## Alternatives considered

- **Keep the unfiltered spawn (normalize to every detected agent).**
  Rejected: it installs into agents the user never opted into — the exact
  silent install the consent matrix exists to prevent, reachable through
  the one branch that bypasses it.
- **A flag selecting the scope (e.g. `--skill all|installed`).** Rejected:
  new public CLI surface to preserve a behavior that contradicts the
  design; nobody has asked to keep it.
- **Ask about the missing agents in mixed sets (this change's first
  cut).** Rejected: the installed set is deliberate curation — prompting
  about agents the user left without the skill is noise, and the prompt
  is reserved for the fresh machine that has nothing installed anywhere.
- **Never install missing targets from inside `upgrade`.** Rejected as
  stated: the fresh machine with nothing installed anywhere is the common
  first-run case the `[Y/n]` default serves; its consent flow stays.
  Mixed sets adopt the position in the narrower form above — untouched,
  not installed.

## Consequences

- A partial install stays partial: each not-yet-installed agent is reached
  only through the first-install flow (nothing installed anywhere) or an
  explicit `relate skill install`.
- Mixed upgrades print nothing about the missing agents — no prompt, no
  hint, `--yes` included: the previous release's silent full install of
  them becomes a silent no-op.
- The per-agent spawn machinery [ADR-0261](./0261-upgrade-skill-refresh-ownership.md)
  reserved for foreign conflicts becomes the general mixed-set path;
  message templates, the no-`--force` rule, and the exit-0 guarantee are
  unchanged.
- [ADR-0261](./0261-upgrade-skill-refresh-ownership.md) §2's "one plain
  `skill install` spawn" now carries the every-target-installed qualifier
  this ADR adds.
