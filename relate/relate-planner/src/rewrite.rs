//! MATCH rewrites after bind (ADR-0024).

use std::collections::HashSet;

use relate_ir::{BinaryOp, Clause, Expr, LabelExpr, NodePattern, PathPattern};
use relate_names::{NamedToken, SmallName, Token};

use crate::order::order_patterns;
use crate::stats::PatternStats;

pub fn rewrite_part(clauses: &mut [Clause], stats: &dyn PatternStats) {
    let introduced = push_match_where_down(clauses);
    let mut scope: Vec<String> = Vec::new();
    let mut bound = HashSet::new();
    for (i, clause) in clauses.iter_mut().enumerate() {
        if let Clause::Match {
            patterns,
            where_clause,
            ..
        } = clause
        {
            if let Some(w) = where_clause.take() {
                // The fold reads the clause's introductions from the
                // pass's recorded list — the one derivation of "what this
                // MATCH introduces" feeds both the scope and the fold.
                *where_clause = fold_where(patterns, w, &bound, &introduced[i]);
            }
            // The ordering pass reorders `patterns` in place; the set it
            // grows stays internal (the caller's binding set advances once,
            // through `advance_scope` below).
            order_patterns(patterns, &bound, where_clause.as_ref(), stats);
        }
        advance_scope(&mut scope, &mut bound, clause, &introduced[i]);
    }
}

/// Move each MATCH-level `WHERE` conjunct to the earliest clause of its run
/// of consecutive non-`OPTIONAL`, non-`SEARCH` MATCH clauses that binds
/// every variable the conjunct reads. Neo4j places such a predicate where
/// its variables become available, and the placement is observable for a
/// `shortestPath()` pattern: a predicate on the selected path must hold
/// *inside* the search, which then answers the shortest path satisfying it
/// instead of filtering the one path it already chose. A filter commutes
/// with an inner-join MATCH, so no other pattern's rows change. A run stops
/// at the first clause of another kind, at an `OPTIONAL MATCH` (its WHERE
/// belongs to the optional match, not to the joined rows), and at a
/// `SEARCH` clause (top-K semantics).
///
/// One binding set decides every placement: after the walk passes clause
/// `e`, the set is exactly what a conjunct placed on `e` may read, so a
/// cumulative per-clause table never exists (that table would make
/// planning quadratic in the clause count), and each parked conjunct's
/// variable set is computed once where it is parked — the per-clause test
/// is a containment check, never a re-walk. Returns each clause's recorded
/// introductions so the caller's fold reads them without re-deriving.
fn push_match_where_down(clauses: &mut [Clause]) -> Vec<Vec<String>> {
    let n = clauses.len();
    let crossing: Vec<bool> = clauses
        .iter()
        .map(|clause| crossing_where(clause).is_some())
        .collect();
    // Per conjunct (in collection order): the clause it moves to, `None`
    // when it stays at its own.
    let mut assignment: Vec<Vec<Option<usize>>> = vec![Vec::new(); n];
    let mut participates = vec![false; n];
    let mut introduced: Vec<Vec<String>> = Vec::with_capacity(n);
    let mut scope: Vec<String> = Vec::new();
    let mut bound = HashSet::new();
    let mut i = 0;
    while i < n {
        if !crossing[i] {
            let names = bindings_of(&clauses[i], &scope);
            advance_scope(&mut scope, &mut bound, &clauses[i], &names);
            introduced.push(names);
            i += 1;
            continue;
        }
        // A run of crossing MATCHes: park its conjuncts first, each with
        // its variable set computed once (source clause, position in its
        // conjunct list, that set), then walk the run once, advancing the
        // one binding set.
        let start = i;
        let mut pending: Vec<(usize, usize, HashSet<String>)> = Vec::new();
        while i < n && crossing[i] {
            let conjuncts = crate::order::where_conjuncts_of(
                crossing_where(&clauses[i]).and_then(Option::as_ref),
            );
            assignment[i] = vec![None; conjuncts.len()];
            for (slot, conjunct) in conjuncts.into_iter().enumerate() {
                let mut vars = HashSet::new();
                relate_ir::expr_vars(conjunct, &mut vars);
                pending.push((i, slot, vars));
            }
            i += 1;
        }
        for e in start..i {
            let names = bindings_of(&clauses[e], &scope);
            advance_scope(&mut scope, &mut bound, &clauses[e], &names);
            introduced.push(names);
            // Earliest covering clause: the walk reaches it first — a
            // conjunct leaves the pending list at the first clause of its
            // run whose available set covers every variable it reads. A
            // conjunct with no variables never moves (it holds or fails
            // at every clause alike, so its placement stays the author's
            // own).
            pending.retain_mut(|(source, slot, vars)| {
                if *source > e && !vars.is_empty() && vars.iter().all(|name| bound.contains(name)) {
                    assignment[*source][*slot] = Some(e);
                    participates[*source] = true;
                    participates[e] = true;
                    false
                } else {
                    true
                }
            });
        }
    }
    commit_where_assignment(clauses, assignment, participates);
    introduced
}

/// The commit half of the push-down: every participating clause's WHERE is
/// taken and rebuilt once — its surviving conjuncts plus the ones routed
/// to it, in the collection order the placement walk numbered. Walked in
/// reverse: a conjunct only ever moves to an earlier clause, so every
/// source above `i` has already routed into `incoming[i]` by the time `i`
/// rebuilds.
fn commit_where_assignment(
    clauses: &mut [Clause],
    assignment: Vec<Vec<Option<usize>>>,
    participates: Vec<bool>,
) {
    let n = clauses.len();
    let mut incoming: Vec<Vec<Expr>> = vec![Vec::new(); n];
    for i in (0..n).rev() {
        if !participates[i] {
            continue;
        }
        let mut all = Vec::new();
        if let Some(expr) = crossing_where_mut(&mut clauses[i]).and_then(Option::take) {
            let mut conjuncts = Vec::new();
            flatten_into(expr, &mut conjuncts);
            for (slot, conjunct) in conjuncts.into_iter().enumerate() {
                // One assignment slot per conjunct: the placement walk
                // numbered this same list — a divergence is a bug, not a
                // silently kept conjunct.
                assert!(
                    slot < assignment[i].len(),
                    "one assignment slot per conjunct"
                );
                if let Some(target) = assignment[i][slot] {
                    incoming[target].push(conjunct);
                } else {
                    all.push(conjunct);
                }
            }
        }
        all.append(&mut incoming[i]);
        let where_clause = crossing_where_mut(&mut clauses[i])
            .expect("a participating clause is a crossing MATCH");
        *where_clause = combine_and(all);
    }
}

/// One clause's introductions, read through the canonical enumerator —
/// bind already validated the query, so its error is an invariant here.
/// The lazy scope is the pass's ordered running scope: the subquery arm
/// seeds on it in declaration order.
fn bindings_of(clause: &Clause, scope: &[String]) -> Vec<String> {
    crate::names::clause_bindings(clause, || scope.to_vec()).expect("bind validated the query")
}

/// Advance the pass's scope pair past one clause from its recorded
/// introductions — the one fold every pass in this module shares. The
/// ordered list is the subquery seed's declaration order; the set is the
/// membership index the placement tests read. `WITH` replaces both
/// through the canonical projection fold.
fn advance_scope(
    scope: &mut Vec<String>,
    bound: &mut HashSet<String>,
    clause: &Clause,
    introduced: &[String],
) {
    match clause {
        Clause::With { items, .. } => {
            *scope = crate::names::projected_scope(items, std::mem::take(scope));
            *bound = scope.iter().cloned().collect();
        }
        _ => {
            for name in introduced {
                if bound.insert(name.clone()) {
                    scope.push(name.clone());
                }
            }
        }
    }
}

/// A push-down run's crossing MATCH: neither `OPTIONAL` (its WHERE
/// belongs to the optional match, not to the joined rows) nor `SEARCH`
/// (top-K semantics). The one spelling of the rule — the run guard, the
/// placement reads and the write phase all go through this pair, which
/// hands back the WHERE slot of the MATCH it proves crossable.
fn crossing_where(clause: &Clause) -> Option<&Option<Expr>> {
    match clause {
        Clause::Match {
            optional: false,
            search: None,
            where_clause,
            ..
        } => Some(where_clause),
        _ => None,
    }
}

fn crossing_where_mut(clause: &mut Clause) -> Option<&mut Option<Expr>> {
    match clause {
        Clause::Match {
            optional: false,
            search: None,
            where_clause,
            ..
        } => Some(where_clause),
        _ => None,
    }
}

fn fold_where(
    patterns: &mut [PathPattern],
    expr: Expr,
    bound: &HashSet<String>,
    introduced: &[String],
) -> Option<Expr> {
    // One conjunct list for the whole `AND` tree — the flattening this
    // used to do with a fresh `Vec` per leaf (and one per inner node's
    // extend) drains into a single list, then `combine_and` re-associates
    // the kept conjuncts exactly as before.
    let mut conjuncts = Vec::new();
    flatten_into(expr, &mut conjuncts);
    // The label-conjunction folds run before the disjunction folds: a
    // companion label conjunct beside a disjunction must land in the
    // node's conjunction FIRST — folded after the `Or`, its
    // `static_labels` normalization declines the non-conjunction
    // expression and the scan loses the conjunction's narrowing, so
    // statement order must not decide whether `WHERE (p:A OR p:B) AND
    // p:C` scans `:C` (LABS-1083 review: the fold is order-independent).
    let defers_to_disjunction_pass = |conjunct: &Expr| {
        or_label_predicates(conjunct).is_some()
            || matches!(
                conjunct,
                Expr::LabelPredicate { label_expr, .. }
                    if static_label_disjunction(label_expr).is_some()
            )
    };
    conjuncts.retain(|conjunct| {
        defers_to_disjunction_pass(conjunct)
            || !fold_conjunct(patterns, conjunct, bound, introduced)
    });
    conjuncts.retain(|conjunct| !fold_conjunct(patterns, conjunct, bound, introduced));
    combine_and(conjuncts)
}

/// Flatten an owned `AND` tree into `out`, left to right — the conjunct
/// order `fold_where` folds and keeps in.
fn flatten_into(expr: Expr, out: &mut Vec<Expr>) {
    match expr {
        Expr::Binary {
            op: BinaryOp::And,
            left,
            right,
        } => {
            flatten_into(*left, out);
            flatten_into(*right, out);
        }
        other => out.push(other),
    }
}

fn combine_and(parts: Vec<Expr>) -> Option<Expr> {
    parts.into_iter().reduce(|acc, p| Expr::Binary {
        op: BinaryOp::And,
        left: Box::new(acc),
        right: Box::new(p),
    })
}

fn fold_conjunct(
    patterns: &mut [PathPattern],
    expr: &Expr,
    bound: &HashSet<String>,
    introduced: &[String],
) -> bool {
    // The fold-eligibility gate, one spelling for every arm below: the
    // variable is introduced by this clause's patterns and not bound
    // earlier — the parenthesized, WHERE-spelled, and property folds
    // consult the same predicate and can never disagree about it.
    let introduced_unbound = |v: &SmallName| {
        v.with_str(|text| introduced.iter().any(|name| name.as_str() == text))
            && !v.with_str(|text| bound.contains(text))
    };
    if let Expr::LabelPredicate {
        expr, label_expr, ..
    } = expr
    {
        if let Expr::Variable(v) = expr.as_ref() {
            if introduced_unbound(v) {
                if let Some(labels) = static_labels(label_expr).filter(|labels| !labels.is_empty())
                {
                    return add_labels(patterns, v, &labels);
                }
                if let Some(disjuncts) = static_label_disjunction(label_expr) {
                    return add_label_disjunction(patterns, v, &disjuncts);
                }
            }
        }
        return false;
    }
    // The WHERE-spelled disjunction of plain label predicates on one
    // introduced, unbound variable folds exactly like the parenthesized
    // form above — the recognizer answers the one variable and its
    // disjuncts (an `Or` tree of same-variable label predicates always
    // carries at least two leaves), never a mixed bag the caller must
    // re-validate.
    if let Some((v, disjuncts)) = or_label_predicates(expr) {
        if introduced_unbound(&v) {
            return add_label_disjunction(patterns, &v, &disjuncts);
        }
        return false;
    }
    let Some((v, key, val)) = eq_prop(expr) else {
        return false;
    };
    if !introduced_unbound(&v) {
        return false;
    }
    if !val.is_seek_const() {
        return false;
    }
    add_prop(patterns, &v, key, val)
}

/// The one variable and its disjunct labels of an `Or` tree whose every
/// leaf is a plain label predicate on that same variable — the WHERE
/// spelling `p:A OR p:B OR p:C` (the parenthesized `p:(A|B)` lowers to
/// one LabelPredicate with an `Or` label expression instead, handled
/// beside `static_labels`). A bare label predicate is never a
/// disjunction of its own — it is the conjunction fold's conjunct — so
/// only an `Or` tree answers, and it always carries at least two
/// leaves: the caller never re-checks arity or variables.
fn or_label_predicates(expr: &Expr) -> Option<(SmallName, Vec<SmallName>)> {
    match expr {
        Expr::Binary {
            op: BinaryOp::Or, ..
        } => collect_or_leaves(expr),
        _ => None,
    }
}

/// The recursive collector behind [`or_label_predicates`]: every leaf is
/// a same-variable plain label predicate, nested `Or`s join, and any
/// other shape declines the whole tree — validated at the leaf join.
fn collect_or_leaves(expr: &Expr) -> Option<(SmallName, Vec<SmallName>)> {
    match expr {
        Expr::Binary {
            op: BinaryOp::Or,
            left,
            right,
        } => {
            let (variable, mut disjuncts) = collect_or_leaves(left)?;
            let (right_var, right_disjuncts) = collect_or_leaves(right)?;
            if variable != right_var {
                return None;
            }
            disjuncts.extend(right_disjuncts);
            Some((variable, disjuncts))
        }
        Expr::LabelPredicate {
            expr, label_expr, ..
        } => {
            let (Expr::Variable(v), LabelExpr::Label(label)) = (expr.as_ref(), label_expr) else {
                return None;
            };
            Some((v.clone(), vec![label.clone()]))
        }
        _ => None,
    }
}

/// The flat label set of a static disjunction of plain labels
/// (`A|B`, `(A|B)|C` — the WHERE-spelled `n:A OR n:B OR n:C` lowers to the
/// same tree), or `None` when the expression is anything else — any `And`
/// node (the mixed And/Or tree would need DNF flattening under a shape
/// cap, deliberately not attempted here), negation, wildcard or dynamic
/// leaf makes the whole expression unfoldable. Derived from the IR's one
/// arm classifier ([`LabelExpr::static_disjunction_arms`] — the same
/// vocabulary the executor's union serve and the plan narration read)
/// with the fold's own conservatism stated beside it: a conjunction base
/// beside the `Or` is the pattern's post-fold shape, not a foldable
/// predicate (folding its disjuncts alone would drop the base), and a
/// one-arm `Or` is identity — both decline. The two classifiers are
/// disjoint: `static_labels` answers conjunctions, this one answers
/// disjunctions of plain labels only.
fn static_label_disjunction(expr: &LabelExpr) -> Option<Vec<SmallName>> {
    let arms = expr.static_disjunction_arms()?;
    if !arms.base().is_empty() || arms.len() < 2 {
        return None;
    }
    Some(
        arms.disjuncts()
            .iter()
            .map(|label| (*label).clone())
            .collect(),
    )
}

/// The folds' node-targeting walk, the one spelling of the scaffolding
/// every fold shares (the label conjunction, the label disjunction, the
/// property map): compact-view-only — a GPM pattern's `nodes`/`rels` may
/// be cleared or duplicated beside the `elements` the GPM engine walks,
/// so a fold landing in `nodes` could desync from them; such patterns
/// are skipped and the conjunct stays in the WHERE clause. The walk
/// yields every node carrying the variable (a cyclic pattern spells one
/// variable on several entries); the fold that lands decides, and what
/// each fold builds from the node stays at the fold (the conjunction
/// fold keeps the plan-resolved tokens, the disjunction fold composes
/// label expressions) — the walk is the shared policy, never the
/// payload.
fn fold_target_nodes<'a>(
    patterns: &'a mut [PathPattern],
    var: &SmallName,
) -> impl Iterator<Item = &'a mut NodePattern> {
    // The variable is cloned into the filter so the returned iterator
    // borrows nothing but the patterns (an `impl Iterator` return cannot
    // carry the argument reference's lifetime).
    let var = var.clone();
    patterns
        .iter_mut()
        .filter(|p| !p.needs_gpm())
        .flat_map(|p| p.nodes.iter_mut())
        .filter(move |n| n.variable.as_ref() == Some(&var))
}

/// Fold a static label disjunction on an introduced, unbound variable
/// into the pattern node's label expression: the node keeps its own
/// static conjunction in `labels` (a disjunction is not a conjunction
/// label) and its `label_expr` becomes that conjunction `AND` the folded
/// `Or`. Semantics-preserving for every statement — the pattern matcher
/// evaluates `label_expr` per candidate exactly as the WHERE predicate
/// evaluated it per row — and it is what lets the seek arm later serve
/// the disjunction from per-label indexes instead of a full scan.
fn add_label_disjunction(
    patterns: &mut [PathPattern],
    var: &SmallName,
    disjuncts: &[SmallName],
) -> bool {
    for n in fold_target_nodes(patterns, var) {
        // Same faithfulness guard as `add_labels`: a non-static existing
        // label expression is left alone (this candidate declines, a
        // later one may still take the fold). The base stays names —
        // one list the entailment test and the final expression both
        // read, never wrapped in `LabelExpr::Label`s only to be
        // re-discriminated below.
        let base: Vec<SmallName> = match n.label_expr.as_ref() {
            None => n.label_names().cloned().collect(),
            Some(expr) => match static_labels(expr) {
                Some(labels) => labels,
                None => continue,
            },
        };
        // One disjunct the node already carries as a conjunction
        // label makes the whole `Or` always true under the node's own
        // labels — the predicate folds away entirely (the polarity
        // inverts from the conjunction fold's per-member drop).
        let entailed = disjuncts.iter().any(|d| base.contains(d));
        if entailed {
            return true;
        }
        let or = LabelExpr::Or(disjuncts.iter().cloned().map(LabelExpr::Label).collect());
        n.label_expr = Some(if base.is_empty() {
            or
        } else {
            LabelExpr::And(base.into_iter().map(LabelExpr::Label).chain([or]).collect())
        });
        return true;
    }
    false
}

/// The flat label set of a static conjunction (`A`, `A&B`, `(A&B)&C`), or
/// `None` when the expression is anything else — a disjunction, negation,
/// wildcard or dynamic leaf anywhere in the tree makes the whole expression
/// unfoldable. Gated on [`LabelExpr::is_static_conjunction`], the one
/// predicate for "a conjunction of plain labels" and the only place it is
/// consulted: callers ask this helper, never the raw predicate.
///
/// Flattening leaf by leaf and comparing the collected labels against the
/// item count used to accept `(A&B)&!(B&C)` as `{A, B}` (the inner `And`
/// contributed two labels for one item), folded the pattern to those two
/// labels and dropped the predicate -- so the negation never applied.
fn static_labels(expr: &LabelExpr) -> Option<Vec<SmallName>> {
    match expr {
        LabelExpr::Label(label) => Some(vec![label.clone()]),
        LabelExpr::And(items) if expr.is_static_conjunction() => {
            // `is_static_conjunction` guarantees every item is static too,
            // so the recursion cannot drop one.
            items
                .iter()
                .map(static_labels)
                .collect::<Option<Vec<_>>>()
                .map(|parts| parts.concat())
        }
        _ => None,
    }
}

fn eq_prop(expr: &Expr) -> Option<(SmallName, NamedToken, Expr)> {
    let Expr::Binary {
        op: BinaryOp::Eq,
        left,
        right,
    } = expr
    else {
        return None;
    };
    // Eq is operand-symmetric — no op to mirror, so only the shared
    // side-selection core applies.
    // The recognizer keeps the key beside its plan-resolved token; the
    // pair is cloned to owned at this one construction.
    relate_ir::orient_side(relate_ir::var_prop_named, left, right)
        .map(|((v, k), other)| (v.clone(), k, other.clone()))
}

fn add_labels(patterns: &mut [PathPattern], var: &SmallName, labels: &[SmallName]) -> bool {
    for n in fold_target_nodes(patterns, var) {
        // The rebuild below replaces `label_expr` with the pattern's
        // own static label set plus the folded conjunct, so it is only
        // faithful when that set holds every label the pattern needs: a
        // disjunction/negation/wildcard/dynamic expression
        // (`MATCH (n:A|B) WHERE n:C`) would be narrowed to `{C}`.
        let mut folded = match n.label_expr.as_ref() {
            None => n.labels.clone(),
            // A label the pattern already resolved keeps its token.
            Some(expr) => match static_labels(expr) {
                Some(labels) => labels
                    .into_iter()
                    .map(|label| {
                        let token = n
                            .labels
                            .iter()
                            .find(|(x, _)| *x == label)
                            .map_or(Token::Unresolved, |(_, token)| *token);
                        (label, token)
                    })
                    .collect(),
                None => continue,
            },
        };
        // A label folded in from the WHERE has no token yet: resolution
        // ran before this rewrite and no resolver is in scope here, so
        // consumers look it up by name (`Token::Unresolved`).
        for l in labels {
            if !folded.iter().any(|(x, _)| x == l) {
                folded.push((l.clone(), Token::Unresolved));
            }
        }
        n.labels = folded;
        n.label_expr = Some(LabelExpr::And(
            n.label_names().cloned().map(LabelExpr::Label).collect(),
        ));
        return true;
    }
    false
}

fn add_prop(patterns: &mut [PathPattern], var: &SmallName, key: NamedToken, val: Expr) -> bool {
    // Same compact-view-only scope as the label folds
    // ([`fold_target_nodes`]'s doc) — spelled here rather than shared,
    // because this fold targets relationships too (a variable may name
    // one), and a relationship's match inside its pattern comes after
    // that pattern's nodes.
    for p in patterns.iter_mut().filter(|p| !p.needs_gpm()) {
        for n in &mut p.nodes {
            if n.variable.as_ref() == Some(var) {
                // Map-bridge feed: IR map keys are value-domain
                // (runtime `Value::Map`), rendered once here.
                return merge_prop_map(&mut n.properties, &mut n.prop_keys, key, val);
            }
        }
        for r in &mut p.rels {
            if r.variable.as_ref() == Some(var) {
                return merge_prop_map(&mut r.properties, &mut r.prop_keys, key, val);
            }
        }
    }
    false
}

/// Fold one `var.key = val` conjunct into the element's property map;
/// `true` means folded (the caller drops it from the WHERE). A map that
/// already carries `key` keeps the conjunct in the WHERE instead: the map
/// holds one value per key, and dropping the conjunct silently discarded a
/// conflicting predicate — `MATCH (n {p: 1}) WHERE n.p = 2` matched `p = 1`
/// (LABS-1086 review; Neo4j matches nothing).
fn merge_prop_map(
    slot: &mut Option<Expr>,
    keys: &mut Vec<NamedToken>,
    key: NamedToken,
    val: Expr,
) -> bool {
    let name = key.0.to_string();
    match slot {
        Some(Expr::Map(pairs)) => {
            if pairs.iter().any(|(k, _)| k == &name) {
                return false;
            }
            pairs.push((name, val));
            keys.push(key);
            true
        }
        None => {
            *slot = Some(Expr::Map(vec![(name, val)]));
            keys.push(key);
            true
        }
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lower `cypher`, run the MATCH rewrite, and return the first pattern
    /// node's flat label list plus the WHERE clause that survived.
    fn rewritten(cypher: &str) -> (Vec<String>, Option<Expr>) {
        let (_, labels, where_clause) = rewritten_with_label_expr(cypher);
        (labels, where_clause)
    }

    /// The same rewrite, also surfacing the node's complete label
    /// expression (rendered) — the disjunction fold's target.
    fn rewritten_with_label_expr(cypher: &str) -> (Option<String>, Vec<String>, Option<Expr>) {
        let mut query = relate_front::parse_and_lower(cypher).unwrap();
        rewrite_part(&mut query.parts[0].clauses, &crate::NoPatternStats);
        let Clause::Match {
            patterns,
            where_clause,
            ..
        } = &query.parts[0].clauses[0]
        else {
            panic!("{cypher}: expected MATCH");
        };
        let node = &patterns[0].nodes[0];
        // The test helper owns String lists; labels render once.
        (
            node.label_expr.as_ref().map(|expr| format!("{expr:?}")),
            node.label_names()
                .map(|label| label.to_string())
                .collect::<Vec<_>>(),
            where_clause.clone(),
        )
    }

    #[test]
    fn static_label_conjunction_folds_into_the_pattern() {
        let (labels, where_clause) = rewritten("MATCH (n:A) WHERE n:B RETURN n");
        assert_eq!(labels, vec!["A".to_string(), "B".to_string()]);
        assert!(where_clause.is_none());
    }

    /// A negation or disjunction anywhere in the label expression makes it
    /// unfoldable: folding `(A&B)&!(B&C)` to `{A, B}` and dropping the
    /// predicate made it match `(:A:B:C)` as well (TCK
    /// LabelExpressionAcceptance / IsLabeledPredicateAcceptance
    /// `(A&B)&!(B&C)`).
    #[test]
    fn non_conjunctive_label_expression_is_not_folded() {
        for cypher in [
            "MATCH (n) WHERE n:(A&B)&!(B&C) RETURN n",
            "MATCH (n) WHERE n:(A&B)&!(C) RETURN n",
            "MATCH (n) WHERE n:(A|B)&!(C) RETURN n",
            "MATCH (n) WHERE n:!(A) RETURN n",
        ] {
            let (labels, where_clause) = rewritten(cypher);
            assert!(labels.is_empty(), "{cypher}");
            assert!(where_clause.is_some(), "{cypher}");
        }
    }

    /// Folding a conjunct into a pattern that already carries a
    /// disjunction must leave that disjunction in place: rebuilding the
    /// expression from the folded labels alone narrowed
    /// `MATCH (n:A|B) WHERE n:C` to `(:C)`.
    #[test]
    fn fold_keeps_the_patterns_own_label_expression() {
        let (labels, where_clause) = rewritten("MATCH (n:A|B) WHERE n:C RETURN n");
        assert!(labels.is_empty(), "{labels:?}");
        assert!(where_clause.is_some());
    }

    /// The variables of each MATCH clause's surviving WHERE, in clause
    /// order — the push-down's observable placement.
    fn clause_where_vars(cypher: &str) -> Vec<Option<HashSet<String>>> {
        let mut query = relate_front::parse_and_lower(cypher).unwrap();
        rewrite_part(&mut query.parts[0].clauses, &crate::NoPatternStats);
        query.parts[0]
            .clauses
            .iter()
            .map(|clause| match clause {
                Clause::Match { where_clause, .. } => where_clause.as_ref().map(|expr| {
                    let mut vars = HashSet::new();
                    relate_ir::expr_vars(expr, &mut vars);
                    vars
                }),
                _ => None,
            })
            .collect()
    }

    /// A predicate moved to the earlier clause applies where its variables
    /// become available — observable for `shortestPath()`, whose search
    /// must choose the path against a predicate on it. A conjunct that also
    /// reads its own clause's variables stays.
    #[test]
    fn match_where_conjuncts_move_to_the_earliest_clause_that_binds_them() {
        let clauses =
            clause_where_vars("MATCH (a) MATCH (b) WHERE a.x > 1 AND b.y > 2 RETURN a, b");
        assert_eq!(
            clauses,
            vec![
                Some(HashSet::from(["a".to_string()])),
                Some(HashSet::from(["b".to_string()])),
                None, // the RETURN clause
            ]
        );
    }

    /// An `OPTIONAL MATCH`'s WHERE belongs to the optional match, not to
    /// the joined rows, so nothing is pushed across it.
    #[test]
    fn match_where_push_down_stops_at_optional_match() {
        let clauses = clause_where_vars("MATCH (a) OPTIONAL MATCH (b) WHERE a.x > 1 RETURN a, b");
        assert_eq!(clauses[0], None);
        assert!(clauses[1].is_some());
    }

    /// A static label disjunction on an introduced, unbound variable folds
    /// into the pattern node's label expression — the keyed-OR shape
    /// (`(p:Model OR p:Source) AND p.id = $d`) stops being a plain WHERE
    /// filter and becomes a pattern-carried label set the seek arm can
    /// serve from per-label indexes.
    #[test]
    fn static_label_disjunction_folds_into_the_label_expression() {
        let (label_expr, labels, where_clause) = rewritten_with_label_expr(
            "MATCH (p) WHERE (p:Model OR p:Source) AND p.id = $d RETURN p",
        );
        assert_eq!(
            label_expr.as_deref(),
            Some("Or([Label(Model), Label(Source)])")
        );
        assert!(
            labels.is_empty(),
            "a disjunction is not a conjunction label: {labels:?}"
        );
        assert!(
            where_clause.is_none(),
            "the label predicate folded out of the WHERE clause"
        );
    }

    /// The node's own labels conjoin with the folded disjunction:
    /// `MATCH (p:C) WHERE p:A OR p:B` is `C AND (A|B)`, never `(C|A|B)`.
    #[test]
    fn folded_disjunction_conjoins_with_the_patterns_own_labels() {
        let (label_expr, labels, where_clause) =
            rewritten_with_label_expr("MATCH (p:C) WHERE p:A OR p:B RETURN p");
        assert_eq!(
            label_expr.as_deref(),
            Some("And([Label(C), Or([Label(A), Label(B)])])")
        );
        assert_eq!(labels, vec!["C".to_string()]);
        assert!(where_clause.is_none());
    }

    /// The fold is conjunct-order-independent (LABS-1083 review): a
    /// label conjunct beside a label disjunction composes into the same
    /// `And([C, Or([A, B])])` whether it reads before or after the
    /// `Or` — folded after, the conjunction must still land in the
    /// node, or the scan loses the `:C` narrowing.
    #[test]
    fn the_fold_is_conjunct_order_independent() {
        for cypher in [
            "MATCH (p) WHERE (p:A OR p:B) AND p:C RETURN p",
            "MATCH (p) WHERE p:C AND (p:A OR p:B) RETURN p",
        ] {
            let (label_expr, labels, where_clause) = rewritten_with_label_expr(cypher);
            assert_eq!(
                label_expr.as_deref(),
                Some("And([Label(C), Or([Label(A), Label(B)])])"),
                "{cypher}: both orders compose the same shape"
            );
            assert_eq!(labels, vec!["C".to_string()], "{cypher}");
            assert!(where_clause.is_none(), "{cypher}");
        }
    }

    /// One entailed disjunct makes the whole `Or` always true under the
    /// node's own labels: `MATCH (p:C) WHERE p:C OR p:D` matches every
    /// `:C` node, so the predicate folds away entirely — never narrowed
    /// to `:C:D`. Both spellings pin the fold-away.
    #[test]
    fn any_entailed_disjunction_folds_to_nothing() {
        for cypher in [
            "MATCH (p:C) WHERE p:C OR p:D RETURN p",
            "MATCH (p:C) WHERE p:C OR p:C RETURN p",
        ] {
            let (label_expr, labels, where_clause) = rewritten_with_label_expr(cypher);
            assert_eq!(
                label_expr.as_deref(),
                Some("Label(C)"),
                "{cypher}: the pattern's own label expression stands"
            );
            assert_eq!(labels, vec!["C".to_string()], "{cypher}");
            assert!(where_clause.is_none(), "{cypher}");
        }
    }

    /// Mixed And/Or label trees do not fold (DNF flattening is not
    /// attempted); the predicate stays in the WHERE clause.
    #[test]
    fn mixed_and_or_label_expression_is_not_folded() {
        let (label_expr, _, where_clause) = rewritten_with_label_expr(
            "MATCH (p) WHERE (p:A OR (p:B AND p:C)) AND p.id = $d RETURN p",
        );
        assert!(label_expr.is_none());
        assert!(where_clause.is_some());
    }
}
