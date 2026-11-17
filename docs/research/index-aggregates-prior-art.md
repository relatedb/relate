# Index-backed aggregates — prior art

What established engines do when an aggregate could be answered from an index
instead of a scan, and which of those mechanisms fit Relate's lazily built
equality index pairs. Nine entries: eight from vendor documentation, one from a
research paper on adaptive indexing. Each records the mechanism, then the
takeaway for this feature. The closing section recommends a scope.

## DuckDB — zone maps (min/max indexes)

**Mechanism.** Every column segment stores min/max zone maps (min-max indexes),
built automatically at write time; scans skip row groups whose range excludes a
filter, and ungrouped min/max/count can use segment statistics.

**Takeaway.** Statistics maintained as a side effect of storage give min/max
pruning for free, but only help when data is clustered; they are not an equality
lookup structure.

Source: <https://duckdb.org/docs/stable/guides/performance/indexing.html>

## DuckDB — ART indexes

**Mechanism.** Optional ART indexes (explicit `CREATE INDEX`, automatic for
PRIMARY KEY/UNIQUE) accelerate highly selective point lookups; the docs advise
against them for analytic scans and note they cost memory and slow bulk loads.

**Takeaway.** Even an OLAP engine keeps real indexes for selective equality
only; aggregates over large ranges go to scans, so index-backed aggregates
should target the selective case.

Source: <https://duckdb.org/docs/stable/sql/indexes.html>

## SQLite — min/max optimization

**Mechanism.** `SELECT min(x)` / `max(x)` with no `GROUP BY` is answered by one
seek to the first or last entry of a b-tree index on `x`. Covering indexes
answer queries without touching the table.

**Takeaway.** Ungrouped min/max from an ordered index is a single seek; it needs
an ordered structure, which a hash-style equality posting does not give.

Source: <https://www.sqlite.org/optoverview.html#minmax>

## SQLite — automatic indexes

**Mechanism.** When joining without a usable index, the planner builds a
transient index for the duration of one statement if it estimates a net win
(`PRAGMA automatic_index`).

**Takeaway.** Closest analogue to lazy equality pairs: build on demand on the
probed key, discard or reuse when the cost model says it pays; SQLite discards
per statement.

Source: <https://www.sqlite.org/optoverview.html#automatic_indexing>

## PostgreSQL — MinMaxAgg and index-only scans

**Mechanism.** The planner rewrites min/max aggregates into an `ORDER BY ...
LIMIT 1` index scan (MinMaxAgg); `count(*)` always scans (or does an index-only
scan using the visibility map), no stored counts.

**Takeaway.** Index-only answers need a cheap validity check; Relate's
in-memory postings avoid visibility problems but must stay consistent with
writes.

Source: <https://www.postgresql.org/docs/current/indexes-index-only-scans.html>

## MongoDB — COUNT_SCAN and DISTINCT_SCAN

**Mechanism.** The `COUNT_SCAN` / `DISTINCT_SCAN` plan stages answer count and
distinct on an indexed field by walking index key ranges without fetching
documents.

**Takeaway.** Grouped count over an equality-indexed key can count posting
entries directly, if postings hold exact, deduplicated membership.

Source: <https://www.mongodb.com/docs/manual/reference/explain-results/>

## Oracle Autonomous Database — automatic indexing

**Mechanism.** A background task observes workload, creates candidate invisible
indexes, verifies they improve captured statements, then makes them visible and
drops unused ones.

**Takeaway.** Workload-driven creation with verification and eviction; heavy for
an embedded engine, but the idea of dropping unused indexes matches a lazy pair
eviction policy.

Source: <https://docs.oracle.com/en/database/oracle/oracle-database/19/admin/managing-indexes.html#GUID-2B4F3C9E-6E1A-4B6B-B1C5-5D6B1B2F4F2D>

## CockroachDB — index recommendations

**Mechanism.** The optimizer, with hypothetical indexes during planning,
suggests indexes from observed statement fingerprints; creation stays manual.

**Takeaway.** Recommend-only automation avoids surprise memory; not needed when
pairs are built lazily in-process.

Source: <https://www.cockroachlabs.com/docs/stable/ui-databases-page.html#index-recommendations>

## Database cracking (MonetDB research) — adaptive indexing

**Mechanism.** Adaptive indexing: each range query partially reorganizes the
column around its bounds, so the index sharpens as a side effect of queries with
no upfront build.

**Takeaway.** The strongest lazy-index precedent, with ordered refinement; it
supports min/max, but is more machinery than the equality pairs require.

Source: <https://www.cidrdb.org/cidr2007/papers/cidr07p07.pdf>

## Recommendation

- **Fits.** Counting and distinct-value aggregates over a lazily built equality
  pair fit well, as MongoDB `COUNT_SCAN` and SQLite covering indexes show,
  provided the posting list is exact and kept consistent with writes.
- **Fits.** Building the pair on first probe and reusing it matches SQLite
  automatic indexes and cracking, and Relate already does this.
- **Fits.** Serving aggregates only when the planner can prove the pair is
  already built or cheap to build, so a one-off query never pays more than a
  scan.
- **Does not fit.** Min/max from an index (SQLite, PostgreSQL), because equality
  postings are unordered and a seek to the extreme needs an ordered structure;
  that would need a separate sorted index or a scan.
- **Does not fit.** DuckDB zone maps, since Relate is not columnar with
  segment-level statistics, and workload-driven background indexing (Oracle,
  CockroachDB), which is heavy for an embedded engine.
- **Recommendation.** Scope the feature to count-by-key answered from equality
  postings and leave min/max to scans.
