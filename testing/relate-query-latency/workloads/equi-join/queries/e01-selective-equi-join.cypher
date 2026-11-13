// e01: selective equi-join control — two patterns sharing no anchor,
// joined by an equality on a shared property with few matches per key:
// the control that separates the product's enumeration cost from its
// output cost (the same 2.33M-row product as e02, but 2184 output pairs
// against e02's 723,576 — an engine that only avoids materializing the
// non-matching product rows cannot move this entry much, one that joins
// instead of multiplying moves it to near-zero)
// source: IN-HOUSE (2026-11-13), written for the LABS-1531 equi-join
// workload over the pinned graph-benchmark-ldbc SF0.1 store
// (Person/birthday; see the workload README).
// oracle: oracle-sf0.1.json — the single count row
// status: ok at SF0.1 (2184 pairs, 7 ms — 2026-11-13 sizing run, release,
// reused store copy on a loaded machine; the workload README records the
// product and output sizes)
MATCH (a:Person), (b:Person)
WHERE a.birthday = b.birthday
RETURN count(*) AS n
