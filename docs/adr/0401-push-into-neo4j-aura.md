# 0401. `push` into Neo4j/Aura: constraint-keyed MERGE, batched UNWIND, additive by default

- **Status**: Proposed
- **Date**: 2026-10-24
- **Deciders**: CypherLite maintainers (Linear LABS-9, LABS-36)
Claim: 157fe4a42bfc87d45893bc5c9639c9490968cb19

## Context

`cypherlite pull` (ADR-0080, ADR-0158) loads a Neo4j/Aura subgraph into a
local `.cl` store over the HTTP Query API v2, upserting source-scoped data.
The reverse direction — pushing local CypherLite data into Aura — does not
exist. Linear **LABS-9** ("Aura migration — one connection string") asks
for the upgrade path from local files to Aura; its child **LABS-36**
("Export/push into Aura") is the CLI utility.

The design constraints, from LABS-9/36 and the maintainer's brief:

- **Agent- and user-friendly**: machine-readable results, a plan mode that
  shows exactly what would happen, no silent partial state.
- **Reduce surprises, with configurable leniency**: idempotent re-push,
  additive updates by default, explicit — never guessed — identity and
  conflict rules, but every strictness default overridable per label,
  per type, or globally.
- **Respect existing unique constraints**: the local store's uniqueness
  constraints are the natural merge keys.
- **Deletions are hard** and explicitly out of V1: merging a subgraph back
  is additive/update-only; deletion control is a later, opt-in design.

## Decision

Add `cypherlite push` — `pull`'s mirror over the same Query API v2 client
and the same credentials file (the "one connection string", LABS-9):

```
cypherlite push --to CREDS --database PATH
  [--query Q | --labels L1,L2 | --types T1,T2]
  [--batch-size N]              (default 5000)
  [--nodes-update POLICY]       (add | replace | skip; default add)
  [--rels-update POLICY]        (add | replace | skip; default add)
  [--updated-at NAME[,NAME]]    (candidate property names)
  [--rel-mode MODE]             (keyed | structural | create; default keyed)
  [--rel-key TYPE:PROP[,PROP]]  (fallback key for unkeyed types)
  [--merge-key LABEL:PROP[,PROP]]  (fallback key for unkeyed labels)
  [--unkeyed-labels ACTION]     (skip | create; default skip)
  [--dry-run] [--json]
```

### 1. Identity comes from the store's uniqueness constraints

Node merge keys are read from the local store's node uniqueness
constraints (`ConstraintDef` in the catalog), per label, including
composite keys. Generated node batches are
`UNWIND $rows AS row MERGE (n:Movie {title: row.key}) … SET n += row.props`
— one statement shape per label, derived from the catalog.

A label with no uniqueness constraint **fails closed by default**: it is
reported and skipped (counted in the run report), never silently guessed.
Leniency is explicit: `--merge-key Movie:myId` supplies a key for that
label; `--unkeyed-labels create` switches unkeyed labels to `CREATE` rows
(one-shot seeding semantics, documented as non-idempotent).

### 2. Batched UNWIND, constraint-derived statement shapes

Nodes are grouped by label, relationships by type, and pushed in
parameterized batches (`--batch-size`, default 5000). Relationship rows
carry their endpoints as resolved node keys (both endpoints are read
locally before generation — the remote is never asked to guess), so a
relationship batch is
`UNWIND $rows AS row MATCH (a {…row.start}) MATCH (b {…row.end}) …`.
All statements are parameterized; no values are interpolated into text.

**The request wire is the typed form under the versioned
`Content-Type: application/vnd.neo4j.query.v1.2`** — exactly the
official Query API v2 typed request form ([the typed JSON
docs](https://neo4j.com/docs/query-api/current/typed-json/)): every
parameter value is a `{"$type", "_value"}` envelope, **containers
included** — the batch's `rows` travel as a `List` envelope whose
elements are `Map` envelopes, whose inner maps (keys, endpoints,
properties) are `Map` envelopes in turn, so dates, points, durations,
and vectors keep their Cypher type remotely by construction. The v1.2
dialect retains types added after the unversioned v1.0 alias, including
vectors and UUIDs. A plain JSON array of envelope objects answers 400 (the
fully enveloped form answers 202); an envelope under plain
`application/json` arrives remotely as a map, not a coerced value; and
a plain JSON object nested in the typed structure reads as a typed
value whose `$type` is null ("Type null is not supported"). Values with
no parameter representation (byte arrays, elements in property
position) are refused loudly, never silently stringified. Servers
older than the typed request dialect (5.26 answers 415) need a plain
`application/json` fallback — a follow-up, not part of this ADR: push
targets Aura and current self-hosted servers.

### 3. Update policies, scoped per labels/types/all

- `add` (default): `SET +=` — local properties land, remote-only
  properties survive. The no-surprises default for merging subgraphs back.
- `replace`: `SET =` — local truth overwrites the property map; remote-only
  properties on that entity are removed.
- `skip`: only missing entities are created; existing ones untouched.

Each policy flag accepts global form (`add`) or per-entity form
(`Movie=replace,Person=skip`); a bare global policy is the default for
every entity not named. Policies are separate for nodes and relationships.

### 4. `--updated-at` conflict leniency (opt-in)

`--updated-at updatedAt,updated,lastModified` names candidate properties.
When both the local entity and the remote row carry the same candidate
(first present wins, resolved per entity), the scoped update policy
applies only if the local timestamp is **newer or equal** (equal applies —
idempotent re-push); a strictly newer remote skips the entity, counted
`remote-newer` in the report. Incomparable types fall back to the scoped
policy and increment a warning counter — stated, never silent. Without
`--updated-at`, the scoped update policy decides everything.

### 5. Relationships: identity resolved per type, structural as fallback

Relationship identity follows the same precedence rule as node identity
(§1): a **`--rel-key TYPE:PROP[,PROP]` override** (composite keys
included) wins; else the type's **relationship uniqueness/key
constraint** — which *overrules* the pair rule when it exists; else —
neither present — **structural** MERGE (`MERGE (a)-[:T]->(b)`), the
"only create one relationship between two nodes" default, idempotent
across re-pushes.

The flag surface mirrors that resolution instead of selecting it by hand:
`--rel-mode keyed` (the default) resolves per type as above; `structural`
forces the pair rule even where a key exists (parallel rels of the same
type between the same two nodes collapse into one on the remote — the
trade-off stated in `--help`); `create` is the explicit non-idempotent
escape hatch. A keyed merge on resolved properties is
`MERGE (a)-[r:T {key}]->(b) SET r += row.props`.

### 6. Plan mode, machine-readable report, fail-closed errors

`--dry-run` prints the plan — per-label/type counts, merge keys, statement
shapes, batch counts, update policies — without touching the remote. The
run report (table by default, `--json` for agents) counts per entity:
created, updated, skipped (unkeyed / policy-skip / remote-newer) and the
batch/byte totals. A failing batch fails the push with the offending
statement, parameters, and batch offset (retryable from the report).
Exit codes follow the CLI convention: 0 success, 1 runtime/remote, 2
usage. Partial application is honest: the report lists exactly which
batches landed.

### 7. Deletions: out of V1, sketched for V2/V3

V1 is additive/update-only. Deleting remote-only data requires trusting
"absent locally ⇒ delete remotely", which across `--labels` scopes and
non-overlapping pushes is easy to get wrong — so it is not guessed now.
The later design (its own ADR): explicit deletion scopes
(`--prune-scope Label[,…]`), a dry-run diff that lists remote-only
candidates for the scope first, and a confirmed delete pass. Nothing in
V1's flags reserves or approximates that behavior.

### 8. Scope and mechanics

`push` lives beside `pull` in the CLI (same feature-bundle family, same
`--database` storage semantics, no `:memory:` target) and beside the load
path in `cypherlite-load` (generation of the batched statements is the
reverse of `extract`: read local store → group by entity/merge-key →
parameterized batches → the existing Query API v2 client with the same
credentials file or URI for `--to`). Remote schema is never modified:
constraints are not created on the remote by the push (a remote that
rejects a merge key fails loudly with the remote's own error); a future
`--ensure-constraints` is a possible follow-up, not part of this ADR.

## Alternatives considered

- **Full bidirectional sync with deletions now** — rejected for V1:
  deletion-on-absence is the highest-surprise primitive in the design
  space; shipping it before the additive semantics are proven would
  compromise the "reduce surprises" constraint. Deferred to its own ADR
  (§7).
- **CREATE-only export (the "dumb pipe")** — rejected as default:
  re-pushing a dataset would duplicate every node and relationship;
  constraint-keyed MERGE is the idempotent default, with `create` kept as
  the explicit escape hatch.
- **Bolt transport** — rejected: the Query API v2 client, credentials
  handling, and typed-wire decoding already exist in `cypherlite-load`
  for exactly these endpoints; a Bolt client would add a heavy dependency
  (R6) to gain nothing for the migration use case.
- **Dump/CSV export instead of statement streaming** — rejected: files
  lose all semantic control (merge keys, policies, timestamps) that this
  design is about; CSV also drops types.
- **`--updated-at` default-on with guessed property names** — rejected:
  guessing which property is a timestamp is exactly the surprise class
  the design forbids; the candidate list is explicit and opt-in.
- **Per-entity defaults diverging from the global default** — rejected:
  one default (`add`) with named per-entity overrides is simpler to
  explain and to predict than per-entity defaults.

## Consequences

- LABS-9's one-connection-string story completes: the same credentials
  file drives both directions, and a `.cl` file becomes a first-class
  citizen of the Aura migration path.
- The CLI surface grows a lifecycle verb and its flag table
  (`docs/api/cli/spec.md`), `--help`, and the skill bundle's `pull`
  reference gain a `push` companion (`client-cli.md` + SKILL.md table
  row).
- Idempotent re-push is the documented contract: running the same push
  twice lands the same remote state (under `add`/`skip`, `structural`
  rels, and `--updated-at` where applicable) — a cookbook recipe and
  integration scenarios on the movies and northwind datasets demonstrate
  it.
- Remote constraint drift becomes visible: a local merge key the remote
  does not enforce can still merge correctly but permits remote duplicates
  pre-existing; the report does not audit this (called out as a known
  limitation).
- Deletion-based sync remains impossible until the V2 ADR; users needing
  removal today must delete on the remote explicitly.

## Later references

The LABS-907 fix (2026-11-04, amended same day) tightened §2's
grouping: nodes are grouped by their **full sorted label set**, not by
one label, so secondary labels survive the push (a local
`:Chunk:SessionChunk` previously arrived as `:SessionChunk` only). The
first cut spelled every label into the MERGE pattern —
`MERGE (n:`A`:`B` {key})` — which only matches nodes carrying BOTH
labels: a remote node already sitting under the keyed label without the
secondary (pushed before the label was added locally, or seeded
otherwise) would not match, and the merge would FORK a second node with
the label combination where an update was meant. The amended shape
merges on the set's **keyed label alone** — the label the
constraint/config keys — and SETs the remaining labels onto the node
the merge found: `MERGE (n:`A` {key}) SET n:`B`, n += row.props`. A
local label-set change is therefore an update (labels are added to the
existing node), never a fork; label removal stays out of scope with the
deletion-sync limitation below. The rel phase still resolves endpoints
through the keyed label, and the pre-read matches the keyed label — the
merge identity — with sibling label sets holding distinct keys under
the keyed label's uniqueness constraint.
