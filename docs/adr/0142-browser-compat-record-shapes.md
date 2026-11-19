---
type: adr
area: surface
date: 2026-09-29
deciders: CypherLite maintainers
name: 0142-browser-compat-record-shapes
status: accepted
tags:
  - bolt
  - conformance
title: Browser/Desktop-compat record shapes for dbms.components, SHOW DATABASES, and dbms.info
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0152-neo4j-kernel-version-reporting-policy]]"
  - "[[adr/0266-bolt-websocket-transport]]"
---

# 0142. Browser/Desktop-compat record shapes for dbms.components, SHOW DATABASES, and dbms.info

- **Later references**: [ADR-0152](./0152-neo4j-kernel-version-reporting-policy.md) supersedes row 1's `versions` value: the
  'Neo4j Kernel' row now reports a calver string with a `-cypherlite`
  product suffix (`2026.10-cypherlite`, following Aura's `5.27-aura`
  convention) so browser.neo4j.io's throwing `>= 4.4.0` sdk gate passes.
  The hyphenated suffix keeps the string distinguishable from a real Neo4j
  version, addressing this ADR's honesty objection; the 'CypherLite' row
  still reports the honest workspace version.

## Context

A user tested the Bolt-over-WebSocket transport (PR #144, [ADR-0266](./0266-bolt-websocket-transport.md)) from the
two Neo4j Browser hosts — the public web Browser and Neo4j Desktop 2.x's
embedded Browser — and hit a series of engine-side failures that blocked the
connection:

1. **The version check fails on a missing `edition` column.** On connect the
   Browser runs `CALL dbms.components() YIELD name, versions, edition`.
   CypherLite's `dbms.components` returned one row,
   `{name: 'Cypher', versions: ['5', '25']}`, with no `edition` output, so the
   engine rejects the query with `UnknownProcedureOutput`. This gap was
   already filed as
   [LABS-139](https://linear.app/neo4j/issue/LABS-139). The public web
   Browser parses `versions[0]` of the **first** record with
   `semver.clean`/`semver.valid`, falling back to `semver.coerce`
   (`'5'` → `5.0.0`), and gates on `>= 4.4.0`; the `['5', '25']` versions
   already pass, so the missing `edition` column is the only failure there.
   The public neo4j-browser swallows this error (it warns and continues);
   Neo4j Desktop's record codec does not.

2. **Desktop's record codec rejects the SHOW DATABASES rows.** The original
   rows carried `null` for `access`, `address`, `role`, `writer`,
   `requestedStatus`, `currentStatus`, `statusMessage` and `default`.
   Desktop's codec (the `neo4j/upx` record decoder, which is private)
   rejects them with `Invalid database record received from the server` and
   refuses the connection.

3. **Desktop finds the version row by name, not by position.** Desktop's
   bundled assets (`assets/src.<hash>.js`, quoted verbatim below; the upx
   codec itself remains private) show the version check thunk
   `connections/updateVersionAndEdition` runs
   `CALL dbms.components() YIELD versions, edition, name;` with a mapped
   transformer:

   ```js
   let n=e.map(e=>e.toObject()), r=n.find(e=>e.name==='Neo4j Kernel'), i=n.find(e=>e.name==='Cypher'), a=n.find(e=>e.name==='Virtual Graph'); return {version: r?.versions[0] ?? '', edition: r?.edition ?? 'community', cypherVersions: i?.versions, virtualGraphVersion: a?.versions[0] ?? null, ...}
   ```

   With no `'Neo4j Kernel'` row, `version` is `''` → `Invalid version: ` →
   the user's "Failed to check Neo4j version" warning. On success Desktop
   compares the version against `4.4.0`; a lower version dispatches a
   NON-fatal warning ("Neo4j \<v\> is not officially supported").

4. **Desktop builds the instance id from name@address and rejects an empty
   address:**

   ```js
   hN(e){let t=`${e.name}@${e.address}`; return mie(t), t}
   ```

   with `pie` requiring both parts non-empty. An empty `address` yields
   `neo4j@` → uncaught `Invalid database instance name: neo4j@` (twice in
   the user's console).

5. **Desktop calls `CALL dbms.info();`** (`updateDBMSInfo`) expecting
   `{id, name, creationDate, ...}` (trial fields optional). CypherLite had
   no such procedure, so this was `ProcedureNotFound` → Neo4jError console
   noise (handled, non-fatal).

6. Desktop also calls `dbms.clientConfig` (`ProcedureNotFound`,
   handled/logged) — this stays a follow-up (LABS-141).

This ADR deliberately extends the AGENTS.md catalog-theater rule ("Neo4j-only
catalog theater … is out of the merge gate unless a later ADR brings it
back") for these surfaces: the shapes below exist for **Browser/Desktop
compatibility**, not Neo4j feature parity. Bolt remains a transport
([ADR-0057](./0057-embedded-c-abi-and-bolt.md)). The namespaces the rows describe are the fixed namespaces of
[ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md).

**Source-cited vs. inferred.** Source-cited: the public Browser's version
check and first-record read come from `neo4j-browser@neo4j-2026.08.0`
(`dbMetaDuck.ts:69-70` for the `dbms.components` query; `utils.ts:34-64` for
`extractServerInfo`'s first-record `versions[0]` parse), the SHOW
DATABASES column set from the Neo4j operations manual, and Desktop's
by-name lookups from the bundle quotes in items 3–5 above. Inferred: the
upx codec is private, so its accepted union members are inferred from the
error text plus the Neo4j docs — `access`: string `read-write|read-only`;
`address`: string (null rejected; the union likely admits `undefined`);
`role`: string `primary|secondary|unknown` (null rejected);
`requestedStatus`: string `online|offline`; `currentStatus`: string
(`online`, `offline`, `starting`, `stopping`, `store copying`, `initial`,
`deallocating`, `dirty`, `quarantined`, `unknown`, `mixed`); `default`:
boolean; `writer` and `statusMessage` are tolerated as null. The
Desktop-only `>= 4.4.0` gate is likewise inferred to use the same parse.

## Decision

1. **`dbms.components` returns three rows, in this order:**

   | # | name | versions | edition |
   |---|---|---|---|
   | 1 | `Neo4j Kernel` | `[<workspace version, e.g. '0.9.1'>]` | `community` |
   | 2 | `Cypher` | `['5', '25']` | `community` |
   | 3 | `CypherLite` | `[<workspace version>]` | `community` |

   Row 1 carries **both** reader roles: Desktop finds the version row **by
   name** (`n.find(e=>e.name==='Neo4j Kernel')`, evidence 3 above), while the
   public web Browser reads the **first** record (`extractServerInfo` takes
   `versions[0]` of record 0, `neo4j-browser@neo4j-2026.08.0`
   `utils.ts:34-64`). Row 1 must therefore be first **and** named
   `Neo4j Kernel`. Row 2 keeps its existing name and versions and feeds
   Desktop's `cypherVersions` (found by name `Cypher`). Row 3 identifies
   CypherLite to consumers that iterate all rows. `edition` is `'community'`
 on all rows: CypherLite has no enterprise tier (matching [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md)'s
   Community Edition semantics).

   **Deliberate consequence:** the workspace version (e.g. `0.9.1`) is below
   Desktop's `4.4.0` gate, so Desktop shows the **non-fatal** "not
   officially supported" warning. That warning is accepted as honest
   output; claiming a `>= 4.4.0` version was rejected (see Alternatives).

2. **SHOW DATABASES rows carry concrete values** in both the `neo4j` and
 `system` namespaces ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md)):

   | field | neo4j | system |
   |---|---|---|
   | `access` | `'read-write'` | `'read-write'` |
   | `address` | `'localhost:7687'` | `'localhost:7687'` |
   | `role` | `'primary'` | `'primary'` |
   | `requestedStatus` | `'online'` | `'online'` |
   | `currentStatus` | `'online'` | `'online'` |
   | `statusMessage` | `''` | `''` |
   | `writer` | `true` | `true` |
   | `default` | `true` | `false` |
   | `home` | `true` (unchanged) | `false` (unchanged) |

   `name`, `type`, `aliases` and `constituents` are unchanged. `address` is
   the constant `'localhost:7687'`: Desktop builds the instance id
   `name@address` and rejects an empty address (evidence 4 above), and the
   engine does not know the Bolt bind address, so the value is a
   display/instance-id placeholder — correct for the default Bolt bind and
   the published Docker port, cosmetic-only otherwise.

3. **`dbms.info()` returns one row**
   `{id: 'cypherlite', name: 'system', creationDate: '1970-01-01T00:00:00Z'}`.
   `name` is the constant `'system'`: the procedure arm sees only the graph,
   not the session's current database, and the engine is single-database.
   `creationDate` is a documented placeholder: the engine tracks no
   creation time, and the Desktop UI merely displays the fields (evidence 5
   above).

## Alternatives considered

For decision 1 (`dbms.components`):

- **Keep row 1 as `Cypher`.** Rejected: Desktop finds the version row by
  name, so no `Neo4j Kernel` row means an empty version — the observed
  "Failed to check Neo4j version" failure.
- **Claim a real Neo4j kernel version (`>= 4.4.0`).** Rejected: dishonest,
  and a `>= 5.3`/`5.7` version would enable extra Desktop query paths
  (`Qte`/`Zte` in the bundle) that CypherLite does not support. The honest
  sub-4.4 version costs only a non-fatal warning. (An earlier form of this
  alternative — Neo4j-kernel name *and* versions — was also theater beyond
  need: the public Browser's `>= 4.4.0` gate already passes on
  `['5', '25']`, and claiming to be the Neo4j kernel invites deeper parity
  expectations the engine does not meet.)
- **Identify CypherLite by renaming an existing row.** Rejected: Desktop
  still finds no `Neo4j Kernel` row, so the version check still fails, and
  anything string-matching `'Cypher'` on its row could break; an additive
  third row is safer.

For decision 2 (SHOW DATABASES, including `address`):

- **Rewrite the records in the Bolt layer** (with or without the
  connection's dialed address). Rejected: the shape is an engine-level
  contract; the CLI and the Rust API must see the same rows as Bolt
  clients, and intercepting and patching result records in the transport is
  too fragile for a display field.
- **Engine knowledge of the bind address.** Rejected: layering — the engine
  has no network identity.
- **Omit `address` or leave the fields null and document a Browser
  limitation.** Rejected: Desktop's codec/`hN` requires a non-empty string,
  and its record codec blocks the connection outright on nulls, so the
  limitation is not cosmetic — the Browser simply cannot connect.

For decision 3 (`dbms.info`):

- **Omit the procedure.** Rejected: `ProcedureNotFound` produces Neo4jError
  console noise and blank DBMS info in Desktop; the procedure is cheap.
- **Return a real creation timestamp.** Rejected: the engine has no
  creation-time tracking; adding it for a display field is scope creep.

## Consequences

- Neo4j Desktop's embedded Browser and the public web Browser connect over
 the [ADR-0266](./0266-bolt-websocket-transport.md) transport: the version checks see `edition` and find a
  `Neo4j Kernel` row, the record codec accepts the SHOW DATABASES rows, the
  instance id `neo4j@localhost:7687` validates, and `dbms.info()` answers.
- Desktop shows a **non-fatal** "not officially supported" warning because
  the workspace version is below `4.4.0`. This is expected and honest;
  documentation for the Bolt/Desktop setup must say so.
- The engine's SHOW DATABASES rows now carry concrete operational-looking
  values that are **fixed constants**, not live status: CypherLite is a
  single-node embedded engine, so every namespace is always
  `online`/`primary`/`read-write`. API documentation must say so.
- Any consumer that iterates all `dbms.components` rows sees CypherLite
  identified on row 3; consumers that read only row 1 get an honest engine
  version.
- `--advertised-address`-style accuracy for `address` stays a non-goal: the
  field is a fixed placeholder constant.
- CIP-eval/TCK scenarios that pin the old nulls or the old single-row
  `dbms.components` shape must be updated by the implementing nodes; the
  scoreboard only moves up. Implementing nodes must add tests for
  `dbms.info()`.
- LABS-139 is resolved by the `edition` output; `dbms.clientConfig` and
  related procedures remain follow-ups (LABS-140/141/142/143).
