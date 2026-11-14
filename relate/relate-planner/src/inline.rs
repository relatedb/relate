//! The bounded-work plan predicate (ADR-0504): which planned statements
//! are quick at this engine's scale and may run directly on the calling
//! thread of a connection whose caller asserted the query worker's own
//! stack budget (the facade's `Connection::assume_worker_stack_on_caller_thread`,
//! named without a link — the planner cannot see the facade crate).
//!
//! The predicate is a *runtime-boundedness* classification, not a
//! stack-safety one: the asserting caller has already guaranteed the
//! calling thread carries the worker's own stack
//! budget, so parse/lowering/planning/execution recursion is as safe
//! there as on the worker (ADR-0259's margin, same stack size). What the
//! worker still buys those statements is its *wait shape*: ADR-0312's
//! NOOP keep-alive ticks on the calling thread only while the statement
//! runs on the worker, so a statement that could outlive the keep-alive
//! interval must keep that shape. Every shape whose work is
//! data-proportional or has unbounded row potential escalates; the
//! residue — point and joined reads, catalog/admin commands, plain
//! projections — is bounded by scan size at this engine's scale (the
//! store's own size limit caps a linear pass) and cannot reach the
//! interval (ADR-0504's arithmetic).
//!
//! This rides the planner's canonical walks ([`plan_any_clause`], the
//! expression descent of [`crate::plan::walk_plan_exprs`]) and the IR's
//! own shape predicates ([`PathPattern::needs_gpm`],
//! [`Expr::is_scope_bearing`]) rather than a parallel classifier: one
//! free function over `&LogicalPlan`, the shape of the planner's other
//! classifiers (`is_database_admin_command`, `query_type`).

use relate_ir::{Clause, Expr, PathPattern};

use crate::clause_kind::clause_mutates_store;
use crate::names::{projected_scope, stage_output_names};
use crate::plan::{plan_any_clause, plan_any_expr, LogicalPlan};

/// Whether `plan` is bounded-work — every clause a bounded shape, no
/// scope-bearing expression — the plan fact the ADR-0504 fast path routes
/// on: a bounded-work statement may run on the calling thread of a
/// connection that asserted the worker stack. The predicate errs toward
/// `false`: anything with data-proportional work (variable-length or
/// quantified paths, GPM patterns, vector search, **any write clause** —
/// an auto-commit write must never run inline, where a client that gave
/// up waiting cannot be kept alive by ADR-0312's NOOPs while its write
/// lands; and writes already pay WAL/commit latency that dwarfs the
/// handoff), unbounded row potential (`UNWIND`, `LOAD CSV`), the
/// **cartesian class** (multi-pattern MATCH whose patterns share no
/// variables — the one read shape whose row product is quadratic in the
/// store and breaks the keep-alive interval's arithmetic), nested
/// engines (`CALL {}`, `WHEN`/`ELSE`, `FOREACH`), or procedure calls
/// escalates to the query worker, whose wait keeps ADR-0312's NOOP
/// keep-alive ticking for the statements that can actually outlive it.
///
/// Conjunct order is load-bearing: the clause walk runs first and owns the
/// write refusal, so the cartesian walk ([`part_products`]) never sees a
/// mutating clause as anything but a no-op. The `writes_never_run_inline`
/// test pins that a write-bearing plan is refused whatever its pattern
/// shape.
pub fn plan_is_bounded_work(plan: &LogicalPlan) -> bool {
    !plan_any_clause(plan, &mut clause_escalates)
        && !plan_any_expr(plan, &mut Expr::is_scope_bearing)
        && !cartesian_join_graph(plan)
}

/// The clause shapes that escalate beyond the mutation table's answer
/// (the table's early return above owns every mutating clause, writes
/// and catalog DDL alike). Pattern-bearing clauses escalate on their
/// patterns ([`pattern_escalates`]); the cartesian class is assessed
/// per part at the plan level ([`cartesian_join_graph`]), not per
/// clause. The final arm lists every remaining variant so a new one
/// fails this match (no silent default) — for the mutating variants
/// among them it is unreachable (the table answered first); they are
/// listed for exhaustiveness, not as bounded work.
fn clause_escalates(clause: &Clause) -> bool {
    // Every mutating clause escalates (ADR-0504) — the native write arms
    // and the catalog passthrough's write commands (constraint and
    // graph-type DDL, `ALTER DATABASE`, the named index forms) alike,
    // one answer from the planner's published per-clause table, the same
    // answer the namespace gates and the write-attempt walk read. Two
    // reasons: an auto-commit write must never commit after its client
    // gave up waiting (inline work cannot tick ADR-0312's NOOPs), and a
    // write's WAL/commit latency dwarfs the handoff it thereby keeps.
    if clause_mutates_store(clause) {
        return true;
    }
    match clause {
        // Unbounded row potential: a range or list expression can carry
        // more elements than any keep-alive interval covers.
        Clause::Unwind { .. } => true,
        // Procedure bodies are arbitrary; their runtime is not a plan
        // fact.
        Clause::Call { .. } => true,
        // A nested engine runs per seed row (data-proportional passes).
        Clause::CallSubquery { .. } => true,
        // Branch bodies are nested engines of the same class.
        Clause::Conditional { .. } => true,
        // A vector search attached to the MATCH rides the index —
        // data-proportional work the compact walker does not bound.
        Clause::Match {
            patterns, search, ..
        } => search.is_some() || patterns.iter().any(pattern_escalates),
        // File-backed rows are unbounded by the plan: their count is a
        // property of the file, stated here (the documented contract's
        // own arm) rather than inherited from the mutation table.
        Clause::LoadCsv { .. } => true,
        // The bounded read residue the table left: projections and the
        // SHOW family.
        Clause::With { .. }
        | Clause::Return { .. }
        | Clause::ShowIndexes
        | Clause::Finish
        | Clause::Use { .. } => false,
        // Unreachable — the mutation table's early return above escalated
        // every one of these — listed as their own arm so the residue
        // above never re-answers the table's question, and so a new
        // `Clause` variant still fails this match.
        Clause::Foreach { .. }
        | Clause::Create { .. }
        | Clause::Merge { .. }
        | Clause::Delete { .. }
        | Clause::Set { .. }
        | Clause::Remove { .. }
        | Clause::CreateIndex { .. }
        | Clause::DropIndex { .. }
        | Clause::Catalog(_) => false,
    }
}

/// Whether the plan contains the cartesian class (ADR-0504): a MATCH
/// pattern that shares no variable with the rows flowing into it
/// multiplies against them — quadratic in the store, the one read shape
/// whose runtime can break the keep-alive interval's arithmetic. The
/// product is detected **where it materializes**, in the ordered clause
/// walk: each pattern is checked against the live scope the rows carry
/// at that point — the seed's names at a part's start, the earlier
/// patterns' bindings added as they run, a mid-part `WITH` projection
/// replacing the scope wholesale, a `NEXT` stage's seed being the
/// earlier stage's exported columns — so no later join can hide it: a
/// re-joining pattern after a product does not undo the product. A
/// `WHERE`-only join applies after the product, so only pattern-bound
/// variables connect. Nested bodies (subqueries, branches, `FOREACH`)
/// need no descent here: their clauses already escalate by arm above.
fn cartesian_join_graph(plan: &LogicalPlan) -> bool {
    // Whether any stage still follows the one being walked — only then is
    // the stage's seed needed; the final stage's output names are
    // discarded otherwise.
    let stages: Vec<&LogicalPlan> = plan.stages().collect();
    // The deepest pipeline hop count through the previous stage — a later
    // stage's patterns expand every row the earlier stages produced, so
    // the compounding depth carries forward (the maximum over the stage's
    // parts: a UNION arm's rows all flow into the next stage).
    let mut carried_hops: usize = 0;
    // The names that flowed in from the previous stage — its projected
    // output columns, not every pattern variable it bound (a projection
    // drops the names it does not export, so a later stage re-binding a
    // dropped name is a fresh binding). Empty for the first stage:
    // nothing flows in yet.
    let mut seed_names: Vec<String> = Vec::new();
    for (index, stage) in stages.iter().enumerate() {
        let seed_variables: Vec<&str> = seed_names.iter().map(String::as_str).collect();
        let mut stage_hops: usize = 0;
        for part in &stage.parts {
            let (product, hops) = part_products(part, &seed_variables, carried_hops);
            if product || hops >= MULTI_HOP_ESCALATION_HOPS {
                return true;
            }
            stage_hops = stage_hops.max(hops);
        }
        carried_hops = stage_hops;
        // This stage's projection is the next stage's seed: the exported
        // column names. Computed only when a next stage exists — the
        // final stage's names are never read. A planned statement's
        // per-stage names always derive (the UNION column-agreement check
        // ran in `plan`), the same expect policy
        // `plan_output_names_expect` states.
        if index + 1 < stages.len() {
            seed_names = stage_output_names(
                stage.parts.iter().map(Vec::as_slice),
                std::mem::take(&mut seed_names),
                &std::collections::HashSet::new(),
            )
            .expect("a planned statement's stage columns agree");
        }
    }
    false
}

/// One part's clause walk, answering both cartesian questions at once:
/// whether a product materialized (a MATCH pattern sharing no live name
/// with the rows flowing into it at that point), and the pipeline's
/// compounding hop count — the carried hops plus every fixed-length hop
/// the part's patterns walk (var-length and quantified hops escalated by
/// arm before this walk, so `rels.len()` is pure fixed length). `UNWIND`
/// never reaches this walk (it escalates by arm above); `RETURN` is
/// terminal; every other non-escalating clause leaves the live scope
/// alone.
fn part_products(part: &[Clause], seed_variables: &[&str], carried_hops: usize) -> (bool, usize) {
    use std::collections::HashSet;
    // The names the rows flowing into the next clause carry, and whether
    // rows flow at all: a leading anonymous pattern (`MATCH ()`) binds
    // no name but still produces rows, so the pattern after it multiplies
    // against them — the live-name set alone cannot express that.
    let mut live: HashSet<String> = seed_variables.iter().map(|n| n.to_string()).collect();
    let mut rows_flow = !seed_variables.is_empty();
    let mut hops = carried_hops;
    for clause in part {
        match clause {
            Clause::Match { patterns, .. } => {
                for pattern in patterns {
                    let variables = relate_ir::pattern_anchor_variables(pattern);
                    // The product materializes HERE: a pattern that
                    // shares no live name multiplies against the flowing
                    // rows, and nothing later undoes it.
                    if rows_flow && !variables.iter().any(|v| v.with_str(|t| live.contains(t))) {
                        return (true, hops);
                    }
                    rows_flow = true;
                    for name in variables {
                        live.insert(name.to_string());
                    }
                    // The pattern's fixed-length hops compound along the
                    // pipeline — each flowing row expands degree^hops
                    // paths — wherever the hops are spelled: one pattern,
                    // comma patterns, or consecutive MATCH clauses.
                    hops += pattern.rels.len();
                }
            }
            // A mid-part projection replaces the live scope: the names it
            // exports are the rows the next clauses see; a name it drops
            // is dead, and a later MATCH re-binding it is a fresh
            // binding — a product, exactly like a `NEXT` stage's seed. A
            // `*` item keeps the incoming names.
            Clause::With { items, .. } => {
                let incoming: Vec<String> = live.iter().cloned().collect();
                live = projected_scope(items, incoming).into_iter().collect();
                rows_flow = true;
            }
            // The inert/terminal residue: `RETURN` is the part's last
            // clause, the SHOW family reads the catalog, `USE` acts as a
            // no-op, and the mutating variants never reach this walk (the
            // table escalated them above) — listed so a new `Clause`
            // variant is a compile error here, exactly as in
            // `clause_escalates`, rather than silently scope-neutral.
            Clause::Return { .. }
            | Clause::ShowIndexes
            | Clause::Finish
            | Clause::Use { .. }
            | Clause::Catalog(_)
            | Clause::Foreach { .. }
            | Clause::LoadCsv { .. }
            | Clause::Create { .. }
            | Clause::Merge { .. }
            | Clause::Delete { .. }
            | Clause::Set { .. }
            | Clause::Remove { .. }
            | Clause::CreateIndex { .. }
            | Clause::DropIndex { .. }
            | Clause::Unwind { .. }
            | Clause::Call { .. }
            | Clause::CallSubquery { .. }
            | Clause::Conditional { .. } => {}
        }
    }
    (false, hops)
}

/// The pipeline hop count at which a statement escalates: two or more
/// fixed-length relationship hops along one pipeline compound
/// multiplicatively on a dense graph (each flowing row expands
/// degree^hops paths), the same data-proportional class as a var-length
/// traversal — while zero and one hops are linear in the store's nodes
/// and edges, the linear-scan class the residue already allows. Counted
/// along the pipeline (one pattern, comma patterns, consecutive MATCH
/// clauses, `NEXT` stages alike), in `part_products` — the one home of
/// the policy.
const MULTI_HOP_ESCALATION_HOPS: usize = 2;

/// The pattern shapes that escalate: GPM matching (the general path
/// matcher's DFS is data-proportional) and any variable-length or
/// quantified relationship in the compact `rels` view (a recursive
/// traversal whose work grows with the graph, `DEFAULT_MAX_HOPS` per
/// candidate).
fn pattern_escalates(pattern: &PathPattern) -> bool {
    pattern.needs_gpm()
        || pattern
            .rels
            .iter()
            .any(|rel| rel.var_length || rel.quantified)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inline classification over a planned statement, the shape of the
    /// `query_type` tests' `type_of`.
    fn bounded_work(cypher: &str) -> bool {
        plan_is_bounded_work(&crate::plan_of(cypher))
    }

    #[test]
    fn writes_never_run_inline() {
        // Disjoint-free single patterns (no product) beside a write: the
        // refusal comes from the clause walk, not the cartesian walk.
        for cypher in [
            "MATCH (a:User {id: $i}) CREATE (a)-[:T]->(:N)",
            "MATCH (a:User {id: $i}) SET a.x = 1",
            "MATCH (a:User {id: $i}) DELETE a",
            "MERGE (a:User {id: $i})",
        ] {
            assert!(!bounded_work(cypher), "{cypher}");
        }
    }

    #[test]
    fn bounded_reads_run_inline() {
        // The LABS-791 workload: point reads, joined reads, plain
        // projections, read-only catalog commands.
        for cypher in [
            "MATCH (n:User) WHERE n.id = $i RETURN n.id",
            "MATCH (n:User {id: $i}) RETURN n.id",
            "RETURN $i AS i",
            "RETURN 1 AS n",
            "MATCH (n:User) RETURN n LIMIT 10",
            "SHOW DATABASES",
            "SHOW INDEXES",
            "USE neo4j MATCH (n:User) RETURN n",
            "MATCH (n:User) WHERE n.id = $i WITH n RETURN n.id",
            "RETURN 1 AS x UNION RETURN 2 AS x",
            "MATCH (n:User) WHERE n.id = $i RETURN n.id LIMIT 1",
            // A single pattern with an anonymous endpoint is one
            // traversal — no product, inline.
            "MATCH (a:User {id: $i})-[:T]->() RETURN a",
            // Consecutive MATCHes that share a variable join; disjoint
            // ones product (the escalate list above).
            "MATCH (a:User {id: $i}) MATCH (a)-[:T]->(b) RETURN b",
            // A mid-part WITH that exports the shared name keeps the join
            // alive across the projection.
            "MATCH (a:User {id: $i}) WITH a MATCH (a)-[:T]->(b) RETURN b",
            // UNION arms are separate pipelines: their patterns never
            // join each other, so two single-pattern arms stay inline.
            "MATCH (a:User {id: $i}) RETURN a AS n UNION MATCH (b:City {id: $i}) RETURN b AS n",
            // A NEXT stage's MATCH connecting to an exported name (the
            // projection's column, not the pattern variable it aliased
            // away) joins the seed rows.
            "MATCH (a:User {id: $i}) RETURN a NEXT MATCH (a)-[:T]->(b) RETURN b AS m",
        ] {
            assert!(bounded_work(cypher), "must be bounded work: {cypher}");
        }
    }

    #[test]
    fn data_proportional_and_unbounded_shapes_escalate() {
        for cypher in [
            // Unbounded row potential.
            "UNWIND range(1, 10) AS x RETURN x",
            "UNWIND [1, 2, 3] AS x CREATE (:N)",
            // Every write clause escalates: an auto-commit write must
            // never commit after its client gave up waiting, and its
            // WAL/commit latency dwarfs the handoff anyway. Index DDL
            // included — a build walks the whole label.
            "CREATE (:User {id: $i})",
            "MERGE (n:User {id: $i}) ON CREATE SET n.fresh = true",
            "MATCH (n:User) WHERE n.id = $i SET n.seen = true",
            "MATCH (n:User) WHERE n.id = $i DELETE n",
            "MATCH (n:User) WHERE n.id = $i REMOVE n.seen",
            "CREATE RANGE INDEX user_id FOR (u:User) ON (u.id)",
            "DROP INDEX user_id",
            // Catalog write/DDL commands (the passthrough the SHOW
            // family shares): the per-clause table's answer.
            "ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 5",
            "CREATE GRAPH TYPE gt AS { (n:Person => {name :: STRING}) }",
            "CREATE INDEX i FOR (n:User) ON (n.id)",
            // The cartesian class: disjoint patterns multiply (quadratic
            // in the store — the read shape that breaks the interval),
            // within one MATCH and across consecutive ones alike.
            "MATCH (a:User), (b:City) RETURN a, b",
            "MATCH (a:User), (b:User) RETURN count(a)",
            "MATCH (a:User), (b:User) WHERE a.id = $i AND b.id = $i RETURN a",
            "MATCH (a)-[:T]->(b), (c)-[:U]->(d) RETURN a",
            "MATCH (a:User) MATCH (b:City) RETURN count(a)",
            // ... and inside a single UNION arm: that arm's graph is
            // disconnected even though the other arm joins nothing.
            "MATCH (a:User), (b:City) RETURN a AS n UNION MATCH (c) RETURN c AS n",
            // A NEXT stage is seeded with the earlier stage's exported
            // rows: a fresh MATCH sharing no exported name products
            // against the seed.
            "MATCH (a:User {id: $i}) RETURN a AS n NEXT MATCH (b:City) RETURN b AS m",
            // An aliased-away name does not flow: stage 2's `a` is a
            // fresh binding, so the product is real even though stage 1
            // bound `a`.
            "MATCH (a:User {id: $i}) RETURN a AS n NEXT MATCH (a)-[:T]->(b) RETURN b AS m",
            // ... and the same inside one part: a WITH that drops the
            // shared name leaves the later MATCH a fresh binding, so the
            // rows product (a projection drops and renames, never joins).
            "MATCH (a:User {id: $i}) WITH 1 AS x MATCH (b:City) RETURN b",
            "MATCH (a:User {id: $i}) WITH a AS x MATCH (b:City) RETURN b",
            // ... and a cartesian inside the NEXT stage, connected to
            // the seed or not, still products internally.
            "MATCH (a:User {id: $i}) RETURN a AS n NEXT MATCH (a)-[:T]->(b), (c:City) RETURN b AS m",
            // A product followed by a re-joining pattern: the product
            // materialized at the second MATCH, and the third's join
            // cannot undo it — detected where it happens, not by a
            // final component count.
            "MATCH (a) MATCH (b) MATCH (a)-[:T]->(b) RETURN b",
            // Fixed-length multi-hop compounds multiplicatively
            // (degree^hops per row) — the same data-proportional class
            // as var-length, connected or not, wherever the hops are
            // spelled: one pattern, comma patterns, consecutive MATCH
            // clauses, or a NEXT stage.
            "MATCH (a:User {id: $i})-[:T]->(b)-[:U]->(c) RETURN b",
            "MATCH (a)-[:T]->(b)-[:T]->(c) RETURN a",
            "MATCH (a)-[:T]->(b), (b)-[:T]->(c) RETURN c",
            "MATCH (a)-[:T]->(b) MATCH (b)-[:T]->(c) RETURN c",
            "MATCH (a:User {id: $i})-[:T]->(b) RETURN b NEXT MATCH (b)-[:T]->(c) RETURN c",
            // A leading anonymous pattern binds no name but still
            // produces rows: the pattern after it multiplies against
            // them — the N² shape, whether in one clause or across two.
            "MATCH (), (n) RETURN n",
            "MATCH () MATCH (n) RETURN n",
            // Variable-length and quantified paths.
            "MATCH (n)-[r:T*1..3]->(m) RETURN n",
            "MATCH (n)-[r:T*]->(m) RETURN n",
            "MATCH (n)-[r:T]->{1,3}(m) RETURN n",
            "MATCH (a)-->(b)((x)-[e]->(y)){1,3}(c) RETURN a",
            // GPM shapes: path mode, selector, inline element predicates.
            "MATCH ANY SHORTEST (n)-[r:T]->(m) RETURN n",
            "MATCH (n WHERE n.id = $i)-[r:T]->(m) RETURN n",
            "MATCH p = (n)-[r:T WHERE r.w > 1]->(m) RETURN p",
            // Nested engines.
            "CALL { MATCH (n) RETURN n }",
            "FOREACH (x IN [1, 2] | CREATE (:N))",
            "WHEN true THEN { RETURN 1 AS x } ELSE { RETURN 2 AS x }",
            // Procedures.
            "CALL db.labels()",
            // File-backed rows — both the write-fed and the bare-read
            // form: the count is a property of the file.
            "LOAD CSV FROM 'file:///f.csv' AS row CREATE (n)",
            "LOAD CSV FROM 'file:///f.csv' AS row RETURN row",
            // Scope-bearing expressions in otherwise-bounded clauses.
            "MATCH (n:User) WHERE EXISTS { (n)-[:T]->(m) } RETURN n",
            "MATCH (n:User) WHERE count { (n)-[:T]->() } > 1 RETURN n",
            "MATCH (n:User) RETURN [(n)-[:T]->(m) | m.name] AS names",
            "MATCH (a:User), (b:User) RETURN shortestPath((a)-[:T*]->(b))",
        ] {
            assert!(!bounded_work(cypher), "must escalate: {cypher}");
        }
    }

    #[test]
    fn escalation_covers_every_nesting_position() {
        // The expression walk's coverage: an escalating shape nested
        // where a part-granular (clause-only) walk would miss it — inside
        // a bounded clause's expressions — still escalates, because the
        // nested shape is the ONLY trigger in each fixture.
        //
        // Bodies whose own clause arms escalate unconditionally
        // (`WHEN`/`ELSE`, `CALL {}`, `FOREACH`) cannot be probed this
        // way: their wrapper escalates before any body shape is
        // consulted, so a fixture there would prove nothing about the
        // descent — escalation is already correct for them by the
        // wrapper's arm.
        for cypher in [
            // A scope-bearing expression inside a WITH projection.
            "MATCH (n:User) WITH n, EXISTS { (n)-[:T]->(m) } AS live RETURN live",
            // A scope-bearing expression inside a MATCH's WHERE — the
            // clause itself stays bounded (a single plain node pattern).
            "MATCH (n:User) WHERE EXISTS { (n)-[:T]->(m) } RETURN n",
            // A pattern comprehension inside a RETURN projection.
            "MATCH (n:User) RETURN [(n)-[:T]->(m) | m.name] AS names",
        ] {
            assert!(
                !bounded_work(cypher),
                "must escalate from its nest: {cypher}"
            );
        }
    }
}
