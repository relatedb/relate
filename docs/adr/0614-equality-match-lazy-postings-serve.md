# 0614. Equality MATCH serves through the lazy postings without a declared index

Status: Proposed

Claim: 8b5f813f4016765af18965b0aafe1a2441c38790

## Context

ADR-0010 made label+property equality an always-on access method: the
executor seeks a `(label, property, value)` posting map for every keyed
node pattern, with no schema statement. ADR-0040 kept `CREATE INDEX` a
catalog name that does not gate access methods and explicitly rejected
opt-in indexes.

Since the declared TEXT/RANGE/POINT structures landed (2026-10), MATCH
equality is served only by a declared RANGE index or an index-backed
constraint. Every other keyed lookup falls to a per-row label-bucket
scan with per-node materialization. The lazy equality postings
(ADR-0063's contract) still exist but only constraint probes read them.

Measured consequence (reference host, release, in-memory store): a
statement-based bulk load — `UNWIND … MATCH (a:El {idx: r.f})` endpoint
resolution, two relationships per node — scales O(rows x label). Four
thousand nodes take ~15 s in the inline-map form and ~26 s in the
WHERE form; the identical statement with a `CREATE RANGE INDEX` pair
takes ~76 ms. The row-derived key was never the blocker — the runtime
funnel serves any per-row evaluated bound through a declared index —
the gap is the pair nobody declared.

## Decision

1. The base keyed lookup serves an equality map no declared RANGE
   index covers through the lazy equality postings: the identity
   bucket of one interned `(label, key)` pair — the smallest label
   bucket, the scan tail's own choice, so the serve never examines
   more nodes than the scan it replaces — re-verified against the
   snapshot by the canonical node predicate. The capped form stops at
   `cap + 1` matches like the declared arm.
2. The WHERE-form `=` / `IN` runtime seek serves the same way when no
   declared index serves it, re-verified through the declared family's
   snapshot filter. A declared pair always wins: its postings are
   ordered and already materialized.
3. Decline: a value whose identity is narrower than its `=` class — a
   named-zone datetime (an IANA region), bare or nested — declines to
   the scan, so an un-indexed lookup keeps `=` semantics (both DST
   fall-back halves), exactly as the scan answers today; the
   declared-index answer keeps its recorded identity behavior. An
   offset-only datetime (`Z`, a bare offset, no zone) serves: its
   (instant, offset) identity is a bijection of its `=` class (wall
   clock, offset). The one decline predicate is the group-key module's
   (the home of both relations), read by both serve sites and the
   overlay's scratch-mode decision.
4. The overlay keeps one relation per lookup: a lookup the base serves
   — declared or postings — seeks the scratch's identity-keyed index;
   a declined lookup scans both halves with `=`.
5. The postings stay outside the ADR-0042 logical charge, as since
   ADR-0172: a built pair holds at most one posting per live node
   carrying the label, and the first serve pays a one-time O(label)
   build with O(dirty) catch-up after (ADR-0063). Re-adding the
   equality term to the formula is a separate decision.
6. The plan tree, the planner ladder, and the missing-index
   notification are unchanged: the serve is a runtime fact, like the
   range seeks before ADR-0486, and the notification stays truthful
   (no RANGE index exists; declaring one still avoids the lazy build).

## Alternatives considered

- **Declared-only, document `CREATE INDEX` for bulk loaders** — leaves
  the quadratic shape for every hand-written statement load while the
  postings sit built-or-idle beside it. Rejected.
- **Restore eager always-on equality postings (the ADR-0010 shape)** —
  repeats the measured write tax ADR-0063 removed (~86% of bulk-load
  wall time). Rejected.
- **Serve every value, zoned datetimes included** — flips the
  un-indexed equality answer from `=` to identity, changing observable
  results for the overlap the scan matches today. Rejected.
- **Re-add the equality term to the ADR-0042 charge now** — moves
  `used` for every store, forcing a loadtest-envelope re-freeze inside
  an unrelated performance change. Deferred to its own decision.

## Consequences

- Statement-based bulk loads (UNWIND + keyed MATCH/MERGE endpoints)
  become near-linear without DDL; the first keyed lookup on a pair
  pays O(label) once, later lookups O(dirty) catch-up.
- Answers are unchanged for every value: served pairs answer exactly
  what the scan answered; identity-divergent values decline.
- ADR-0040's "always available" intent holds again for equality;
  ADR-0010 and ADR-0040 gain Later-references notes pointing here.
- The scratch half of a postings-served lookup seeks the scratch index
  instead of scanning, so explicit-transaction loads stay linear too.
- ADR-0172's constraint-probe path is untouched: same postings, same
  reads, one more reader.
