//! The shared deep-plan fixture (ADR-0628): the 251-clause CREATE chain
//! whose plan tree — one plan node per clause, about two container levels
//! each, ~502 in all — crosses the ADR-0462 data-value cap the plan wire
//! conversion no longer rides. The engine's plan-wire integration test and
//! the formatter's plan-presentation test pin this one chain, so the
//! crossing fails them together instead of drifting between two inline
//! copies. The published counts are the facts those tests assert exactly —
//! never a threshold.

/// The chain's node-CREATE count: `CREATE (n0)` … `CREATE (n124)`.
pub const NODE_CREATES: usize = 125;

/// The chain's relationship-CREATE count: `CREATE (n0)-[:R]->(n1)` …
/// `CREATE (n123)-[:R]->(n124)` — the linear chain over the nodes.
pub const REL_CREATES: usize = 124;

/// The chain's total clause count: the MATCH, both CREATE families, the
/// RETURN — one plan operator per clause, the depth the engine test's
/// document walk pins.
pub const CLAUSES: usize = 1 + NODE_CREATES + REL_CREATES + 1;

/// The ADR-0628 repro chain: `MATCH (r:L1)`, [`NODE_CREATES`] node CREATEs,
/// [`REL_CREATES`] relationship CREATEs, `RETURN n0` — [`CLAUSES`] clauses
/// in total, past the boundary the ADR names (a 248-clause chain plans
/// under the old cap, 249+ crossed it). Returns the bare statement; a
/// caller wraps it for its own form — `format!("EXPLAIN {chain}")` for the
/// plan tests, bare for the executing form.
pub fn long_create_chain() -> String {
    create_chain(NODE_CREATES)
}

/// [`long_create_chain`]'s builder at an arbitrary node count: the chain
/// over `node_creates` nodes — one node CREATE each and one relationship
/// CREATE per link — for the tests that need a size the published repro
/// does not (a node count past a quarter of the engine's plan-wire budget
/// `MAX_PLAN_WIRE_DEPTH`, whose plan crosses it). Every other caller
/// reads the published 251-clause default.
pub fn create_chain(node_creates: usize) -> String {
    let mut parts = vec!["MATCH (r:L1)".to_string()];
    for i in 0..node_creates {
        parts.push(format!("CREATE (n{i})"));
    }
    for i in 1..node_creates {
        parts.push(format!("CREATE (n{})-[:R]->(n{})", i - 1, i));
    }
    parts.push("RETURN n0".to_string());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated chain keeps the shape the consts publish — the MATCH
    /// head, the RETURN tail, and the two CREATE kinds counted separately
    /// against their own counts — the facts the consuming tests' exact
    /// assertions read.
    #[test]
    fn the_chain_keeps_its_advertised_shape() {
        let chain = long_create_chain();
        assert!(
            chain.starts_with("MATCH (r:L1) CREATE (n0)"),
            "the MATCH head opens the chain: {chain}"
        );
        assert!(
            chain.ends_with("CREATE (n123)-[:R]->(n124) RETURN n0"),
            "the RETURN tail closes the chain: {chain}"
        );
        // Every relationship clause carries exactly one `-[:R]->` and every
        // CREATE clause one `CREATE (n` opening, so the two kinds count
        // separately on the whole string — each against its own const.
        let rel_creates = chain.matches("-[:R]->").count();
        assert_eq!(
            rel_creates, REL_CREATES,
            "relationship CREATEs, counted on their own: {chain}"
        );
        let create_openings = chain.matches("CREATE (n").count();
        assert_eq!(
            create_openings - rel_creates,
            NODE_CREATES,
            "node CREATEs, counted on their own: {chain}"
        );
    }
}
