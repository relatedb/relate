// e03: the cross-product headline — two large-lane patterns sharing no
// anchor, joined on a shared property, the shape LABS-1531 exists for:
// today the engine materializes the full product inside the Expand and
// filters after it (the CartesianProduct notification, 03N90), so the
// entry's cost is the product, not the join. The `c.length > 100` bound
// is the sizing knob, chosen in the workload README's measured ladder so
// the entry stays seconds-scale under the lane's timeout; the join
// predicate and the pattern shape are unbounded by it.
// source: IN-HOUSE (2026-11-13), written for the LABS-1531 equi-join
// workload over the pinned graph-benchmark-ldbc SF0.1 store
// (Comment/Person, length and browserUsed; see the workload README).
// oracle: oracle-sf0.1.json — the single count row
// status: ok at SF0.1 (3537568 pairs, 7.2 s — 2026-11-13 sizing run,
// release, reused store copy on a loaded machine)
MATCH (c:Comment), (p:Person)
WHERE c.length > 100 AND c.browserUsed = p.browserUsed
RETURN count(*) AS n
