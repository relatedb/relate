# ADR-0567: A shortest route's endpoint listing is capped and refuses with a typed capacity error

Status: Accepted
Claim: c08553c9ad9073dba77619f4f306605c4e443c8d

## Context

ADR-0556 resolves a shortest route's property-map endpoints through the
node-candidates funnel. The pair bound probes each side with a capped
funnel call, so a non-selective map stops scanning one match past the bound.
When both sides exceed the bound, the route still needs both lists in full to
fan from the shorter, and it listed them through the uncapped funnel: an
unindexed map scanned its whole label bucket per input row and materialized
every match before anything bounded the result. ADR-0555 forbids a result
that grows with intermediate matches without a declared, charged bound.

## Decision

1. **A second, larger cap on the full listing.** The both-sides-over case
   lists each side through the capped funnel with a fixed listing limit far
   above the pair bound, rather than the uncapped funnel.
2. **Typed refusal at the cap.** A side matching more than the listing limit
   fails the statement with the capacity family's error, in a new
   `CapacityKind::SeekCandidates`, carrying the match count the scan reached
   (a lower bound) and the limit. There is no silent truncation, and no
   fallback to the unbounded path.
3. **The pair-bound probe is under the same contract.** It stops one
   match past the pair bound on every endpoint shape it probes: a bare map, a
   label and a map, a label disjunction, and a declared-index seek. Over the
   bound is a routing signal, not a result, so the probe raises no error;
   the answer is never wrong, since only the list's length is read. Only
   shapes whose scan answers with exact matches are probed: a negated or
   dynamic label falls to the all-nodes tail, a superset whose length is not
   a match count, and stays fan territory.
4. **Postings and buckets are streams.** The declared index's equality
   postings are read as a stream the consumer stops pulling; the unbounded
   id set is that stream collected, one policy. The capped funnel streams the
   shortest posting and checks each candidate against the snapshot, never
   building an id set sized by the index. A scan copies the shortest label
   bucket's ids under one short guard and visits them outside it, so the
   guard is never held across a node read; the copy is a flat id list sized
   by the bucket, not by the matches. A vanished index (a concurrent drop)
   still falls to the scan.
5. **Db Hits count the work performed.** Every node a scan examines and
   every index posting read counts, whether or not it matches, including on
   the stop-at-bound and refusing paths; an answer served whole from
   postings counts what it returned. PROFILE never reports the full set an
   unbounded scan would have read, nor only the matches of a long scan.
6. **Surface mapping is unchanged.** The refusal is the memory-limit class on
   every surface, like the other capacity kinds (ADR-0494).

## Alternatives considered

- **Leave the listing uncapped.** Rejected: memory grows with the match
  count, against ADR-0555.
- **Truncate to the limit and search on.** Rejected: silently wrong answers
  from a partial candidate set.
- **Fall back to a bound-driven path on overflow.** Rejected: the fallback is
  the unbounded scan under another name.
- **Reuse the expression-allocation kind.** Rejected: its message tells the
  user to build a value in smaller pieces, which names the wrong action.

## Consequences

- A statement whose both endpoint maps each match more than the limit now
  fails where it previously ran to completion; the message names the remedy
  (bind or index the endpoint).
- `CapacityKind` gains a variant; consumers matching it exhaustively add an
  arm. Class mapping on every surface is unchanged.
- The listing limit is a fixed constant, as ADR-0395's family is; raising it
  is a new decision.
- PROFILE Db Hits rise for scans that examine more nodes than they
  return; the engine is pre-release, so the pinned figures were updated.
- Range-comparison seeks are not reached by the probe (it carries no
  residual conjuncts) and keep their set-building read.
- The fan's start-candidate scan for bare labels is unchanged and is not
  covered by this cap.
