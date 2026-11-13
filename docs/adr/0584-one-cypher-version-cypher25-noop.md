# ADR-0584: One Cypher version — CYPHER 25 only, the preamble a no-op

Status: Proposed
Claim: fe268ee9f3a090fc75922f6bdad84bb1ff5eb4b7

## Context

The engine speaks one dialect: Cypher 25 — an unprefixed statement is
Cypher 25, the openCypher TCK is the floor (ADR-0056), the stretch corpus
is Cypher-25 material. Yet the `CYPHER <version>` statement preamble
admits two integers, 5 and 25; every other integer already fails at parse.
`CYPHER 5` selects a second dialect that exists only to be subtracted
from: the front end's gate allowlist rejects the Cypher-25-only surface
under it (vector values and types, the SEARCH clause, vector-index forms,
string interpolation, two-argument temporal constructors, withheld SHOW
columns) — incomplete by construction, since every new Cypher-25 feature
must remember its subtraction arm, and one arm (string interpolation)
landed only in a review round; the create-self-reference rule stands
itself down under it (ADR-0513 Decision 1); the evaluation context
branches on the version for SHOW-family null semantics; and the version
threads from the statement AST through lowering, the IR and the plan into
the query context — machinery whose only job is telling 5 from 25.

`ALTER DATABASE … SET DEFAULT LANGUAGE` admits `CYPHER 5|25` (ADR-0116
§2); the stored value is display-only (§8). Nothing the engine answers to
— the TCK floor, the live-verified Neo4j behaviours the ADRs record
(probed under `CYPHER 25`) — asks for a `CYPHER 5` answer; the few
Cypher-5 scenarios in the reporting-only harness corpus were already
slated for a re-pin (ADR-0513).

## Decision

1. **One Cypher version: 25.** The `CYPHER 25` statement preamble is an
   accepted no-op — it parses and changes nothing; an unprefixed
   statement already speaks Cypher 25.
2. **Every other `CYPHER <n>` integer fails with one authored
   unsupported-version error**, `CYPHER 5` included. Settled text, `<n>`
   the requested integer:
   `UnsupportedCypherVersion: <n> is not a valid option for cypher version. Valid options are: 25`
   The preamble fails it at parse, the language value at command
   execution — where each is checked today.
3. **`ALTER DATABASE … SET DEFAULT LANGUAGE` admits only `CYPHER 25`**
   (ADR-0116 §2, narrowed). The stored value stays display-only catalog
   state (§8 unchanged); no store format changes and a value an older
   binary wrote is not rewritten.
4. **The version machinery is removed as unreachable.** The dialect fact,
   the gate allowlist, the create-self-reference stand-down (ADR-0513
   Decision 1's arm), the SHOW-family version branches and the version
   threading through the AST, lowering, IR, plan and query context exist
   to tell 5 from 25; with 25 the only admitted version none can fire.

## Alternatives considered

- **Keep both versions and complete the CYPHER-5 gate allowlist.**
  Rejected: a complete allowlist is a forever-growing parallel vocabulary
  (each Cypher-25 feature carries a second, subtracting arm, and arms
  have landed late — string interpolation in a review round), and no
  contract asks for a CYPHER 5 answer, so the cost buys no behaviour.
- **Reject every preamble, including `CYPHER 25`.** Rejected: Neo4j
  accepts `CYPHER 25` — the live probes the ADRs record spell
  it — so rejecting it diverges from the target dialect and breaks
  correct statements to remove a wrong one.
- **Keep admitting `CYPHER 5`, drop the gates and machinery.** Rejected:
  the preamble would silently claim a version the engine does not
  implement (`CYPHER 5` would execute Cypher-25-only syntax Neo4j
  rejects) — the silent fallback the code-shape rules ban; a stated
  version that selects nothing cannot be tested for meaning.

## Consequences

- **Observable change:** `CYPHER 5`-prefixed statements — previously
  valid — now fail with the settled error, as does `ALTER DATABASE … SET
  DEFAULT LANGUAGE CYPHER 5`. The reference docs' version-selection and
  limitations sections narrow with the implementation.
- The front end loses the gate module and the stand-down; the executor's
  SHOW-family branches collapse to their Cypher-25 behaviour; the
  four-layer threading goes.
- The harness corpus's remaining Cypher-5 scenarios (ADR-0513) fail
  under the one version; their planned re-pin becomes a deletion. ADR-0513's
  Decision 1 arm is removed; its Later references note records this ADR.
