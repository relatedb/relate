---
type: adr
area: infra
claim: d8d1efa08e27f89965fed1ede42388ba40950b9b
name: 0616-npm-platform-file-links-repo-pins-at-pack
status: proposed
tags:
  - release
title: |-
  npm platform packages resolve as file: links in the repository, registry pins injected at pack time
REFERENCES:
  - "[[adr/0278-npm-distribution]]"
  - "[[adr/0562-npm-platform-package-names]]"
  - "[[adr/0580-post-release-workspace-versioning]]"
---

# 0616: npm platform packages resolve as file: links in the repository, registry pins injected at pack time

## Context

The JavaScript client delivers its native library through five
`@relate/relate-<platform>-<arch>` optionalDependencies ([ADR-0562](./0562-npm-platform-package-names.md)), each
pinned at the release version. The registry only holds that version
after the release publishes — while every commit on `main` names the
next, unpublished one ([ADR-0580](./0580-post-release-workspace-versioning.md)'s advance). The release sweep
regenerated the lockfile with `npm install --package-lock-only` and
relied on optional-dependency install tolerance to carry the
unpublishable pins — but `npm ci` has no such tolerance: it hard-fails
on a lockfile without resolvable entries, so `npm ci` from a repository
checkout was broken at every point of the release cycle, by design, and
nobody noticed because the repository's own lanes never ran it.

## Decision

The repository manifest and the published manifest are now two shapes,
deliberately:

- The repository's `clients/javascript/package.json` resolves the five
  platform packages as `file:platforms/<dir>` links to the in-repo
  platform directories. A local link resolves at every commit, so the
  lockfile is always complete and `npm ci` holds from any checkout,
  regardless of what the registry holds. The links carry no version:
  nothing in the repository manifest needs the release bump, so the
  sweep's version rewrite has nothing to mis-bump there — the class of
  drift that produced the broken window cannot recur.
- The published package pins the registry packages at the release
  version. `scripts/lib/npm_json.py`'s `pin` verb (the one
  implementation — the same module behind every release-sweep read)
  rewrites the platform links' values to the checkout's own version
  immediately before the release workflow's `npm pack`, and the pack
  fails closed if the packed manifest carries any surviving `file:`
  link. The pins are correct by construction — derived from the
  same release bump that versions the platform packages — instead of
  being maintained by hand or by sweep.
- The release sweep verifies the repository shape, not values: its
  typed reads (`npm_json.py file-links` / `lock-file-links`) count the
  five links and refuse a versioned `@relate` pin in the repository
  manifest loudly — the exact shape the broken window came from.

## Alternatives considered

- Document the broken window (consumers unaffected, `npm install`
  works): rejected — the state contradicts `npm ci`'s contract from
  every checkout, and the fix turned out to be smaller than the caveat.
- `publishConfig.optionalDependencies` carrying the registry pins:
  rejected — npm does not apply publishConfig overrides to dependency
  maps; the packed manifest would ship the `file:` links (verified
  empirically against npm 11).
- Version ranges in the published optionalDependencies (always
  resolvable, no injection): rejected — consumers would resolve the
  newest 1.x platform package rather than the release's matched one,
  trading the exact-pin contract for install-time convenience the
  injection already provides.

## Consequences

Repository checkouts and CI can run `npm ci` at every commit. The
release pipeline carries one new step (the injection, with its
fail-closed verification) and the platform loader's development
behavior is unchanged: the in-repo platform directories are
manifest-only, so the `file:` links resolve packages without native
libraries and the loader's existing fallback order (RELATE_LIB, then
the platform package, then the repository build) serves local
development exactly as before. The published consumer contract is
byte-identical to the previous design: exact registry pins at the
release version. [ADR-0278](./0278-npm-distribution.md) and [ADR-0562](./0562-npm-platform-package-names.md) stand; this ADR narrows their
optionalDependencies wiring to the two-shape rule above.
