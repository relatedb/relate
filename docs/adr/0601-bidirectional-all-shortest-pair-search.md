# ADR-0601: All-shortest pair partitions search from both ends

Status: Proposed

Claim: 1bbbe0855655bf448eb7de32f18f5694ee15a203

## Context

A shortest-path pattern with both ends fixed is a pair partition. The
single-path forms already meet in the middle (ADR-0034, ADR-0037), so their
cost follows two balls of about half the distance. The all-path forms
(`allShortestPaths`, `ALL SHORTEST`) did not: they ran the one-sided layered
search of ADR-0186/ADR-0189 from the start, stopped after the layer holding
the end. On a hub-heavy social graph a distance-4 pair expands almost the
whole depth-3 ball from one side, which took tens of seconds where a
reference engine answers in milliseconds, while the single-path form for the
same pairs took about one second.

The one-sided search stays right for fans (one end fixed, every reachable
node an answer), where there is no second end to meet.

## Decision

The all-path pair partition uses a bidirectional layered search. Two
layered predecessor DAGs grow, one from each end, the second along reversed
direction rules. Each step grows one whole layer of the side with the
smaller frontier; equal frontiers grow the shallower side, so two small
frontiers still meet in the middle.

Stopping layer. While no node is held by both sides, the distance exceeds
the sum of the two depths. The first completed layer that contains a node
the other side holds therefore lands exactly on the shortest length, and the
other side holds that node at its deepest layer. The search stops there, or
returns nothing when a frontier empties or the next meeting would exceed
the pattern's maximum length.

Enumeration. Every shortest trail crosses the newly grown layer at exactly
one node of that kind. The trails are, per meeting node, each start-side
predecessor chain followed by each end-side chain read backwards. Meeting
nodes partition the trails, so none repeats and counts are the products of
the two sides' chain counts. Each trail is a shortest walk, hence node
simple and relationship unique, so no per-trail uniqueness check is needed;
multi-edges and undirected hops are separate predecessor entries on their
own side and multiply paths exactly as before. Types and direction come from
the same search description both sides share.

The set of trails and their multiplicities are the contract and equal the
one-sided result. Their order is unspecified, as for any match without an
ordering clause; the one-sided order was an accident of its stack walk.
Layer growth is one shared step used by both the one-sided walk and the
bidirectional search.

Not changed: the single-path pair search, fans, closed trails (equal ends),
the reachability prefilter, and the path-reading filter route.

## Alternatives considered

- Keep the one-sided search and prune by a precomputed end-side distance
  ball: needs the end's ball anyway, so it is the same work with a worse
  stopping rule. Rejected.
- Bidirectional search with edge-level meeting and the single-path
  `pair` trees: trees keep one parent per node and lose tied predecessors,
  which the all-path form needs. Rejected.
- Choose the side by summed degree instead of frontier size: cheaper
  per-layer estimate for hubs, but needs degree reads on every frontier node
  before expanding; frontier size matches ADR-0034 and can be refined later.
  Rejected for now.
- Stream the trails through a visitor instead of returning a vector: right
  in the long run (ADR-0555) but changes the partition interface that other
  routes share; the existing one-sided result was already a vector.
  Deferred.

## Consequences

- Distance-`d` pairs expand two balls of radius about `d/2`; the work no
  longer includes the far side of a hub reached only at the last layer.
- Memory: both DAGs hold their balls, smaller than the one-sided ball; the
  trail vector still grows with the number of shortest paths, as before.
  Making that streaming is a follow-up.
- Row order of all-path pair results may differ from earlier releases.
- Differential tests against the one-sided search (named shapes and random
  graphs) and a live comparison against a reference engine pin the answers.
