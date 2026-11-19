---
type: adr
area: surface
date: 2026-09-30
deciders: CypherLite maintainers
name: 0143-browser-dbms-procedures
status: accepted
tags:
  - bolt
  - conformance
title: Browser/Desktop-compat shapes for dbms.showCurrentUser, dbms.clientConfig, dbms.listConfig, and dbms.licenseAgreementDetails
REFERENCES:
  - "[[adr/0055-apache-2-license]]"
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0138-bolt-procedure-not-found-code]]"
  - "[[adr/0142-browser-compat-record-shapes]]"
  - "[[adr/0266-bolt-websocket-transport]]"
  - "[[adr/0273-adr-session-plumbing]]"
  - "[[adr/0497-catalogue-corpus-alignment]]"
---

# 0143. Browser/Desktop-compat shapes for dbms.showCurrentUser, dbms.clientConfig, dbms.listConfig, and dbms.licenseAgreementDetails

## Context

[ADR-0142](./0142-browser-compat-record-shapes.md) made the public web Browser and Neo4j Desktop connect over the
[ADR-0266](./0266-bolt-websocket-transport.md) transport and left four `dbms.*` procedures as follow-ups
(LABS-140/141/142/143, consolidated under
[LABS-148](https://linear.app/neo4j/issue/LABS-148), which delegates the
per-procedure honest-vs-placeholder call to this ADR). This ADR pins the
record shapes for the remaining four, extending the AGENTS.md
catalog-theater rule exactly as [ADR-0142](./0142-browser-compat-record-shapes.md) did: these procedures exist for
**Browser/Desktop compatibility**, not Neo4j feature parity; Bolt remains a
transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)).

Two research inputs pin what the clients actually send and read:

1. **neo4j-browser@neo4j-2026.08.0 source citations** (Neo4j Browser
   2025.8.0/2026.08.0 and Neo4j Desktop 2.x's embedded Browser):

   - `dbms.showCurrentUser` — sent only when the client-side `authEnabled`
     state is true (`currentUserDuck.ts:96-99`) and the server passes the
     multi-db gate (`versionedFeatures.ts:38-48`). The Browser reads **only**
     `username` and `roles`; both are optional (missing `username` → `'-'`,
     missing `roles` → `['admin']`, `currentUserDuck.ts:118-125`), `flags`
     is **not read**, and any error — including `ProcedureNotFound` — is
     swallowed with user state cleared (`currentUserDuck.ts:111-113, 117`).
   - `dbms.clientConfig` / `dbms.listConfig` — records are reduced via
     `record.get('name')`/`record.get('value')` **only**
     (`dbMetaEpics.ts:525-526`); `cleanupSettings` then reads the keys
     `browser.allow_outgoing_connections`, `browser.credential_timeout`,
     `browser.post_connect_cmd`,
     `browser.remote_content_hostname_whitelist`,
     `browser.retain_connection_credentials`, `browser.retain_editor_history`,
     `clients.allow_telemetry` + `client.allow_telemetry`,
     `dbms.security.auth_enabled` (the string `browser.auth_enabled` does
     not exist in the source), `metrics.namespaces.enabled` (pre-5.0) and
     `metrics.prefix` / `server.metrics.prefix`
     (`dbMetaEpics.ts:562-600`). The clientConfig → listConfig fallback is
     keyed on the exact FAILURE code
     `Neo.ClientError.Procedure.ProcedureNotFound`
 (`dbMetaEpics.ts:491-506`) — the code [ADR-0138](./0138-bolt-procedure-not-found-code.md) already emits at the
     Bolt boundary; any other error marks server config unreadable
     (`dbMetaEpics.ts:513-518`).
   - `dbms.licenseAgreementDetails` — **confirmed never sent to a
     community-edition server**: `fetchTrialStatus` guards on
     `version && enterprise` where `enterprise` is the server-reported
     edition (`dbMetaEpics.ts:292-299`), and additionally requires semver
 `>= 5.7.0` (`dbMetaDuck.ts:72-73`). [ADR-0142](./0142-browser-compat-record-shapes.md) pins `edition:
     'community'` on every `dbms.components` row, so no current client ever
     calls this procedure against CypherLite.

2. **Neo4j reference column sets** (docs- and source-cited):
   `dbms.showCurrentUser` returns `username`/`roles`/`flags`
   (<https://neo4j.com/docs/operations-manual/current/procedures/>);
   `dbms.clientConfig` (undocumented `@Internal`,
   `BuiltInDbmsProcedures.java` on github.com/neo4j/neo4j) and
   `dbms.listConfig` return the 8-column `ConfigResult` shape (`name`,
   `description`, `value`, `dynamic`, `defaultValue`, `startupValue`,
   `explicitlySet`, `validValues`; `ConfigResult.java` 5.25, unchanged
   through 2026.x), with the 5.x+ default clientConfig key allowlist
   `browser.allow_outgoing_connections`, `browser.credential_timeout`,
   `browser.retain_connection_credentials`, `browser.retain_editor_history`,
   `dbms.security.auth_enabled`,
   `browser.remote_content_hostname_whitelist`, `browser.post_connect_cmd`,
   `client.allow_telemetry`, `server.metrics.prefix`;
   `dbms.licenseAgreementDetails` returns
   `status`/`daysLeftOnTrial`/`totalTrialDays`, enterprise-only in Neo4j,
   with `status` one of `'yes'|'no'|'eval'|'expired'` and the integer
   columns null off-trial.

**The session-context question.** Auth in CypherLite is Bolt-layer only
([ADR-0126](./0126-bolt-basic-auth.md)): `surfaces/cypherlite-bolt/src/auth.rs`
(`AuthConfig::{None, Basic{user,password}}`) validates the HELLO token, and
the engine (`cypherlite::Database`/`Connection`) carries no user identity —
procedure arms see only the graph
(`cypherlite/cypherlite-executor/src/exec/procedures.rs`,
`builtin_procedure(graph, name)`). So `dbms.showCurrentUser` and the
`dbms.security.auth_enabled` config row cannot answer honestly without
plumbing the Bolt principal into the engine. That would be a new public
engine API (connection/session identity) with the full AGENTS.md §2 docs
burden across every surface — to serve a sidebar display field. [ADR-0142](./0142-browser-compat-record-shapes.md)
already established the documented-placeholder precedent for exactly this
class of field (`dbms.info` creationDate, SHOW DATABASES address) and
rejected both Bolt-layer record rewriting and engine knowledge of
transport-layer facts.

## Decision

**Session context is not plumbed.** The four procedures return fixed rows
from the engine, per the [ADR-0142](./0142-browser-compat-record-shapes.md) documented-placeholder precedent; the
rejected plumbing alternative is named under Alternatives. Implementation
follows the [ADR-0142](./0142-browser-compat-record-shapes.md) pattern: unlisted registry entries in
`cypherlite/cypherlite-executor/src/procedure_registry.rs`, exec arms in
`exec/procedures.rs`, tests in `cypherlite/cypherlite/tests/dbms_procedures.rs`.

1. **`dbms.showCurrentUser()` returns one row** with the Neo4j reference
   columns:

   | column | type | value | status |
   |---|---|---|---|
   | `username` | STRING | `'cypherlite'` | documented placeholder |
   | `roles` | LIST&lt;STRING&gt; | `['admin']` | documented placeholder |
   | `flags` | LIST&lt;STRING&gt; | `[]` | honest |

 `username: 'cypherlite'` is the [ADR-0126](./0126-bolt-basic-auth.md) default principal (the
   generated-token user), so it is accurate for the default deployment and a
   display-only placeholder under `--auth user:pw`; the Browser shows it in
   the sidebar and reads nothing else. `roles: ['admin']` matches the
   Browser's own missing-`roles` fallback and honestly conveys the engine's
   actual authorization model: there is none, so every connected principal
   can do everything. `flags: []` is honest — CypherLite has no user flags.
   Under `--auth none` the Browser's client-side `authEnabled` gate
   (decision 2's `dbms.security.auth_enabled` row) is what keeps this call
   cosmetic.

2. **`dbms.clientConfig()` returns the 8-column ConfigResult shape**
   (`name`, `description`, `value`, `dynamic`, `defaultValue`,
   `startupValue`, `explicitlySet`, `validValues`) with one row per key of
   the Neo4j 5.x+ default allowlist:

   | name | value |
   |---|---|
   | `browser.allow_outgoing_connections` | `'true'` |
   | `browser.credential_timeout` | `'0'` |
   | `browser.post_connect_cmd` | `''` |
   | `browser.remote_content_hostname_whitelist` | `'*'` |
   | `browser.retain_connection_credentials` | `'false'` |
   | `browser.retain_editor_history` | `'true'` |
   | `client.allow_telemetry` | `'false'` |
   | `dbms.security.auth_enabled` | `'true'` |
   | `server.metrics.prefix` | `'cypherlite'` |

   On every row: `description` is a short fixed string naming the key's
   Browser/Desktop effect, `dynamic: false`, `defaultValue` and
   `startupValue` equal `value`, `explicitlySet: false`,
   `validValues: ''`. **All rows are documented placeholders** except
   `server.metrics.prefix: 'cypherlite'` and `client.allow_telemetry:
   'false'`, which are honest (CypherLite exposes no Neo4j-style metrics
   namespace, and engine telemetry is off unless the operator opts in).
 `dbms.security.auth_enabled: 'true'` is accurate for the [ADR-0126](./0126-bolt-basic-auth.md)
   default (generated token) and wrong under `--auth none`; the cost is
   cosmetic — the Browser calls `dbms.showCurrentUser` and shows the
   placeholder user — and the alternative (plumbing) is rejected below. The
   Browser reads only `name`/`value`; the remaining six columns keep the
   Neo4j 5.x+ documented shape for other consumers. The
   `clients.allow_telemetry` (4.4) and `metrics.*` (pre-5.0) keys are
 omitted: [ADR-0142](./0142-browser-compat-record-shapes.md)'s version rows steer current clients down the 5.x+
   path, and the Browser treats missing keys as absent.

3. **`dbms.listConfig()` returns the same 8-column shape and the same nine
   rows as `dbms.clientConfig`.** The Browser only ever calls it as the
 `ProcedureNotFound`-keyed fallback ([ADR-0138](./0138-bolt-procedure-not-found-code.md) emits that code for
   unregistered procedures, but both procedures are registered here, so the
   fallback is for other clients and future-proofing) and post-processes it
   identically. Neo4j's `dbms.listConfig` exposes the full settings
   registry and accepts an optional `searchString` filter; CypherLite has no
   general settings registry, so the documented scope is the
   Browser-relevant subset, and no input arguments are accepted. Same
   honest/placeholder marking as decision 2.

4. **`dbms.licenseAgreementDetails()` returns one row**
   `{status: 'yes', daysLeftOnTrial: null, totalTrialDays: null}` — an
 **honest** constant. CypherLite is Apache-2.0 ([ADR-0055](./0055-apache-2-license.md)): there is no
   license agreement to accept and no trial, so the honest state is
   "accepted/valid" with null trial counters (the reference semantics for
   off-trial). No current client ever calls it — the Browser guards on
   server-reported `edition === 'enterprise'` plus semver `>= 5.7.0`, and
 [ADR-0142](./0142-browser-compat-record-shapes.md) reports `community` on every row — so the procedure is
   defensive completeness for future or non-Browser clients, not a
   Browser requirement.

Every placeholder value above must be named as such in `docs/api/bolt/`
(Browser/Desktop compatibility notes) per the [ADR-0142](./0142-browser-compat-record-shapes.md) documentation
consequence.

## Alternatives considered

Cross-cutting (session context):

- **Plumb the [ADR-0126](./0126-bolt-basic-auth.md) Bolt principal into the engine** (new public
  engine API: connection/session identity on `Database`/`Connection`, so
  `dbms.showCurrentUser` and `dbms.security.auth_enabled` answer
  per-connection). Rejected: disproportionate — a new public engine API
  with the AGENTS.md §2 docs burden (spec + description + examples across
  the rust, c, python, javascript, java and go surfaces, most of which are
  embedded and have no principal at all) to serve a sidebar display field
 and one config row; it also breaks the [ADR-0142](./0142-browser-compat-record-shapes.md) layering rule that the
 engine has no transport identity, and under the [ADR-0126](./0126-bolt-basic-auth.md) default the
  placeholders are already accurate. Honesty is preserved by documenting
 the placeholders, per the [ADR-0142](./0142-browser-compat-record-shapes.md) precedent.
- **Bolt-layer record rewriting** (intercept procedure results in the
 transport and patch in the principal). Rejected per the [ADR-0142](./0142-browser-compat-record-shapes.md)
  precedent: the shape is an engine-level contract, the CLI and the Rust
  API must see the same rows as Bolt clients, and intercepting and patching
  result records in the transport is too fragile for display fields.
- **Omit `dbms.showCurrentUser` and let `ProcedureNotFound` stand.**
  Rejected: the error is swallowed, but only after the client-side
  `authEnabled` gate passes — which decision 2's `auth_enabled: 'true'` row
  makes the common case — leaving a blank sidebar and console noise; the
  procedure is one fixed row.

For `dbms.clientConfig`/`dbms.listConfig`:

- **Return only the `name`/`value` columns the Browser reads.** Rejected:
  the Neo4j 5.x+ documented shape is the 8-column `ConfigResult`; other
  consumers (cypher-shell-style clients, fleet tooling) read more, and six
  extra constant columns cost nothing.
- **Return `dbms.security.auth_enabled: 'false'` (or omit the key) so the
  Browser never calls `dbms.showCurrentUser`.** Rejected: wrong for the
 [ADR-0126](./0126-bolt-basic-auth.md) default deployment, where auth *is* enabled; lying downward to
  suppress a cosmetic call trades an accurate default for an inaccurate
  one.
- **Emit the full Neo4j settings surface (hundreds of keys).** Rejected:
  catalog theater beyond need — CypherLite has no such settings, and the
  Browser reads a fixed nine-key list.

For `dbms.licenseAgreementDetails`:

- **Omit the procedure.** Rejected: cheap defensive completeness; a future
  enterprise-edition-reporting path or non-Browser client (cypher-shell's
  `getTrialStatus`, fleet tooling) gets an honest answer instead of
  `ProcedureNotFound`.
- **Return `status: 'no'` (no agreement accepted).** Rejected: dishonest in
  the other direction — there is no agreement *to* accept under Apache-2.0,
  and `'no'` invites accept-license UX flows that do not apply.

## Consequences

- The Browser/Desktop connect sequence completes its remaining probes:
  `dbms.clientConfig` answers with the nine-key map (no reliance on the
 [ADR-0138](./0138-bolt-procedure-not-found-code.md) `ProcedureNotFound` fallback), `dbms.showCurrentUser` fills the
  sidebar, and `dbms.listConfig`/`dbms.licenseAgreementDetails` answer if
  called.
- The placeholders are **fixed constants, not live state**: under
  `--auth user:pw` the sidebar shows `cypherlite`, and under `--auth none`
  the config map claims `auth_enabled: 'true'`. Both are cosmetic-only and
 must be documented in `docs/api/bolt/`; [ADR-0126](./0126-bolt-basic-auth.md) deployments using the
  default generated token see accurate values.
- If a later ADR adds engine-level identity or authorization, these rows
  become honest reads of it; until then the constants stand, and any change
  is a new ADR plus a **Later references** note here.
- Implementing nodes add tests in
  `cypherlite/cypherlite/tests/dbms_procedures.rs` pinning every column and
  row value above, and update `docs/api/bolt/spec.md` /
  `docs/tck/compliance.md`-adjacent docs if any existing scenario pinned
  `ProcedureNotFound` for these names.
- Together with [ADR-0142](./0142-browser-compat-record-shapes.md) this
  closes the Browser/Desktop compatibility work stream.

## Later references

- [ADR-0273](./0273-adr-session-plumbing.md) revisits the session-context decision: at explicit maintainer
 request the [ADR-0126](./0126-bolt-basic-auth.md) Bolt principal is plumbed engine-ward, so
  `dbms.showCurrentUser` reports the authenticated user (the
  `--auth user:pw` user, or `cypherlite` under the generated-token
  default) instead of the decision-1 placeholder. The placeholder stands
  under `--auth none` and on non-Bolt surfaces; decisions 2–4 are
  unchanged.
- [ADR-0497](./0497-catalogue-corpus-alignment.md) (the Cypher-25 catalogue alignment) now lists
  `dbms.showCurrentUser` and `dbms.listConfig` as `SHOW PROCEDURES` rows:
  both stay executable with the record shapes of decision 1 (as revised by
 [ADR-0273](./0273-adr-session-plumbing.md)) and decision 3 unchanged. `dbms.clientConfig` (decision 2) and
  `dbms.licenseAgreementDetails` (decision 4) stay callable but unlisted.
