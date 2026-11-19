---
type: adr
area: surface
date: 2026-09-30
deciders: CypherLite maintainers
name: 0152-neo4j-kernel-version-reporting-policy
status: accepted
tags:
  - bolt
  - conformance
title: Neo4j Kernel version string reported by dbms.components
REFERENCES:
  - "[[adr/0142-browser-compat-record-shapes]]"
  - "[[adr/0143-browser-dbms-procedures]]"
SUPERSEDES:
  - "[[adr/0142-browser-compat-record-shapes]]"
---

# 0152. Neo4j Kernel version string reported by dbms.components

## Context

browser.neo4j.io's neo4j-sdk has a minimum-version gate that blocks the
Database information sidebar (Linear LABS-150). Verified against the deployed
bundle (`https://browser.neo4j.io/assets/src.CrPyDXpB.js`):

- The gate is fed **only** from
  `CALL dbms.components() YIELD versions, edition, name`, mapping
  `version = row('Neo4j Kernel').versions[0]`, stored as `neo4jVersion` in
  connection state and passed to the sdk factory
  `Tk({queryCypher, neo4jVersion})`. The Bolt HELLO server agent is only
  copied into result summaries, never into the gate.
- The gate is `if (semverCompare(neo4jVersion, '4.4.0') === -1) throw` where
  `semverCompare = compareLoose(coerce(a,{loose:true}), coerce(b,{loose:true}))`
  over bundled npm semver (`types.DVd7vXNM.js`); `coerce` strips
  `-prerelease`/`+build` suffixes. Verified with semver 7.8.5:
  `2026.09.0-cypherlite` coerces to `2026.9.0` → passes;
  `2026.10-cypherlite` → `2026.10.0` → passes; `4.4.0-cypherlite` → `4.4.0`
  → passes (boundary); the current `0.9.1` → throws (the bug). `coerce('')`
  throws `Invalid version`, so `versions[0]` must always be non-empty.
- Aura precedent, verified via neo4j-cli against a live Aura instance:
  Aura's `dbms.components` reports 'Neo4j Kernel' `versions[0]` =
  `5.27-aura`, edition enterprise — a two-part version plus hyphenated
  product suffix, and Browser special-cases `version.endsWith('-aura')`.

[ADR-0142](./0142-browser-compat-record-shapes.md) rejected claiming a real Neo4j kernel version (`>= 4.4.0`) as
dishonest, accepting Desktop's non-fatal "not officially supported" warning
as honest output. The browser.neo4j.io gate is harsher: it **throws** and
blocks the sidebar outright, so the honest `0.9.1` workspace version is no
longer tenable in the 'Neo4j Kernel' row. The honesty objection is addressed
differently here: the reported string carries a hyphenated `-cypherlite`
product suffix, keeping it distinguishable from any real Neo4j version and
matching Aura's own `-aura` convention (Aura, a Neo4j-operated service,
itself reports a suffixed kernel version that is not a stock Neo4j release).
The 'CypherLite' row in `dbms.components` ([ADR-0142](./0142-browser-compat-record-shapes.md), row 3) keeps reporting
the honest workspace version, so no information is hidden.

Risk weighed: a coerced version `>= 5.x` trips other feature gates in the
same bundle (`Mx(e,'5.11.0')`, `Mx(e,'5.26.0')` gte checks in
`types.DVd7vXNM.js`) that may enable query paths CypherLite doesn't support.
Against that: CypherLite passes 3891/3893 openCypher TCK scenarios and
already implements the Browser/Desktop metadata surface ([ADR-0142](./0142-browser-compat-record-shapes.md),
[ADR-0143](./0143-browser-dbms-procedures.md)), any path a `2026.x` version enables can be fixed forward, and
staying at `4.4.x` forever lags Neo4j's own calver world (Neo4j itself now
reports versions like `2026.08.0`).

## Decision

`dbms.components` reports the 'Neo4j Kernel' row ([ADR-0142](./0142-browser-compat-record-shapes.md) row 1) with
`versions[0]` = **`2026.10-cypherlite`** — the current Neo4j calver
month plus the `-cypherlite` product suffix, the same two-part-plus-suffix
shape as Aura's `5.27-aura`.

- The string is a **policy constant**, not derived from the workspace
  version: it tracks the Neo4j calver month the compat surface was verified
  against, and is bumped deliberately (with a re-check of the Browser bundle
  gates) rather than automatically.
- `versions[0]` is always non-empty (`coerce('')` throws in the gate).
- The 'CypherLite' row keeps reporting the honest workspace version; the
  'Cypher' row is unchanged.
- The Bolt HELLO server agent string is **unchanged**: the gate reads only
  `dbms.components`, and the HELLO agent is only copied into result
  summaries.

## Alternatives considered

- **Minimal `4.4.0-cypherlite`.** Passes the sidebar gate exactly at the
  boundary and trips nothing else in the bundle (no `5.11.0`/`5.26.0`
  `Mx` checks fire). Rejected: it freezes CypherLite's reported version at
  a 2021 Neo4j release forever, while Neo4j itself has moved to calver; it
  is inconsistent with Aura's self-reporting convention (which reports a
  current version with a product suffix, not a pinned minimum); and the
  `>= 5.x` feature-gate risk it avoids is manageable — those gates enable
  query paths that can be fixed forward against a TCK that already passes
  3891/3893 scenarios.
- **Keep the honest workspace version (`0.9.1`).** Rejected: the
  browser.neo4j.io sdk gate throws on `< 4.4.0` and blocks the Database
  information sidebar outright (LABS-150); unlike Desktop's non-fatal
  warning, there is no continue path.
- **Claim a real, unsuffixed Neo4j version (e.g. `2026.10`).** Rejected:
 dishonest per [ADR-0142](./0142-browser-compat-record-shapes.md)'s reasoning — indistinguishable from a genuine
  Neo4j kernel and invites parity expectations the engine does not meet.
  The `-cypherlite` suffix is what makes the calver form acceptable.
- **Bare calver month without suffix (`2026.10-cypherlite` vs.
  `2026.10.0`).** The three-part unsuffixed form was rejected for the same
  honesty reason; the two-part suffixed form follows Aura's `5.27-aura`
  shape exactly and coerces cleanly (`2026.10.0`) through the gate.

## Consequences

- browser.neo4j.io's `>= 4.4.0` sdk gate passes and the Database information
  sidebar populates (LABS-150).
- Coerced to `2026.10.0`, the reported version is `>= 5.x`, so Browser
  feature gates at `5.11.0`/`5.26.0` (`Mx` checks) may enable query paths
  CypherLite does not yet support; those are fixed forward as they surface,
  tracked as bugs against the TCK/compat surface.
- The reported constant must be re-verified against the deployed Browser
  bundle when bumped; the bundle hash in Context is the evidence snapshot
  for this decision.
- [ADR-0142](./0142-browser-compat-record-shapes.md)'s `dbms.components` row table is superseded for row 1's
  `versions` value only; rows 2–3 and the SHOW DATABASES / `dbms.info`
  shapes are unchanged. Tests pinning row 1's version must be updated by the
  implementing node.
