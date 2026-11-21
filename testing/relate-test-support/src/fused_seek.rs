//! The shared fused-children fixture (ADR-0622): the range index and
//! two-node graph an index-seek-started `MATCH` runs over, and the
//! statement. The Bolt, HTTP, MCP and FFI surface tests all seed with
//! these, so the four wire pins exercise one shape.

/// The range index and the graph, in statement order.
pub const FUSED_SEEK_SEED: [&str; 2] = [
    "CREATE RANGE INDEX person_name FOR (p:Person) ON (p.name)",
    "CREATE (:Person {name: 'Ada'})-[:KNOWS]->(:Person {name: 'Bob'})",
];

/// The seek-started statement over [`FUSED_SEEK_SEED`]: its plan nests the
/// seek step and one `Expand` per hop under the clause.
pub const FUSED_SEEK_MATCH: &str =
    "MATCH (a:Person)-[:KNOWS]->(b:Person) WHERE a.name = 'Ada' RETURN b.name";
