# ADR-0535: The HINT notification category and the planner-hint kinds

Status: Proposed

Claim: be6e913fdc3a9fe75ba072f2f8f136692eedfb63

## Context

ADR-0363 built the notification vocabulary around three categories —
`PERFORMANCE`, `UNRECOGNIZED` and `DEPRECATION` — plus the `SCHEMA`
category its later DDL work added, and its Consequences state that a new
category is a new ADR. The engine accepts the CIP-era planner hints
(`USING INDEX`, `USING JOIN ON`, `USING SCAN`) but its planner is not
cost-based: a join hint is never fulfilled, and an index hint is served
exactly when a catalog node index on the hinted label covers every hinted
property. Live Neo4j 2026.10 reports both conditions as warnings in a
category the vocabulary does not carry: `HINT`, with the Neo4j codes
`Neo.ClientNotification.Schema.HintedIndexNotFound` (`01N31`) and
`Neo.ClientNotification.Statement.JoinHintUnfulfillableWarning`
(`01N30`), one notification per hint.

## Decision

The notification vocabulary (ADR-0363) adopts the `HINT` category — wire
spelling `HINT`, ranked after `SCHEMA` in the statement-scoped merge
order — with two Neo4j-analog kinds:

| kind | code | GQLSTATUS | severity | category | trigger | remedy |
| --- | --- | --- | --- | --- | --- | --- |
| `HintedIndexNotFound` | `Neo.ClientNotification.Schema.HintedIndexNotFound` | `01N31` | WARNING | HINT | A `USING INDEX [SEEK]` hint no catalog node index can serve: no non-vector node index on the hinted label covers every hinted property (range, text, point and fulltext indexes all serve; vector never). | Create the index, or drop the hint. |
| `JoinHintUnfulfillable` | `Neo.ClientNotification.Statement.JoinHintUnfulfillableWarning` | `01N30` | WARNING | HINT | A `USING JOIN ON` hint — the planner is not cost-based and never plans a hint-directed join, so every one is unfulfillable. | Try a different join key or restructure the query. |

Both kinds are occurrence-scoped (one row per hint, never collapsed) and
statement-scoped (no operator association). The kinds fire where a plan
tree is built, like every ADR-0363 derivation; the hints reach the
derivation through the IR's `MATCH` clause, which carries them from the
parser. The GQL-status description text reproduces the live server's
word-for-word.

## Alternatives considered

- Fold the two kinds into `UNRECOGNIZED` — rejected: the live category is
  `HINT`, and the category is part of the wire contract drivers render.
- Require a range index to serve the hint — rejected: live 2026.10.1
  fulfills the hint through text, point and fulltext indexes as well
  (probed: each suppresses the warning for a compatible predicate); only
  a vector index never serves.
- Model a fulfilled join hint as silent and an unfulfilled one as
  warned, per query shape — rejected: without a cost-based planner no
  shape can fulfil a join hint; pretending otherwise would invent
  planner behavior the engine does not have.

## Consequences

- The category vocabulary grows to five; every surface that renders
  categories (Bolt, HTTP, Python, MCP) already renders the string, so no
  transport change rides along.
- When a later ADR makes the planner honor hints (join planning, forced
  seeks or scans), the triggers change with it and this ADR gains a
  Later references note; the kinds and their codes stay.
- ADR-0363's table predates several vocabulary additions; this ADR's
  table is the record for these rows, and retrofitting the older rows
  into ADR-0363's immutable text is out of scope here.
