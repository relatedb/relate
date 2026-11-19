---
type: adr
area: cli
date: 2026-09-26
deciders: CypherLite maintainers
name: 0262-skill-install-adopts-recognizable-bundle
status: accepted
tags:
  - skill
title: Skill install adopts a recognizable cypherlite bundle
REFERENCES:
  - "[[adr/0261-upgrade-skill-refresh-ownership]]"
---

# 0262. Skill install adopts a recognizable cypherlite bundle

- **Renumbered**: from 0111 (shared-number cleanup)

## Context

[ADR-0261](./0261-upgrade-skill-refresh-ownership.md) taught the post-upgrade skill refresh to recognize an **unmarked**
cypherlite bundle by the `name: cypherlite` **and** `version:` lines in its
`SKILL.md` frontmatter, and calls such a directory **Legacy**. That decision
kept the standalone `cypherlite skill install|remove` command strict: its
`ensure_replaceable` in `surfaces/cypherlite-cli/src/skill_cmd.rs` still
refused any directory without the `.cypherlite-installed` marker unless
`--force` was passed.

The threat model `a110706b-3126-4b3d-823e-96295b949da8` for this branch filed
`OPLANE_REQ-00263246` (a TOCTOU finding) plus the failing cases of
`OPLANE_REQ-00263241`/`245`. The refresh classified each skill directory
**once**, in the parent (`surfaces/cypherlite-cli/src/upgrade_cmd.rs`), then
spawned `cypherlite skill install --force`. `ensure_replaceable` short-circuits
on `force` (`surfaces/cypherlite-cli/src/skill_cmd.rs`), so a directory the
parent classified as **Legacy** could be swapped between the classification and
the child's write; the child, trusting `--force`, replaced whatever was there.
The ownership decision and the replacement were split across two processes, and
the second process re-derived nothing. `OPLANE_REQ-00263242` (an unbounded
`SKILL.md` read in the classifier) is a separate concern, fixed separately by
LABS-123.

The root cause is that the **replacing** process — `skill install` — never
validated what it was about to replace. The fix is to move the ownership
decision into that process: have `skill install` itself accept a recognizable
cypherlite bundle so the child re-validates at the moment of use, and stop the
upgrade refresh from forcing at all.

## Decision

**`skill install`'s replace check adopts a recognizable cypherlite bundle.** A
destination is replaceable without `--force` when it is any of:

1. **missing** — the destination does not exist (`NotFound`), so it is safe to
   create fresh. An unreadable or otherwise-uninspectable destination is
   **not** missing: any metadata error other than `NotFound` is refused without
 `--force` (fail closed, matching [ADR-0261](./0261-upgrade-skill-refresh-ownership.md)'s `Foreign` classification);
2. **marker-owned** — it carries `.cypherlite-installed` as a real regular
   file (symlinks not followed), the existing ownership proof; or
3. **a recognizable cypherlite bundle** — no marker, but `<dir>/SKILL.md`
   exists and its YAML frontmatter declares `name: cypherlite` **and**
   contains a `version:` line. The version value may be a concrete version or
   the literal `{{VERSION}}` placeholder; either counts as a machine-written
   cypherlite bundle.

`--force` keeps its existing meaning: the user explicitly requested a clobber,
so it replaces any destination without further checks. A missing directory and
a plain `--force` are still allowed through exactly as before.

**`skill remove` keeps the strict marker refusal.** Removal has no
replacement bundle to fall back on, so accepting a recognizable-but-unmarked
directory would delete a directory that might be user-authored with no way to
recover it. Only a marker-owned directory (or `--force`) is removable.

**The upgrade refresh stops passing `--force`.** With the install-side check
now recognizing Legacy bundles, the refresh no longer needs to force: it
spawns `cypherlite skill install` for the recognized targets without `--force`,
and the child re-derives "this is ours" immediately before it deletes and
recreates the directory. That is the TOCTOU fix — the decision and the write
now happen in the same process, at the same instant.

**One shared recognizer lives in `crate::skill`.** The recognizable-bundle
predicate (`name: cypherlite` plus a `version:` line, read from `SKILL.md`
frontmatter) moves to `surfaces/cypherlite-cli/src/skill/` as a single shared
function. Both `skill_cmd`'s install-side replace check and `upgrade_cmd`'s
classifier call it, so the two call sites cannot drift. [ADR-0261](./0261-upgrade-skill-refresh-ownership.md) already made
the marker rules shared, load-bearing policy; this decision extends the same
rule to the bundle recognizer.

Recognizing a bundle is **identification**, not a staleness gate: an install
always replaces the destination it recognized. `skill install` is invoked
either directly by a user or by the refresh after a successful binary
self-replace, and in both cases the incoming bundle is the one that should win.

## Alternatives considered

- **Add a `skill install --adopt-legacy` flag.** Rejected: it introduces new
  public CLI surface for a decision the install path can already infer safely
  from the bundle's own `SKILL.md`; the migration path is exactly the one that
  should be automatic, and a new flag would have to be threaded through the
  refresh spawn anyway.
- **Use a hidden environment-variable switch.** Rejected: it is
  undiscoverable and unauditable, and the project documents every honored
  environment variable; a hidden switch would be an undocumented, untestable
  ownership bypass rather than a policy.
- **Re-check the directory in the parent only (before spawning the child).**
  Rejected: it does not close the cross-process window — the child still trusts
  the parent's decision, so a directory swapped between the parent's check and
  the child's write is still replaced. The check must run in the process that
  performs the replacement.
- **Waive the finding as accepted risk.** Rejected: the fix is small and keeps
  the semantics coherent — the recognizable-bundle recognizer already had to
 exist for [ADR-0261](./0261-upgrade-skill-refresh-ownership.md), so moving it into the replacing process adds no new
  concept, and accepting a known TOCTOU in an ownership check would leave the
  guard weaker than its own documentation claims.

## Consequences

- `skill install` re-validates ownership in the process that replaces the
  directory, so a directory swapped between the refresh's classification and
  the child's write is no longer force-replaced; the TOCTOU window
  (`OPLANE_REQ-00263246`) is closed for the install path.
- The upgrade refresh passes no `--force`: the child adopts a recognized Legacy
 bundle on its own. The user-visible outcome of [ADR-0261](./0261-upgrade-skill-refresh-ownership.md)'s legacy adoption is
  unchanged, but the mechanism is now install-side rather than parent-side.
- `skill remove` behavior is unchanged: an unmarked directory is still refused
  without `--force`.
- The recognizable-bundle predicate becomes a single shared function in
  `crate::skill`, called by both `skill_cmd` and `upgrade_cmd`; changing the
  `name: cypherlite` + `version:` convention now requires updating that one
 function, its callers, the CLI tests, and this ADR plus [ADR-0261](./0261-upgrade-skill-refresh-ownership.md).
- `--force` remains the explicit user-requested clobber, and is now the only
  way to replace a directory that is neither marker-owned nor recognizable.
- `skill install` replaces a recognized bundle without a prompt, consistent
 with [ADR-0261](./0261-upgrade-skill-refresh-ownership.md)'s "recognized is ours to update" rule; foreign content is still
  reported and left untouched.
