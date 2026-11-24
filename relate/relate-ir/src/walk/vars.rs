//! The name-collection family of the expression walkers
//! ([`references_any`], [`expr_vars`], [`label_expr_vars`],
//! [`path_pattern_vars`]): the folds over the scope model
//! ([`walk_scope_children`](super::walk_scope_children)) that read which
//! names an expression uses — split from the `Expr` traversal core in
//! the parent (the LABS-311 size gate).

use std::collections::HashSet;

use relate_names::SmallName;

use super::{each_label_expr_child, reads, walk_scope_children, ChildScope};
use crate::walk_query::{
    clause_patterns, path_pattern_mentions, pattern_anchor_variables, walk_query_with_roles,
    ExprRole,
};
use crate::{CallScope, Clause, Expr, LabelExpr, MapProjectionItem, PathPattern, Query};

/// The names a map projection's shorthand items read (`{n, .name, k: v}` reads
/// `n`; `k: v` reads only what `v` reads). One policy both free-variable
/// walkers consult — the shorthand item is a name, not an expression, so it is
/// invisible to [`Expr::children`].
fn map_projection_shorthand_names(items: &[MapProjectionItem]) -> impl Iterator<Item = &SmallName> {
    items.iter().filter_map(|item| match item {
        MapProjectionItem::Variable { name } => Some(name),
        MapProjectionItem::KeyValue { .. }
        | MapProjectionItem::Property { .. }
        | MapProjectionItem::AllProperties => None,
    })
}

/// Whether `expr` references any of `names` from *its own* scope.
///
/// One fold over the scope model ([`walk_scope_children`]): a scope-bearing
/// variant ([`Expr::is_scope_bearing`]) is opaque (its body opens its own
/// scope), and an identifier a comprehension/quantifier/`reduce` binds
/// ([`Expr::binds_identifiers`]) is not a reference to an outer name — only
/// the variant's *source* expression is evaluated outside that binding.
/// The one predicate the CALL-subquery importing-WITH rule uses to decide
/// whether a projection reads the enclosing scope.
pub fn references_any(expr: &Expr, names: &HashSet<String>) -> bool {
    fn go(expr: &Expr, names: &HashSet<String>, bound: &mut Vec<SmallName>) -> bool {
        match expr {
            // The String-keyed read set is probed through the allocation-
            // free text bridge (with_str), never a per-lookup to_string.
            Expr::Variable(name) => {
                name.with_str(|text| names.contains(text)) && reads(name, bound)
            }
            other if other.is_scope_bearing() => false,
            // A map projection's shorthand items are reads the children walk
            // cannot see (`MapProjectionItem::Variable` is a name, not an
            // expression): test them through the one shared policy, then
            // descend the value expressions as usual.
            Expr::MapProjection { items, .. }
                if map_projection_shorthand_names(items).any(|name| {
                    name.with_str(|text| names.contains(text)) && reads(name, bound)
                }) =>
            {
                true
            }
            other => {
                let mut hit = false;
                walk_scope_children(other, &mut |child, scope| {
                    if hit {
                        return;
                    }
                    match scope {
                        ChildScope::Enclosing => hit = go(child, names, bound),
                        ChildScope::Element(bound_names) => {
                            let mark = bound.len();
                            bound.extend(bound_names);
                            hit = go(child, names, bound);
                            bound.truncate(mark);
                        }
                    }
                });
                hit
            }
        }
    }
    go(expr, names, &mut Vec::new())
}

/// Collect the variable names an expression references, comprehension- and
/// reduce-bound identifiers excluded.
///
/// For pattern variants the pattern's own variable names are included: the
/// executor's column analysis treats them as values the expression needs
/// from the surrounding row. Subquery variants contribute every variable
/// name mentioned in the subquery (an over-approximation of the outer
/// references, matching the historical executor behaviour).
///
/// This is [`references_any`]'s scope model over a collecting leaf policy
/// (the two documented differences: scope-bearing variants are descended
/// here rather than treated as opaque, and a map projection's shorthand
/// items count as reads).
pub fn expr_vars(expr: &Expr, out: &mut HashSet<String>) {
    fn go(expr: &Expr, bound: &mut Vec<SmallName>, out: &mut HashSet<String>) {
        // A subquery body contributes every name it mentions (the
        // documented over-approximation) instead of the generic child
        // descent — the one accessor decides which expressions open one.
        if let Some(query) = expr.subquery_body() {
            exists_outer_vars(query, out);
            return;
        }
        match expr {
            Expr::Variable(v) => {
                if reads(v, bound) {
                    // Map-bridge: the String-keyed variable sink stays until
                    // the typed expr_vars sink (LABS-936) lands; the text
                    // render happens once per collected name, at this feed.
                    out.insert(v.to_string());
                }
            }
            Expr::PatternPred { path } | Expr::ShortestPath { path, .. } => {
                path_pattern_mentions(path, out);
            }
            Expr::PatternComp {
                path,
                predicate,
                projection,
            } => {
                path_pattern_mentions(path, out);
                if let Some(predicate) = predicate {
                    go(predicate, bound, out);
                }
                go(projection, bound, out);
            }
            // The shorthand items are reads the children walk cannot see,
            // tested through the same unbound-read predicate as the
            // bare-variable arm — a shorthand name an enclosing
            // comprehension binds is excluded exactly like a bound
            // variable.
            Expr::MapProjection { items, .. } => {
                for name in map_projection_shorthand_names(items) {
                    if reads(name, bound) {
                        out.insert(name.to_string());
                    }
                }
                expr.each_child(&mut |child| go(child, bound, out));
            }
            other => walk_scope_children(other, &mut |child, scope| match scope {
                ChildScope::Enclosing => go(child, bound, out),
                ChildScope::Element(names) => {
                    let mark = bound.len();
                    bound.extend(names);
                    go(child, bound, out);
                    bound.truncate(mark);
                }
            }),
        }
    }
    go(expr, &mut Vec::new(), out);
}

/// Collect the variable names an expression reads from the **current row**
/// — [`expr_vars`] with every nested scope's reads left out: a subquery
/// expression's body (`EXISTS` / `COUNT` / `COLLECT`), a pattern predicate
/// or path expression, and a comprehension's / quantifier's / reduce's
/// per-element bodies read names of their own scope, never this row.
///
/// The consumers are the projections that end an input name's scope: after
/// a `DISTINCT` or aggregating `WITH`/`RETURN`, its `WHERE` / `ORDER BY`
/// may only read what the projection exposes, so a subquery body there
/// that re-uses a dropped name binds it fresh (Neo4j-verified; REL8-2023
/// issue 64) — seeding the body from the row would pin it to the dropped
/// value. A name the projection does expose is in the emitted row anyway,
/// so a body's correlation on it needs no seed either.
pub fn expr_row_vars(expr: &Expr, out: &mut HashSet<String>) {
    // A scope-bearing variant (`is_scope_bearing`) opens a nested scope;
    // an identifier-binding one's per-element bodies do — the one
    // classification [`walk_scope_children`] applies below.
    if expr.is_scope_bearing() {
        return;
    }
    match expr {
        Expr::Variable(v) => {
            out.insert(v.to_string());
        }
        // The shorthand items are reads no child holds (the same accessor
        // [`expr_vars`] reads); the base expression and every `KeyValue`
        // value are its children, so they ride the one child walk.
        mp @ Expr::MapProjection { items, .. } => {
            for name in map_projection_shorthand_names(items) {
                out.insert(name.to_string());
            }
            mp.each_child(&mut |child| expr_row_vars(child, out));
        }
        other => walk_scope_children(other, &mut |child, scope| {
            if matches!(scope, ChildScope::Enclosing) {
                expr_row_vars(child, out);
            }
        }),
    }
}

/// Variables referenced by the dynamic (`$(…)`) parts of a label
/// expression: a fold over `each_label_expr_child`, the one enumeration
/// of the `LabelExpr` structure. The variant vocabulary is matched in
/// that helper alone, so a new `LabelExpr` variant fails compilation
/// there and is picked up here without a second match to keep aligned.
pub fn label_expr_vars(expr: &LabelExpr, out: &mut HashSet<String>) {
    each_label_expr_child(expr, &mut |child| expr_vars(child, out));
}

/// Node variables, then rel variables, of a path pattern's compact view
/// (`nodes`/`rels`) — the path variable excluded. Compact-view only:
/// lowering clears `nodes`/`rels` when an element is non-alternating or
/// quantified (see `walk_query`'s rule), so such a pattern yields nothing.
pub fn path_pattern_vars(path: &PathPattern) -> impl Iterator<Item = &SmallName> {
    path.nodes
        .iter()
        .filter_map(|n| n.variable.as_ref())
        .chain(path.rels.iter().filter_map(|r| r.variable.as_ref()))
}

/// Every name a subquery body mentions — the over-approximation
/// documented on [`expr_vars`]. The scan is the canonical clause walk,
/// not a slot list: every Read-role expression ([`walk_query_with_roles`]
/// owns the slot enumeration — a projection's `group_by`, a CALL's
/// `where`, a search spec, a `SET` value, and the UNION arms, `NEXT`
/// stages and nested bodies a hand-enumerated parts walk cannot reach),
/// plus every name a clause's patterns mention ([`path_pattern_mentions`]:
/// a pattern's names are values the expression needs from the surrounding
/// row, `expr_vars`'s documented rule — a `CREATE`/`MERGE` target pattern
/// included, whose WriteTarget payload the Read filter skips by design).
pub fn exists_outer_vars(query: &Query, out: &mut HashSet<String>) {
    // The pattern mentions land in their own sink so the two callbacks
    // never share a borrow; the union happens once at the end.
    let mut pattern_names = HashSet::new();
    walk_query_with_roles(
        query,
        &mut |clause| {
            // The patterns the clause carries (the one clause→patterns
            // selection) and a `CALL (vars) { }` scope clause's import list —
            // a read of the enclosing row the body may reach only through
            // the import (`COUNT { CALL (n) { RETURN 1 AS q } RETURN q }`
            // mentions `n` nowhere else, REL8-2028).
            for path in clause_patterns(clause) {
                path_pattern_mentions(path, &mut pattern_names);
            }
            if let Clause::CallSubquery {
                scope: CallScope::Vars(vars),
                ..
            } = clause
            {
                pattern_names.extend(vars.iter().map(SmallName::to_string));
            }
        },
        &mut |expr, role| {
            if matches!(role, ExprRole::Read) {
                expr_vars(expr, out);
            }
        },
    );
    out.extend(pattern_names);
}

/// The names a subquery body **correlates** on: every name it reads or
/// mentions ([`exists_outer_vars`]) minus the names its own patterns bind
/// ([`pattern_anchor_variables`] per [`clause_patterns`]) — the one IR fact
/// behind a caller that must hand a body's enclosing row the columns it
/// reads without pre-binding what it binds for itself (an aggregating
/// projection's seed; REL8-2023 issue 64).
pub fn correlated_reads(query: &Query, out: &mut HashSet<String>) {
    let mut reads = HashSet::new();
    exists_outer_vars(query, &mut reads);
    let mut bound = HashSet::new();
    walk_query_with_roles(
        query,
        &mut |clause| {
            for path in clause_patterns(clause) {
                bound.extend(
                    pattern_anchor_variables(path)
                        .into_iter()
                        .map(|n| n.to_string()),
                );
            }
        },
        &mut |_, _| {},
    );
    out.extend(reads.into_iter().filter(|name| !bound.contains(name)));
}
