//! MATCH rewrites after bind (ADR-0024).

use std::collections::HashSet;

use relate_ir::{BinaryOp, Clause, Expr, LabelExpr, PathPattern, Projection};
use relate_names::{NamedToken, SmallName, Token};

use crate::order::order_patterns;
use crate::stats::PatternStats;

pub fn rewrite_part(clauses: &mut [Clause], stats: &dyn PatternStats) {
    let mut bound = HashSet::new();
    for clause in clauses {
        match clause {
            Clause::Catalog(_) => {}
            Clause::Match {
                patterns,
                where_clause,
                search,
                ..
            } => {
                if let Some(w) = where_clause.take() {
                    // Only the fold reads the introduced set — a MATCH
                    // without a WHERE never pays for it.
                    let introduced = pattern_introduced_vars(patterns);
                    *where_clause = fold_where(patterns, w, &bound, &introduced);
                }
                bound = order_patterns(patterns, &bound, where_clause.as_ref(), stats);
                if let Some(score_var) =
                    search.as_ref().and_then(|search| search.score_var.as_ref())
                {
                    bound.insert(score_var.to_string());
                }
            }
            Clause::Create { patterns } => {
                for p in patterns {
                    add_pattern_vars(&mut bound, p);
                }
            }
            Clause::Merge { pattern, .. } => add_pattern_vars(&mut bound, pattern),
            Clause::Unwind { alias, .. } => {
                bound.insert(alias.to_string());
            }
            Clause::Call { yield_items, .. } => {
                bound.extend(yield_items.effective_names().map(|name| name.to_string()));
            }
            Clause::With { items, .. } => {
                bound = with_bound(items, &bound);
            }
            Clause::Return { .. }
            | Clause::Delete { .. }
            | Clause::Set { .. }
            | Clause::Remove { .. }
            | Clause::CreateIndex { .. }
            | Clause::DropIndex { .. }
            | Clause::ShowIndexes
            | Clause::Finish
            | Clause::LoadCsv { .. }
            | Clause::CallSubquery { .. }
            | Clause::Conditional { .. }
            | Clause::Foreach { .. }
            | Clause::Use { .. } => {}
        }
    }
}

fn pattern_introduced_vars(patterns: &[PathPattern]) -> HashSet<String> {
    patterns
        .iter()
        .flat_map(relate_ir::path_pattern_vars)
        .map(|name| name.to_string())
        .collect()
}

pub(crate) fn add_pattern_vars(bound: &mut HashSet<String>, p: &PathPattern) {
    // Map-bridge feeds: the rewrite's bound sets stay String-keyed; every
    // IR name renders once, at its push.
    if let Some(v) = &p.variable {
        bound.insert(v.to_string());
    }
    bound.extend(relate_ir::path_pattern_vars(p).map(|name| name.to_string()));
}

fn with_bound(items: &[Projection], prev: &HashSet<String>) -> HashSet<String> {
    let has_star = items.iter().any(|i| matches!(i.expr, Expr::Star));
    let mut next = if has_star {
        prev.clone()
    } else {
        HashSet::new()
    };
    for (i, item) in items.iter().enumerate() {
        if matches!(item.expr, Expr::Star) {
            continue;
        }
        next.insert(item.output_name(i));
    }
    next
}

fn fold_where(
    patterns: &mut [PathPattern],
    expr: Expr,
    bound: &HashSet<String>,
    introduced: &HashSet<String>,
) -> Option<Expr> {
    // One conjunct list for the whole `AND` tree — the flattening this
    // used to do with a fresh `Vec` per leaf (and one per inner node's
    // extend) drains into a single list, then `combine_and` re-associates
    // the kept conjuncts exactly as before.
    let mut conjuncts = Vec::new();
    flatten_into(expr, &mut conjuncts);
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
    introduced: &HashSet<String>,
) -> bool {
    if let Expr::LabelPredicate {
        expr, label_expr, ..
    } = expr
    {
        if let Expr::Variable(v) = expr.as_ref() {
            if v.with_str(|text| introduced.contains(text))
                && !v.with_str(|text| bound.contains(text))
            {
                if let Some(labels) = static_labels(label_expr).filter(|labels| !labels.is_empty())
                {
                    return add_labels(patterns, v, &labels);
                }
            }
        }
        return false;
    }
    let Some((v, key, val)) = eq_prop(expr) else {
        return false;
    };
    if !v.with_str(|text| introduced.contains(text)) || v.with_str(|text| bound.contains(text)) {
        return false;
    }
    if !val.is_seek_const() {
        return false;
    }
    add_prop(patterns, &v, key, val)
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
    // Folding into a pattern is compact-view-only: a GPM pattern's
    // `nodes`/`rels` may be cleared or duplicated beside the `elements`
    // the GPM engine walks, so a label folded into `nodes` could desync
    // from the elements. Such paths are skipped and the conjunct stays
    // in the WHERE clause.
    for p in patterns.iter_mut().filter(|p| !p.needs_gpm()) {
        for n in &mut p.nodes {
            if n.variable.as_ref() != Some(var) {
                continue;
            }
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
    }
    false
}

fn add_prop(patterns: &mut [PathPattern], var: &SmallName, key: NamedToken, val: Expr) -> bool {
    // Same compact-view-only scope as `add_labels` above.
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
        // The test helper owns String lists; labels render once.
        (
            patterns[0].nodes[0]
                .label_names()
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
            "MATCH (n) WHERE n:(A|B) RETURN n",
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
}
