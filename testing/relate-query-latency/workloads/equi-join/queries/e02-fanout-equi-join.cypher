// e02: fan-out equi-join — the SAME two patterns and the SAME
// 2.33M-row product as e01, joined on a four-valued property instead of
// a near-unique one: the output fan-out (723,576 pairs, a third of the
// product) is what a hash join still has to emit, so this entry bounds
// the best case a join rewrite can reach — the product shrinks to
// build+probe, the emitted pairs do not shrink at all
// source: IN-HOUSE (2026-11-13), written for the LABS-1531 equi-join
// workload over the pinned graph-benchmark-ldbc SF0.1 store
// (Person/browserUsed; see the workload README).
// oracle: oracle-sf0.1.json — the single count row
// status: ok at SF0.1 (723576 pairs, 1.1 s — 2026-11-13 sizing run,
// release, reused store copy on a loaded machine)
MATCH (a:Person), (b:Person)
WHERE a.browserUsed = b.browserUsed
RETURN count(*) AS n
