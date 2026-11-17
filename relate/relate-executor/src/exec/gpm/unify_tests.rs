//! LABS-661: one traversal engine for every var-length spelling. The
//! compact `-[r:T*m..n]-` form lowers to the same quantified-path IR the
//! parenthesized group produces (asserted at lowering, in
//! `relate-front`), so these tests pin the executor-side contract:
//! the start-point index seek inside the QPP traversal (NodeReads=1 for a
//! seek-able WHERE conjunct on the start variable, every spelling), and
//! identical answers across the spellings (including the WALK mode's
//! documented rel-reuse superset).

use super::*;
use crate::exec::take_node_reads;
use crate::exec::test_support::{build, exec_autocommit, exec_cypher};
use crate::graph::Graph;

/// A ring of six `Person` nodes (each `id` its build index) with one
/// undirected `knows` rel per adjacent pair, plus an `id` range index:
/// the GBL `((knows)){1,2}` shape at fixture scale.
fn person_ring() -> Graph {
    let person = &["Person"][..];
    let labels: [&[&str]; 6] = [person; 6];
    let built = build(
        &labels,
        &[
            (0, 1, "knows"),
            (1, 2, "knows"),
            (2, 3, "knows"),
            (3, 4, "knows"),
            (4, 5, "knows"),
            (5, 0, "knows"),
        ],
    );
    exec_autocommit(&built.graph, "CREATE INDEX FOR (p:Person) ON (p.id)").unwrap();
    built.graph
}

/// The trail-semantics spellings of one var-length pattern: the compact
/// bracket form, the parenthesized group, and the postfix quantifier —
/// one IR, one engine, one answer (the TRAIL-prefixed form is derived in
/// [`trailed_pattern_bodies`]).
const PATTERN_BODIES: [&str; 3] = [
    "(x:Person)-[:knows*1..2]-(y)",
    "(x:Person)(()-[:knows]-()){1,2}(y)",
    "(x:Person)-[:knows]-{1,2}(y)",
];

fn trailed_pattern_bodies() -> Vec<String> {
    PATTERN_BODIES
        .iter()
        .map(|body| (*body).to_owned())
        .chain([format!("TRAIL {}", PATTERN_BODIES[0])])
        .collect()
}

fn rows_of(g: &Graph, cypher: &str) -> Vec<Vec<Value>> {
    let r = exec_cypher(g, cypher);
    let mut rows = r.rows;
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    rows
}

/// The start point of every var-length spelling is selected through the
/// same index seek: a `WHERE x.id = …` conjunct narrows the QPP
/// traversal's start candidates to the one indexed node (NodeReads=1 in
/// PROFILE), where the pre-unification group and mode-prefixed forms
/// paid a full label scan per start.
#[test]
fn every_spelling_seeks_the_start_point_through_the_index() {
    let g = person_ring();
    for body in trailed_pattern_bodies() {
        let cypher = format!("MATCH p = {body} WHERE x.id = 3 RETURN y.id AS y ORDER BY y");
        take_node_reads();
        let r = exec_cypher(&g, &cypher);
        assert_eq!(r.rows.len(), 4, "{cypher}: two 1-hop and two 2-hop trails");
        assert_eq!(
            take_node_reads(),
            1,
            "{cypher}: the start point must be an index seek, not a label scan"
        );
    }
}

/// Identical answers across the spellings: the trail-semantics family
/// (compact, group, postfix, TRAIL-prefixed) returns the same row
/// multiset — endpoint, path length, and traversed rel list per row.
#[test]
fn trail_semantics_spellings_answer_identically() {
    let g = person_ring();
    let query = |body: &str| {
        format!(
            "MATCH p = {body} WHERE x.id = 3 \
             RETURN y.id AS y, length(p) AS l, [r IN relationships(p) | r.k] AS ks \
             ORDER BY y, l, ks"
        )
    };
    let reference = rows_of(&g, &query(PATTERN_BODIES[0]));
    assert_eq!(reference.len(), 4, "two 1-hop and two 2-hop trails");
    for body in PATTERN_BODIES[1..].iter() {
        assert_eq!(
            rows_of(&g, &query(body)),
            reference,
            "the {body} spelling must answer identically"
        );
    }
    assert_eq!(
        rows_of(&g, &query(&format!("TRAIL {}", PATTERN_BODIES[0]))),
        reference,
        "the TRAIL-prefixed spelling must answer identically"
    );
}

/// The WALK mode is the one documented divergence: under REPEATABLE
/// ELEMENTS it may reuse a relationship, so its answer is a strict
/// superset of the trail family on a graph where the only 2-walk back to
/// the start reuses the pair's single rel (the PathDB spike's seed
/// signature, research note 2026-11-01 §2).
#[test]
fn walk_mode_is_the_rel_reuse_superset() {
    let g = person_ring();
    let query = |prefix: &str| {
        format!(
            "MATCH {prefix}(x:Person)-[:knows*1..2]-(y) WHERE x.id = 3 \
             RETURN y.id AS y, length(p) AS l ORDER BY y, l"
        )
    };
    let trail = rows_of(&g, &query("p = "));
    let walk = rows_of(&g, &query("REPEATABLE ELEMENTS p = WALK "));
    // The 2-walks 3-2-3 and 3-4-3 reuse the pair's single knows rel:
    // legal for WALK, rejected for the trail family.
    let reuse_rows = walk
        .iter()
        .filter(|row| row[0] == Value::Int(3) && row[1] == Value::Int(2))
        .count();
    assert_eq!(reuse_rows, 2, "the two rel-reusing 2-walks back to x");
    assert_eq!(walk.len(), trail.len() + reuse_rows);
}

/// The zero-lower-bound and unbounded spellings keep their semantics
/// through the one engine: `*0..2` includes the zero-hop row (x=y, no
/// rels), and the unbounded `*` on the ring enumerates every trail.
#[test]
fn zero_lower_bound_and_unbounded_spellings_agree() {
    let g = person_ring();
    let query = |body: &str| {
        format!(
            "MATCH p = {body} WHERE x.id = 3 \
             RETURN y.id AS y, length(p) AS l ORDER BY y, l"
        )
    };
    let zero_hop = rows_of(&g, &query("(x:Person)-[:knows*0..2]-(y)"));
    assert_eq!(
        zero_hop.len(),
        5,
        "the zero-hop row plus the four 1..2 trails"
    );
    assert!(
        zero_hop
            .iter()
            .any(|row| row[0] == Value::Int(3) && row[1] == Value::Int(0)),
        "the zero-hop row binds y to x with an empty path"
    );
    assert_eq!(
        rows_of(&g, &query("(x:Person)(()-[:knows]-()){0,2}(y)")),
        zero_hop
    );

    let unbounded = rows_of(&g, &query("(x:Person)-[:knows*]->(y)"));
    assert_eq!(
        rows_of(&g, &query("(x:Person)(()-[:knows]->()){1,}(y)")),
        unbounded,
        "the unbounded group spelling must agree with the legacy `*`"
    );
    // A trail around the 6-ring can be at most 6 rels long (each rel
    // once); the full tour is the longest enumerated trail.
    let longest = unbounded
        .iter()
        .map(|row| row[1].as_int().expect("length is an integer"))
        .max();
    assert_eq!(longest, Some(6), "the full ring tour is the longest trail");
}

/// A var-length chain with a constrained interior node: the interior
/// node stays between the two quantified groups (first group's target,
/// second group's start), so the group spelling agrees with the legacy
/// chain.
#[test]
fn chained_var_length_with_interior_node_agrees() {
    let person = &["Person"][..];
    let place = &["Place"][..];
    let labels: [&[&str]; 5] = [person, person, place, person, person];
    let built = build(
        &labels,
        &[
            (0, 1, "knows"),
            (1, 2, "knows"),
            (2, 3, "knows"),
            (3, 4, "knows"),
        ],
    );
    let g = &built.graph;
    let query = |body: &str| {
        format!(
            "MATCH p = {body} WHERE a.id = 0 \
             RETURN b.id AS b, length(p) AS l ORDER BY b, l"
        )
    };
    let legacy = rows_of(
        g,
        &query("(a:Person)-[:knows*1..2]->(m:Place)-[:knows*1..2]->(b:Person)"),
    );
    // 0->1->2 reaches the Place in 2 hops, then 2->3 and 2->3->4 reach
    // the two Persons: one 3-hop and one 4-hop trail.
    assert_eq!(legacy.len(), 2);
    assert_eq!(
        rows_of(
            g,
            &query("(a:Person)(()-[:knows]->()){1,2}(m:Place)(()-[:knows]->()){1,2}(b:Person)")
        ),
        legacy
    );
}

/// Every spelling routes to the GPM engine — the one traversal path. The
/// GBL SF0.1 parity measurement (compact vs `((knows)){1,2}`, from the
/// 5,120 ms vs 4.3 ms cliff) is recorded in the frontier-NFA
/// measurements, its live
/// oracle the ADR-0478 differential suite
/// (`exec/kernels/frontier_differential_tests.rs`) — the retired qpp.rs
/// harness took it historically; this file pins the executor-side
/// invariants it rests on.
#[test]
fn every_spelling_takes_the_gpm_engine_path() {
    for body in PATTERN_BODIES {
        let query = relate_front::parse_and_lower(&format!("MATCH p = {body}")).unwrap();
        let relate_ir::Clause::Match { patterns, .. } = &query.parts[0].clauses[0] else {
            panic!("{body}: expected a MATCH");
        };
        assert!(
            patterns[0].needs_gpm(),
            "{body}: must route to the GPM engine"
        );
    }
}

/// The unbounded DFS depth cap under REPEATABLE ELEMENTS is one rule
/// across the spellings (stellar B8): the quantified expansion (`qpp`)
/// and the legacy var-length rel DFS (`mod`) both consult
/// `unbounded_dfs_cap`, so the same unbounded pattern enumerates to the
/// same depth whichever element carries it. The parser rejects every
/// unbounded spelling under REPEATABLE ELEMENTS — the top-level forms as
/// `UnsafeRepeatableElements`, a quantified form nested in a group as
/// `NestedQuantification` — so the pair is handed to the engine as IR
/// rewritten from one parsed bounded statement: the `{1,8}` group with
/// its bound erased (the quantified spelling), and the same traversal as
/// a legacy `Relationship` element carrying the unbounded `*` (the shape
/// the pre-ADR-0471 lowering produced and the planner-IR diagnostic
/// parser can still produce). On the 6-rel ring the unified cap is the
/// satiety cap `min(6 + 1, 32) = 7` — one hop past the 6-rel DIFFERENT
/// bound, not the raw rel count the legacy spelling took before the
/// unification.
#[test]
fn repeatable_unbounded_dfs_cap_is_one_rule_across_spellings() {
    let g = person_ring();
    let bounded = "MATCH REPEATABLE ELEMENTS p = (x:Person)((_)-[:knows]-()){1,8}(y) \
                   WHERE x.id = 3 RETURN y.id AS y, length(p) AS l";
    let unbounded_query = |legacy_rel: bool| {
        let mut query = relate_front::parse_and_lower(bounded).unwrap();
        let relate_ir::Clause::Match { patterns, .. } = &mut query.parts[0].clauses[0] else {
            panic!("{bounded}: expected a MATCH");
        };
        let elements = &mut patterns[0].elements;
        let index = elements
            .iter()
            .position(|element| matches!(element, relate_ir::PatternElement::Quantified(_)))
            .unwrap_or_else(|| panic!("{bounded}: expected a quantified group"));
        if legacy_rel {
            let (min, rel) = match &elements[index] {
                relate_ir::PatternElement::Quantified(qpp) => {
                    let relate_ir::PatternElement::Relationship(rel) = &qpp.elements[1] else {
                        panic!("{bounded}: the group's middle element must be the rel");
                    };
                    (qpp.min, rel.clone())
                }
                _ => unreachable!("the position search found a quantified group"),
            };
            let mut var_length = rel;
            var_length.var_length = true;
            var_length.min_hops = min;
            var_length.max_hops = None;
            elements[index] = relate_ir::PatternElement::Relationship(var_length);
        } else {
            let relate_ir::PatternElement::Quantified(qpp) = &mut elements[index] else {
                unreachable!("the position search found a quantified group");
            };
            qpp.max = None;
        }
        query
    };
    let rows = |legacy_rel: bool| {
        let mut rows =
            crate::exec::test_support::exec_lowered(&g, unbounded_query(legacy_rel), bounded).rows;
        rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        rows
    };
    let quantified = rows(false);
    let legacy = rows(true);
    assert_eq!(
        legacy, quantified,
        "the two spellings of one unbounded pattern under REPEATABLE ELEMENTS \
         must enumerate identically"
    );
    let longest = |rows: &Vec<Vec<Value>>| {
        rows.iter()
            .map(|row| row[1].as_int().expect("length(p) is an integer"))
            .max()
    };
    // The unified rule is the satiety cap, not the raw rel count (6): the
    // deepest enumerated walk is 7 hops. The rows are equal (above), so one
    // read of the cap covers both spellings.
    assert_eq!(longest(&quantified), Some(7));
}
