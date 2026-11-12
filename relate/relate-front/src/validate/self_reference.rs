//! 42I58 (Cypher 25): a write clause may not reference an entity the same
//! clause creates. One home for both halves of the rule — the property-value
//! half (`validate_create_self_reference`) and the CIP-156 dynamic
//! label/type half (`check_dynamic_label_scope`) — because they are one
//! Neo4j admission with one error text. ADR-0513 records the rule and the
//! conservative tier's deliberate under-approximation.
//!
//! The scope-tracking pass ([`crate::validate::validate_variable_types`])
//! applies the rule with the exact enclosing bindings — the top level and
//! `FOREACH` bodies. The scope-free battery reaches nested bodies
//! (`CALL { }`, UNION arms, `NEXT` stages) without their correlated outer
//! bindings, so it uses the conservative tier, where only the elements that
//! *must* be declarations count as created ([`declares_shape`]). The bodies
//! only that tier reaches keep that approximation (ADR-0513 §Consequences).
//!
//! Read detection is one pass per expression: [`ReadCollector`] walks an
//! expression once, applying binder-scope shadowing as it enters each
//! scope-introducing construct, and the caller intersects the collected
//! names with the clause's created set — never a re-walk per created name
//! (that shape was quadratic in the clause's declaration count, a remote
//! CPU-exhaustion vector).

use relate_error::{EngineError, ErrorKind};
use relate_names::SmallName;
use relate_parser::{Clause, Expr, LabelExpr, NodePattern, PatternElement, PatternPath, Statement};

use crate::ast::collect::{
    clause_introduced_names, clause_patterns, pattern_vars, walk_expr_scoped, ScopedExprVisitor,
};
use crate::ident_hash::{IdentHashMap, IdentHashSet};

/// Neo4j's verbatim 42I58 sentence. The one spelling both halves of the rule
/// report.
fn self_reference_error(var: &str) -> EngineError {
    EngineError::semantic(
        ErrorKind::SyntaxError,
        format!("Entity, '{var}', cannot be created and referenced in the same clause"),
    )
}

/// The smallest name in `reads` that `created` also contains — the
/// deterministic diagnostic pick, by name and never by hash order
/// (ADR-0505 §4's selection rule: `.min()`, never first-found). One policy
/// for every caller of the rule.
fn smallest_created_read(
    reads: &IdentHashSet<SmallName>,
    created: &IdentHashSet<SmallName>,
) -> Option<SmallName> {
    reads
        .iter()
        .filter(|name| created.contains(*name))
        .min()
        .cloned()
}

/// Whether a node pattern carries a shape that can only be a declaration:
/// labels or a property map. A bare element variable may instead be a
/// reference to an outer binding the caller cannot see (a nested body's
/// correlated scope), so the conservative tier does not count it as created
/// (ADR-0513 §Consequences). `has_properties` is the parser's one marker for
/// a `{…}`/`$param` map, so the property list adds nothing.
pub(crate) fn declares_shape(node: &NodePattern) -> bool {
    node.label_expr.is_some() || node.has_properties
}

/// The tier a 42I58 self-reference scan runs at: one value carrying every
/// fact the tier varies. The exact tier holds the caller's enclosing
/// bindings by reference; the conservative tier holds none — it cannot see
/// a nested body's correlated outer bindings, so it never consults a
/// fabricated empty set.
#[derive(Clone, Copy)]
enum SelfReferenceTier<'a> {
    /// The exact-scope tier: `enclosing` is every name bound before
    /// `clauses`, so a bare element variable that is already bound is a
    /// reference, never a declaration. `FOREACH` bodies share the
    /// enclosing scope and arrive here through the scope-tracking pass's
    /// own recursion. The set is consulted by reference — the caller keeps
    /// owning it.
    Exact {
        enclosing: &'a IdentHashSet<SmallName>,
    },
    /// The scope-free battery's tier: the caller cannot see a nested body's
    /// correlating outer bindings, so only an unambiguous declaration
    /// counts as created. The under-approximation is deliberate and
    /// recorded in ADR-0513 §Consequences.
    Conservative,
}

impl SelfReferenceTier<'_> {
    /// The declaration predicate and the MERGE inclusion — the tier's one
    /// correlated pair, derived together in a single match. Never two
    /// independently-settable parameters: a tier fixes both facts or
    /// neither.
    fn rules(self) -> (fn(&NodePattern) -> bool, bool) {
        match self {
            SelfReferenceTier::Exact { .. } => (|_| true, true),
            SelfReferenceTier::Conservative => (declares_shape, false),
        }
    }

    /// Whether `name` was bound before the clause list: the exact tier
    /// consults the caller's set; the conservative tier sees no enclosing
    /// binding at all.
    fn bound_before(self, name: &SmallName) -> bool {
        match self {
            SelfReferenceTier::Exact { enclosing } => enclosing.contains(name),
            SelfReferenceTier::Conservative => false,
        }
    }
}

/// The exact-scope tier over one clause's patterns: `enclosing` is every
/// name bound before the clause, so a bare element variable that is
/// already bound is a reference, never a declaration. `FOREACH` bodies
/// share the enclosing scope and arrive here through the scope-tracking
/// pass's own recursion. The clause-list shape is the conservative
/// battery's alone (it spans whole nested bodies); every exact-tier caller
/// holds one clause, so this entry takes exactly its patterns.
pub(crate) fn validate_create_self_reference(
    patterns: &[PatternPath],
    enclosing: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // One clause: nothing earlier in a list exists, so the introduced set
    // stays empty through the scan.
    let mut created = IdentHashSet::default();
    scan_patterns(
        patterns,
        SelfReferenceTier::Exact { enclosing },
        &IdentHashSet::default(),
        &mut created,
    )
}

/// The scope-free battery's tier: the caller cannot see a nested body's
/// correlating outer bindings, so only an unambiguous declaration counts as
/// created. The under-approximation is deliberate and recorded in
/// ADR-0513 §Consequences.
pub(crate) fn validate_create_self_reference_conservative(
    clauses: &[Clause],
) -> Result<(), EngineError> {
    // MERGE is excluded: in a body whose correlated outer bindings this
    // tier cannot see, a merged element with a label/property map may be a
    // reference to an imported entity rather than a declaration, so only the
    // exact tier can decide it.
    validate_create_self_reference_with(clauses, SelfReferenceTier::Conservative)
}

/// The conservative battery's whole-list span. A name is "bound before the
/// clause" when the tier says so or it sits in `introduced`, the names
/// earlier clauses of this list bound — a pattern variable that is already
/// bound is a *reference* to the existing entity, not a new one
/// (`MATCH (a) … CREATE (a)-[:R]->(b)`), and reading it in a property
/// value is legal. The exact tier never reaches this body: its callers
/// hold one clause and enter through [`validate_create_self_reference`]'s
/// pattern entry directly, so neither tier ever clones the enclosing set.
fn validate_create_self_reference_with(
    clauses: &[Clause],
    tier: SelfReferenceTier<'_>,
) -> Result<(), EngineError> {
    let (_, include_merge) = tier.rules();
    let mut introduced: IdentHashSet<SmallName> = IdentHashSet::default();
    for clause in clauses {
        let mut created: IdentHashSet<SmallName> = IdentHashSet::default();
        match clause {
            Clause::Create(create) => {
                scan_patterns(&create.pattern, tier, &introduced, &mut created)?;
            }
            // MERGE's pattern property maps are the same-clause reads the
            // rule rejects when the merged entity is a fresh declaration
            // (`MATCH (a) WITH a AS b MERGE (a {prop: a.prop})`, live
            // 2026.10 42I58). Its ON CREATE/ON MATCH SET values read the
            // merged entity after the merge, which live allows.
            Clause::Merge(merge) if include_merge => {
                scan_patterns(
                    std::slice::from_ref(&merge.pattern),
                    tier,
                    &introduced,
                    &mut created,
                )?;
            }
            _ => {
                introduced.extend(clause_introduced_names(clause));
                continue;
            }
        }
        introduced.extend(created);
    }
    Ok(())
}

/// One clause's patterns under either tier: the created set and the
/// same-clause reads, intersected once — the body both entries share.
/// `introduced` is the names earlier clauses of the enclosing list bound
/// (empty for the exact tier's one-clause entry); `created` accumulates
/// this clause's declarations for the caller's `introduced` roll-up.
fn scan_patterns(
    patterns: &[PatternPath],
    tier: SelfReferenceTier<'_>,
    introduced: &IdentHashSet<SmallName>,
    created: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let (declares_node, _) = tier.rules();
    let mut collector = ReadCollector::default();
    for path in patterns {
        // A CREATE/MERGE path name is always a fresh binding.
        created.extend(path.path_names().cloned());
        for element in &path.elements {
            collect_new_element_vars(element, tier, introduced, declares_node, created);
            collector.element(element)?;
        }
    }
    if let Some(name) = smallest_created_read(&collector.reads, created) {
        return Err(name.with_str(self_reference_error));
    }
    Ok(())
}

/// Collect the element variables a CREATE pattern introduces fresh (not
/// already bound before the clause: neither bound before the clause list
/// nor among the names earlier clauses of the list introduced).
fn collect_new_element_vars(
    element: &PatternElement,
    tier: SelfReferenceTier<'_>,
    introduced: &IdentHashSet<SmallName>,
    declares_node: fn(&NodePattern) -> bool,
    out: &mut IdentHashSet<SmallName>,
) {
    match element {
        PatternElement::Node(node) => {
            if let Some(name) = &node.variable {
                if declares_node(node) && !tier.bound_before(name) && !introduced.contains(name) {
                    out.insert(name.clone());
                }
            }
        }
        PatternElement::Relationship(rel) => {
            // A relationship variable in CREATE is always a declaration: a
            // bound one is `VariableAlreadyBound`.
            if let Some(name) = &rel.variable {
                if !tier.bound_before(name) && !introduced.contains(name) {
                    out.insert(name.clone());
                }
            }
        }
        PatternElement::Quantified(quantified) => {
            for inner in &quantified.elements {
                collect_new_element_vars(inner, tier, introduced, declares_node, out);
            }
        }
    }
}

/// Every variable name the rule's expressions read, collected in ONE walk:
/// a single descent applies the binder-scope shadowing as it enters each
/// scope-introducing construct — a comprehension's binders, a pattern
/// comprehension's pattern, the clause-ordered binders of a nested
/// statement body — so a read of a shadowed name is a reference to the
/// binder's own variable, never to a same-clause created entity
/// (`CREATE (n {prop: [n IN [false] | n]})` stays legal). The caller
/// intersects [`reads`](Self::reads) with its created set once, so the cost
/// is linear in the expression, never a walk per created name.
///
/// The descent itself is the shared scope-aware walker
/// ([`crate::ast::collect::walk_expr_scoped`]): this collector implements
/// the visit — what a read records, what a binder shadows, how a nested
/// statement's own scope is modeled — while the walker owns the one table
/// of which construct binds which name and where each scope begins and
/// ends, and the ADR-0257 nesting cap with it (its error is returned,
/// never panicked).
#[derive(Default)]
struct ReadCollector {
    /// Binder names in scope, innermost last — the frame stack `bind` and
    /// `unbind_to` own; the map below is its index, never edited beside
    /// them.
    shadowed: Vec<SmallName>,
    /// Binder name -> number of live frames binding it (nested binders of
    /// one name stack up). The O(1) membership `read` consults — a linear
    /// scan of the stack per variable read was the admission-time
    /// quadratic (rust-review + security-review, #741).
    live: IdentHashMap<SmallName, usize>,
    /// Every unshadowed variable read seen so far.
    reads: IdentHashSet<SmallName>,
}

impl ReadCollector {
    /// Record a read of `name` unless a binder in scope shadows it.
    fn read(&mut self, name: &SmallName) {
        if !self.live.contains_key(name) {
            self.reads.insert(name.clone());
        }
    }

    /// Bind `name` in the current frame.
    fn bind(&mut self, name: &SmallName) {
        self.shadowed.push(name.clone());
        *self.live.entry(name.clone()).or_insert(0) += 1;
    }

    /// Unbind back to the frame boundary `scope` (a previously captured
    /// `shadowed.len()`), reversing exactly the binds pushed above it.
    fn unbind_to(&mut self, scope: usize) {
        while self.shadowed.len() > scope {
            let name = self.shadowed.pop().expect("scope is a captured length");
            let count = self
                .live
                .get_mut(&name)
                .expect("every stacked binder is live");
            if *count == 1 {
                self.live.remove(&name);
            } else {
                *count -= 1;
            }
        }
    }

    /// The property-value expressions of one created pattern element —
    /// each property entry and the whole-map `$param` spelling — every
    /// expression the rule checks against the clause's created names.
    fn element(&mut self, element: &PatternElement) -> Result<(), EngineError> {
        match element {
            PatternElement::Node(node) => {
                self.element_properties(&node.properties, node.properties_param.as_ref())
            }
            PatternElement::Relationship(rel) => {
                self.element_properties(&rel.properties, rel.properties_param.as_ref())
            }
            PatternElement::Quantified(quantified) => {
                for inner in &quantified.elements {
                    self.element(inner)?;
                }
                Ok(())
            }
        }
    }

    fn element_properties(
        &mut self,
        properties: &[(SmallName, Expr)],
        properties_param: Option<&Expr>,
    ) -> Result<(), EngineError> {
        for (_, value) in properties {
            self.expr(value)?;
        }
        if let Some(param) = properties_param {
            self.expr(param)?;
        }
        Ok(())
    }

    /// One expression subtree, walked by the shared scope-aware walker
    /// ([`walk_expr_scoped`]): each root starts a fresh walk, so a nested
    /// body's clause roots restart the depth count exactly as the
    /// per-expression walks this collector replaced did.
    fn expr(&mut self, expr: &Expr) -> Result<(), EngineError> {
        walk_expr_scoped(expr, self)
    }

    /// One nested statement body: its clause list, then the UNION arm and
    /// the `NEXT` stage under the same shadowing — each link's own clause
    /// binders are pushed and popped by [`clause_list`](Self::clause_list).
    fn statement(&mut self, statement: &Statement) -> Result<(), EngineError> {
        self.clause_list(&statement.clauses)?;
        if let Some(union) = &statement.union {
            self.statement(&union.statement)?;
        }
        match &statement.next {
            Some(next) => self.statement(next),
            None => Ok(()),
        }
    }

    /// One clause list of a nested body: a MATCH/MERGE/CREATE pattern
    /// variable of a created name reads the enclosing entity (the clause's
    /// own declarations are not yet in scope, so they cannot shadow it);
    /// the clause's root expressions are read with the binders accumulated
    /// so far; the clause-level nested bodies apply their own scope rules;
    /// and after the clause, every name it introduces shadows for the rest
    /// of the list.
    fn clause_list(&mut self, clauses: &[Clause]) -> Result<(), EngineError> {
        let scope = self.shadowed.len();
        for clause in clauses {
            if let Some(patterns) = clause_patterns(clause) {
                for name in pattern_vars(patterns) {
                    self.read(name);
                }
            }
            let mut roots = |expr: &Expr| self.expr(expr);
            crate::ast::tree::walk_clause_expr_roots(clause, &mut roots)?;
            self.clause_level_bodies(clause)?;
            for name in clause_introduced_names(clause) {
                self.bind(&name);
            }
        }
        self.unbind_to(scope);
        Ok(())
    }

    /// The clause-level nested bodies a `FOREACH`, `CALL { }` or `WHEN`
    /// clause carries. Every body is one clause list under the binder
    /// state its enclosing walk holds: a `FOREACH` adds its loop variable
    /// on top, and a `CALL`/`WHEN` body keeps the enclosing binders
    /// shadowing — an importing `CALL` resolves its names to the nested
    /// statement's clause scope, the innermost binding, so a read there is
    /// the body's own name, never a same-clause created entity (part of
    /// the conservative tier's documented approximation, ADR-0513
    /// §Consequences).
    fn clause_level_bodies(&mut self, clause: &Clause) -> Result<(), EngineError> {
        match clause {
            Clause::Foreach(foreach) => {
                let scope = self.shadowed.len();
                self.bind(&foreach.variable);
                let body = self.clause_list(&foreach.clauses);
                self.unbind_to(scope);
                body
            }
            Clause::CallSubquery(subquery) => self.clause_list(&subquery.statement.clauses),
            Clause::Conditional(conditional) => {
                for branch in &conditional.branches {
                    self.expr(&branch.predicate)?;
                    self.clause_list(&branch.body.clauses)?;
                }
                if let Some(else_body) = &conditional.else_body {
                    self.clause_list(&else_body.clauses)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl ScopedExprVisitor for ReadCollector {
    fn enter_expr(&mut self, expr: &Expr) -> Result<(), EngineError> {
        if let Expr::Variable(name) = expr {
            self.read(name);
        }
        Ok(())
    }

    fn read_name(&mut self, name: &SmallName) -> Result<(), EngineError> {
        self.read(name);
        Ok(())
    }

    fn bind_name(&mut self, name: &SmallName) -> Result<(), EngineError> {
        self.bind(name);
        Ok(())
    }

    fn unbind_name(&mut self) -> Result<(), EngineError> {
        // The walker's event stream is well-nested, so the frame being
        // popped is always the youngest open binder.
        let scope = self.shadowed.len() - 1;
        self.unbind_to(scope);
        Ok(())
    }

    fn subquery_body(&mut self, statement: &Statement) -> Result<(), EngineError> {
        self.statement(statement)
    }
}

/// CIP-156: a dynamic label/type expression cannot reference a variable the
/// same CREATE/MERGE clause creates (the label-expression half of 42I58).
/// One binder-aware walk — the property half's [`ReadCollector`] — over the
/// dynamic expression: comprehension binders shadow their bodies, subquery
/// bodies are scanned with their own scope rules, and the pick after the
/// intersection is the one smallest-name policy.
pub(crate) fn check_dynamic_label_scope(
    expr: &LabelExpr,
    created_here: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // The leaf rule only: the shared walker ([`crate::ast::tree::walk_label_expr`])
    // owns the `Not`/`And`/`Or` fold.
    crate::ast::tree::walk_label_expr(expr, &mut |label_expr| match label_expr {
        LabelExpr::Dynamic(dynamic) => {
            let mut collector = ReadCollector::default();
            collector.expr(&dynamic.expr)?;
            if let Some(name) = smallest_created_read(&collector.reads, created_here) {
                return Err(name.with_str(self_reference_error));
            }
            Ok(())
        }
        _ => Ok(()),
    })
}

#[cfg(test)]
mod tests {
    /// The rule through the real lowering boundary's validation sequence
    /// (`lower_statement` is what the engine and the CLI run).
    fn validate(query: &str) -> Result<(), relate_core::Error> {
        let stmt = relate_parser::parse(query).expect("parse");
        crate::lower::validate_statement_for_test(&stmt)
    }

    fn self_reference(query: &str) -> String {
        validate(query).expect_err(query).to_string()
    }

    fn assert_no_self_reference(query: &str) {
        validate(query).unwrap_or_else(|error| panic!("`{query}`: {error}"));
    }

    #[test]
    fn direct_dynamic_label_self_references_are_rejected() {
        for query in [
            "CREATE (n:$(labels(n)))",
            "CREATE (n:$(n.prop) {prop: 5})",
            "CREATE ()-[r:$(type(r))]->()",
            "CREATE ()-[r:$(r.prop) {prop: 5}]->()",
        ] {
            assert!(self_reference(query).contains("cannot be created and referenced"));
        }
    }

    /// The rule reaches inside a subquery nested in the dynamic expression:
    /// a `MATCH` pattern variable (or an expression variable) already bound
    /// by the enclosing clause is a reference to the entity being created,
    /// exactly as Neo4j's 42I58 reports it.
    #[test]
    fn subquery_nested_dynamic_label_self_references_are_rejected() {
        for query in [
            "CREATE (n:$(COLLECT { MATCH (n) WITH COUNT(n) AS cnt RETURN '_' + cnt })) RETURN n",
            "CREATE (n:$(COLLECT { MATCH (n) RETURN 'x' })) RETURN n",
            "CREATE (n:$(CASE WHEN EXISTS { MATCH (n) RETURN 1 } THEN 'A' ELSE 'B' END)) RETURN n",
            // The hit in the first subquery must not be clobbered by a clean
            // second one.
            "CREATE (n:$(COLLECT { MATCH (n) RETURN 'x' } + COLLECT { MATCH (m) RETURN 'y' })) RETURN n",
        ] {
            assert!(self_reference(query).contains("cannot be created and referenced"));
        }
    }

    /// The label half rides the same binder-aware walk as the property
    /// half: a comprehension binder of the created name shadows its body —
    /// including a subquery nested in that body, which the retired second
    /// scan read with a fresh collector and false-rejected — while a read
    /// the binder-blind scan never descended to (a comprehension's WHERE, a
    /// map projection's variable selector) is the same 42I58 reference.
    #[test]
    fn dynamic_label_reads_walk_with_binder_scope() {
        for query in [
            "CREATE (n:$( [n IN [false] | n] )) RETURN n",
            "CREATE (n:$( [n IN [false] | true] )) RETURN n",
            "CREATE (n:$( [n IN [1] | CASE WHEN EXISTS { MATCH (n) RETURN 1 } THEN 'A' ELSE 'B' END ] )) RETURN n",
        ] {
            assert_no_self_reference(query);
        }
        for query in [
            "CREATE (n:$( [a IN [false] WHERE n.prop | a] )) RETURN n",
            "MATCH (m) CREATE (n:$( properties(m{n}) )) RETURN n",
            "CREATE (n:$( [a IN [false] | COUNT { MATCH (m) WHERE n.x > 0 RETURN m }] )) RETURN n",
        ] {
            assert!(
                self_reference(query).contains("cannot be created and referenced"),
                "{query}"
            );
        }
    }

    /// Several created entities in one dynamic expression: the reported
    /// offender is the smallest name, never the process-seeded hash order's
    /// first (ADR-0505 §4) — five candidates make a hash-order pick fail
    /// this most runs.
    #[test]
    fn multiple_self_references_report_the_smallest_name() {
        let error = self_reference(
            "CREATE (e), (d), (c), (b), (a:$(labels(e) + labels(d) + labels(c) + labels(b) + labels(a)))",
        );
        assert!(error.contains("Entity, 'a', cannot be created"), "{error}");
    }

    #[test]
    fn unrelated_names_and_outer_bindings_are_not_self_references() {
        for query in [
            "CREATE (n:$(COLLECT { MATCH (m) RETURN 'x' })) RETURN n",
            "MATCH (m) CREATE (n:$(labels(m))) RETURN n",
            "MATCH (m) CREATE (n:$(COLLECT { MATCH (m) RETURN 'x' })) RETURN n",
        ] {
            assert_no_self_reference(query);
        }
    }

    /// A MERGE's `ON CREATE`/`ON MATCH SET` values read the merged entity
    /// after the merge, the canonical idiom live 2026.10 accepts -- unlike
    /// the MERGE pattern's own property map, which is the 42I58
    /// same-clause reference.
    #[test]
    fn merge_on_create_and_on_match_may_reference_the_merged_entity() {
        for query in [
            "MERGE (person:Person {email: 'x'}) \
             ON CREATE SET person.identifier = 1",
            "MERGE (person:Person {email: 'x'}) \
             ON MATCH SET person.roles = [r IN person.roles WHERE r <> 'gone']",
        ] {
            assert_no_self_reference(query);
        }
        assert!(
            self_reference("MERGE (person:Person {email: person.email})")
                .contains("cannot be created and referenced")
        );
    }

    /// A plain property read of an entity the clause itself creates is the
    /// same 42I58 reference as the subquery shape: order within the clause
    /// does not matter, and neither does a function call, a list
    /// comprehension, a map projection or the `INSERT` spelling.
    #[test]
    fn plain_created_entity_reads_are_rejected() {
        for query in [
            "CREATE (a {prop: 7})-[r:R {prop: a.prop}]->(b)",
            "CREATE (a {prop: 7})-[r:R]->(b {prop: a.prop})",
            "CREATE (a:A:B)-[r:R]->(b {prop: labels(a)})",
            "CREATE p = (a {prop: 7})-[r:R {prop: a.prop}]->(b)",
            "CREATE (a {prop: 7})-[:R {prop: a.prop}]->(b)",
            "CREATE (a {prop: 7})-[r:R]->({prop: a.prop})",
            "CREATE (a), (b {prop: a.prop})",
            "CREATE (a)-[r:R]->(b), (c {prop: r.prop})",
            "CREATE (n)-[r:R {prop: true IN [a IN [false] WHERE n.prop | a]}]->(m)",
            "CREATE (n)-[r:R]->(m {prop: true IN [a IN [false] WHERE n.prop | a]})",
            "CREATE (n {prop: true IN [a IN [false] WHERE n.prop | a]})",
            "MATCH (m) CREATE (n {k: properties(m{n})})",
            "MATCH (m) CREATE (n {k: properties(m{prop: n.prop})})",
            "INSERT (a {prop: 7})-[r:R {prop: a.prop}]->(b)",
            "INSERT (a), (b {prop: a.prop})",
            // A read nested in a pattern comprehension's WHERE is reached by
            // the same fold (the pattern's and the comprehension's own names
            // shadow, the outer one does not).
            "CREATE (n {prop: [(m)-[:R]->(o) WHERE [k IN [1] | n.prop] = [1] | o]})",
            // The whole-tree battery reaches a nested body without its outer
            // bindings, so an unambiguous declaration (labels/properties)
            // still rejects a read of itself there.
            "CALL { CREATE (a {x: 1}), (b {y: a.x}) RETURN b } RETURN 1",
        ] {
            assert!(
                self_reference(query).contains("cannot be created and referenced"),
                "{query}"
            );
        }
    }

    /// A comprehension binder of the same name is a local variable, not a
    /// read of the created entity — the accepted shape the source's
    /// "may reference an entity from an earlier clause" family pins. A
    /// nested body applies the same fold (M8): the binder shadows there too.
    #[test]
    fn comprehension_binders_shadow_created_names() {
        for query in [
            "CREATE (n {prop: true IN [n IN [false] | true]})",
            "CREATE (n {prop: true IN [n IN [false] | n]})",
            "CREATE (a)-[r:R {prop: true IN [r IN [false] | true]}]->(b)",
            "CREATE (a)-[r:R {prop: true IN [a IN [false] | a]}]->(b)",
            "CREATE (a)-[r:R]->(b {prop: true IN [r IN [false] | r]})",
            "CREATE (n {prop: reduce(n = 0, x IN [1] | n + x)})",
            "CREATE (n {prop: all(n IN [1] WHERE n > 0)})",
            "CREATE (n {prop: [n IN [1] | n]})",
            "CREATE (n {prop: COLLECT { MATCH (m) WITH [n IN [1] | n] AS l RETURN l }})",
            "CREATE (n {prop: COLLECT { MATCH (m) WITH 1 AS n RETURN n }})",
        ] {
            assert_no_self_reference(query);
        }
    }

    /// A created name read inside a nested body is still a reference when no
    /// binder shadows it.
    #[test]
    fn nested_body_reads_without_a_binder_are_rejected() {
        for query in [
            "CREATE (n {prop: COLLECT { MATCH (m) WHERE n.x > 0 RETURN m }})",
            "CREATE (n {prop: COLLECT { MATCH (m) RETURN n.x }})",
        ] {
            assert!(
                self_reference(query).contains("cannot be created and referenced"),
                "{query}"
            );
        }
    }

    /// The inverse arm: a binder the nested statement itself introduced (a
    /// `WITH` alias) shadows the created name for the rest of that body —
    /// including inside a `CALL { }` body, whose imports resolve to the
    /// statement's clause scope, the innermost binding.
    #[test]
    fn nested_body_binders_shadow_created_names() {
        for query in [
            "CREATE (n {prop: COLLECT { MATCH (m) WITH m AS n RETURN n.q }})",
            "CREATE (n {prop: COLLECT { MATCH (m) WITH m AS n CALL (n) { RETURN n.q AS x } RETURN x }})",
        ] {
            assert_no_self_reference(query);
        }
    }

    #[test]
    fn structural_and_earlier_clause_reads_stay_legal() {
        for query in [
            "CREATE (a)-[:R]->(a)",
            "CREATE (a {prop: 7})-[:R]->(a)",
            "MATCH (b) CREATE ()-[:R]->(a {prop: b.prop})",
            "CREATE (a {prop: 7}) CREATE (b {prop: a.prop})",
            "CREATE (a {prop: 7}) CREATE (a)-[:R]->(b {prop: a.prop})",
            "MATCH (a) CREATE (a)-[:R]->(b {prop: a.prop})",
            // A `FOREACH` body shares the enclosing scope, so `c` created by
            // an earlier body clause is a reference (the harness's nested
            // FOREACH acceptance fixtures).
            "MATCH (r:Root) FOREACH (i IN range(1, 10) | \
             CREATE (r)-[:PARENT]->(c:Child {id: i}) \
             FOREACH (j IN range(1, 10) | \
               CREATE (c)-[:PARENT]->(:Child {id: c.id * 10 + j})))",
            // A `CALL` body's imported bindings are references too.
            "MATCH (c) CALL (c) { CREATE (c)-[:R]->(:X {p: c.q}) } RETURN c",
            "MATCH (c) CALL { WITH c CREATE (c)-[:R]->(:X {p: c.q}) } RETURN c",
            // ADR-0513 §Consequences: the conservative tier cannot see a
            // nested body's correlated outer bindings, so a *bare* declaration
            // inside `CALL { }` read by its own clause is not caught. Pinned
            // so closing the gap is a deliberate, test-visible change.
            "CALL { CREATE (a), (b {x: a.x}) RETURN b } RETURN 1",
        ] {
            assert_no_self_reference(query);
        }
    }

    /// The rule holds under every preamble spelling: an explicit
    /// `CYPHER 25` preamble (the one admitted version, ADR-0584) rejects
    /// the same-clause read exactly like the unprefixed form.
    #[test]
    fn cypher_25_preamble_keeps_the_modern_rejection() {
        for query in [
            "CYPHER 25 CREATE (a {x: 1}), (b {y: a.x})",
            "CREATE (a {x: 1}), (b {y: a.x})",
            "CYPHER 25 MERGE (a {prop: a.prop})",
        ] {
            assert!(
                self_reference(query).contains("cannot be created and referenced"),
                "{query}"
            );
        }
    }
}
