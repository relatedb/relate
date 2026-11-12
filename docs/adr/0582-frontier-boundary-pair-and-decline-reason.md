# ADR-0582: The frontier admits the boundary-pair comparison and names why it declined

Status: Proposed
Claim: 1e1c9a613f79f35d07127c1d1bdea5b8a2dfc481

## Context

The frontier rung (ADR-0478) decodes each `MATCH ... WHERE` conjunct into a
closed vocabulary: a property of one boundary variable against a
row-invariant bound. A comparison between the two boundary variables,
`n <> p`, is outside it, so the common distinct-reach statements (`WHERE
a <> b WITH DISTINCT a, b RETURN count(*)`) decline with the conjunct gate
and run on the per-row walker, whose cost is linear in the trails it
enumerates rather than in the distinct (node, state) pairs.

Separately, the plan-time gate that kept a clause off the rung is dropped
where the rung is engaged. PROFILE (ADR-0516) names the run-time declines
only, so a clause on the walker could not say whether the frontier never
applied, declined on shape, or declined on cost; the answer needed source
reading and timing fits.

## Decision

1. The vocabulary gains one entry: `<>` between the pattern's two named
   boundary variables, in either order. It is admitted only for the
   distinct-endpoint consumer, whose answer is the set of (seed, endpoint)
   pairs, and is decided per pair at arrival: a pair whose endpoint is its
   own seed (re-reached by a cycle or self-loop, or the zero-length arrival
   of a zero lower bound) does not answer; endpoints compare by node
   identity. The default pool's trail-existence proof is unchanged, so the
   admitted endpoint set is exactly the one the walker's row filter leaves.
   Every other consumer, every other comparison, and every comparison
   against a non-boundary variable keeps the conjunct decline. The cost
   gate is untouched.
2. The PROFILE record's decline field carries a typed reason that is
   either a plan-time gate (`FrontierDecline`) or a run-time decline
   (`FrontierRunDecline`), as one public sum, `FrontierDeclineReason`,
   re-exported by the `relate` crate. It is recorded for clauses with one
   quantified pattern whose family is not stood down; the first decline per
   clause wins. The wire vocabulary of the existing reason field grows from
   the two run-time names to those plus one kebab-case name per plan-time
   gate; the plan table marks it as before.

## Alternatives considered

- **Treat `<>` as a residual filter after the walk.** Rejected: the
  dispatch's reduce tail already re-applies the `WHERE`, but a clause can
  reach the rung without one, and an unfiltered self pair would be emitted
  and counted before it is dropped.
- **Admit equality and the ordering comparisons between boundary
  variables.** Rejected: no measured shape needs them, and equality keeps
  only the self pair, a different answer shape.
- **Admit the comparison for the capped and EXISTS consumers.** Rejected:
  those paths emit or test per witness; the pair rule is proven only for
  the pair-set answer.
- **Keep the decline reason internal, test-only.** Rejected: the cost of
  not seeing it was the investigation this decision answers, and the record
  is already the public place the rung and its run-time declines appear.
- **A second, separate PROFILE field for plan-time reasons.** Rejected: one
  clause has one first reason, and two fields would need a rule for both
  being set.

## Consequences

- Statements with `a <> b` over a bounded or unbounded typed quantifier can
  be served by the frontier when the cost gate agrees; answers are
  identical to the walker's, pinned by differential tests over generated
  multigraphs and the gate's statement shapes.
- Whether the cost gate agrees is unchanged and unaudited here: small
  bounds the gate lets through may run slower on the frontier than on the
  walker; retuning belongs to the cost model, not this decision.
- `PlanNode` metrics' `frontier_decline` changes type from the run-time
  enum to the sum; consumers reading only `as_str` are unaffected, the wire
  field keeps its name and gains values.
- A renamed or added decline fails the executor/wire parity test.
