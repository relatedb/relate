//! The shared traversal-rung fixture (LABS-1117): one fixed 3-hop chain in
//! the chain kernel's closed vocabulary — typed hops, static label
//! conjunctions, one terminal predicate — and the statement that runs it.
//! The executor's PROFILE rung pins, the `relate` integration test, and the
//! Bolt/HTTP surface tests all exercise this one shape, so a routing change
//! fails them together instead of drifting between copies.

/// The chain's graph: two 3-hop trails from `Alpha` through `Beta` and
/// `Gamma` to `Delta`, with the head predicate (`a.k`) and the terminal
/// predicate (`d.name CONTAINS`) the kernel's programs decode.
pub const THREE_HOP_CORRIDOR: &str = "CREATE (a1:Alpha {id:'a1', k:1}), \
     (a2:Alpha {id:'a2', k:2}), (b1:Beta {id:'b1'}), (b2:Beta {id:'b2'}), \
     (g1:Gamma {id:'g1'}), (g2:Gamma {id:'g2'}), \
     (d1:Delta {id:'d1', name:'one'}), (d2:Delta {id:'d2', name:'two'}), \
     (a1)-[:R]->(b1), (a2)-[:R]->(b2), \
     (b1)-[:S]->(g1), (b2)-[:S]->(g2), \
     (g1)-[:T]->(d1), (g2)-[:T]->(d2)";

/// The statement over [`THREE_HOP_CORRIDOR`] the chain kernel serves. The
/// `WHERE` and the `LIMIT` keep the sink projection off the clause, which
/// is what lets the kernel rung take it (the ladder's gate).
pub const THREE_HOP_MATCH: &str = "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma)-[:T]->(d:Delta) \
     WHERE a.k >= 1 AND d.name CONTAINS 'o' \
     RETURN a.k AS ak, d.name AS dn LIMIT 99";
