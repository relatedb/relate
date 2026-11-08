# ADR-0533: graduate the `FOR` clause from the `gql-for` preview gate

Status: Accepted

Claim: 3641dd2a2616ce457b4ed733bf68e0fc0ad9a66b

## Context

ADR-0391 shipped the GQL-compliant `FOR <variable> IN <expression>`
clause (the UNWIND-successor spelling) behind a `gql-for` row of the
preview registry (ADR-0371), because the construct was approved into the
dialect but no pinned reference release shipped it ungated. The preview
registry's stated lifecycle is that a row and its gate are removed once
the feature is part of the reference dialect.

Live verification against reference releases 2026.09 and 2026.10.1 (a
local instance and a container run of each) shows the clause is
default-on there: no configuration toggle exists for it, the spelling
parses and runs with no opt-in, and its results, plan shape (the shared
Unwind operator) and error classes match what Relate ships behind the
opt-in. The gate no longer guards a divergence; it only hides a
reference-dialect spelling from the default dialect.

## Decision

Graduate the clause: delete the `gql-for` registry row, the front's
detector arm, and the gate's fail-closed behavior. The `FOR` spelling is
default dialect, valid wherever `UNWIND` is valid, with no
`RELATE_PREVIEW` opt-in.

Everything ADR-0391 built stays: the parse-time normalization into
`Clause::Unwind` and the `for_syntax` spelling flag remain — the flag's
one remaining consumer is the FOREACH-body error renderer (the 42I01
text names `FOR`, not `UNWIND`). The two gate tests
collapse into one ungated lowering test; the integration crate drops its
process-level opt-in. The registry-driven fixture in the public crate
covers the remaining rows unchanged.

## Alternatives considered

- **Keep the row until the pinned TCK floor itself carries the
  construct.** Rejected: the floor is the openCypher TCK, which will not
  adopt a GQL spelling; the preview gate's purpose is to track the
  reference dialect, which has shipped the feature.
- **Keep the row but default-enable it.** Rejected: a registry row that
  defaults on is parallel state over the same fact — the registry is the
  list of *gated* features; an ungated feature has no row.
- **Retire the `for_syntax` flag with the gate.** Rejected: it is the
  parse-spelling record the FOREACH-body rejection message reads; deleting
  it would regress the 42I01 text to the `UNWIND` spelling.

## Consequences

- `RELATE_PREVIEW=gql-for` now fails the open fail-closed as an unknown
  feature name — a loud signal to any script still setting it, not a
  silent ignore; the fix is to drop the name from the value.
- The default dialect gains one clause spelling; nothing else moves. The
  reference dialect's `SHOW FUNCTIONS`/plan surfaces are unchanged (the
  clause never had its own operator).
- The preview registry drops from fifteen rows to fourteen; its docs,
  the CLI spec's environment-variable row, and the agent-safety
  reference list ship in the same change.
- This is the registry's first graduation; it sets the pattern for the
  remaining rows as reference releases unlock them.
