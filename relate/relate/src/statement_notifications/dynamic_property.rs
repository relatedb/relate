//! The 03N95 dynamic-property notification (ADR-0363 decisions 3/5): a
//! pattern variable carrying a dynamic property-key predicate whose label
//! (or relationship type) holds an index no static predicate of that
//! variable can serve. [`super::Facts`] owns this accumulator: the
//! `MATCH` clauses of one scope part observe into it, and
//! [`super::Facts::flush_dynamic`] fires it at each scope boundary (a
//! plan part, a nested body) and at the statement's end — one
//! notification per scope part. This module is its own file because the detector is
//! catalog-fed (typed index facts), unlike the clause-shape detectors of
//! [`super::shape`].
//!
//! Firing rule and text verified against live Neo4j 2026.10.1 (probed
//! 2026-11-07, 25 corpus scenarios and edge probes fire and suppress
//! exactly as the reference):
//!
//! - the label predicate may sit in the pattern or in the `WHERE`, but
//!   only positive evidence counts — a negated label predicate (`NOT
//!   n:Person`, or a `!` sub-term of a pattern label expression) never
//!   selects an index (probed);
//! - a static read of an indexed property of the same variable suppresses
//!   — a read anywhere in the predicate counts, including a right-hand-side
//!   value position (`n.name = n.alias` suppresses for an index on
//!   `name`), while a read inside a dynamic key expression never does
//!   (`n['k' + n.name]` is a value read of `name`);
//! - a static read on a different variable does not suppress, and a name
//!   bound by a comprehension, quantifier or reduce inside the predicate
//!   shadows the pattern variable (its reads attribute to nothing);
//! - a node variable never selects a relationship-type index of the same
//!   spelling, nor the mirror (the entity kind is the index's own);
//! - an inline pattern-element predicate is a predicate scope even on an
//!   anonymous element, and a quantified group's own `WHERE` is one too;
//! - one notification per scope part, naming the label the candidate
//!   filter actually matched on the first candidate index in the
//!   catalog's name order (live names the first-created index — a
//!   one-row divergence recorded in the spec harness's re-pin note).

use std::collections::BTreeSet;

use indexmap::IndexMap;
use relate_executor::IndexFacts;
use relate_ir::{
    walk_pattern_payloads, walk_scope_children, ChildScope, Expr, LabelExpr, PathPattern,
    PatternPayload,
};
use relate_model::wire::{Notification, NotificationKind};
use relate_model::EntityKind;
use relate_names::SmallName;

use super::resolve_names_with;

/// The dynamic-property facts of one scope part (one plan part: the
/// statement, or one `UNION` arm / `CALL { … }` body — a part is the
/// boundary at which pattern variables are distinct): every `MATCH`
/// clause of the part accumulates here, and [`Self::fire`] runs once per
/// part. One collection — an insertion-ordered map — holds the variables
/// (declaration order is the fire order, the name key is the O(1)
/// lookup; two tables over one vocabulary would have to stay in step).
#[derive(Default)]
pub(super) struct DynamicPropertyFacts {
    variables: IndexMap<SmallName, VariableFacts>,
}

/// One pattern variable's facts.
struct VariableFacts {
    /// The variable's positive label (node) or type (relationship) names.
    names: BTreeSet<String>,
    /// The element's entity kind — the typed discriminator the candidate
    /// filter compares against the index's own kind.
    kind: EntityKind,
    /// Whether any predicate subexpression accesses this variable by a
    /// dynamic property key.
    dynamic: bool,
    /// The variable's static property reads outside dynamic key
    /// expressions — each is a predicate an index could serve.
    static_reads: BTreeSet<String>,
}

impl DynamicPropertyFacts {
    /// One `MATCH` clause's contributions. Two passes over the patterns:
    /// every element is declared (so a predicate read never depends on
    /// declaration order) before any predicate scope is observed.
    pub(super) fn observe_match(&mut self, patterns: &[PathPattern], where_clause: Option<&Expr>) {
        for pattern in patterns {
            walk_pattern_payloads(pattern, &mut |payload| self.declare_element(payload));
        }
        for pattern in patterns {
            walk_pattern_payloads(pattern, &mut |payload| self.observe_element(payload));
        }
        self.observe_scopes(where_clause, None);
    }

    /// The fire check, once per scope part: the first declared variable
    /// with a dynamic use, a label/type name carrying an index of its own
    /// entity kind, and no static read of a candidate index's property —
    /// one notification, naming the label the filter matched on the first
    /// candidate index in the catalog's name order.
    pub(super) fn fire(&self, notifications: &mut Vec<Notification>, indexes: &[IndexFacts]) {
        if indexes.is_empty() {
            return;
        }
        for facts in self.variables.values() {
            if !facts.dynamic {
                continue;
            }
            // The entity kind is the index's own: a node variable never
            // selects a relationship-type index of the same spelling
            // (live 2026.10.1 does not cross kinds, probed 2026-11-07).
            // The filter yields the label it matched, so the description
            // names what the candidate actually is; the one candidate set
            // serves both the naming and the suppression scan.
            let candidates: Vec<(&IndexFacts, &str)> = indexes
                .iter()
                .filter_map(|index| {
                    (EntityKind::from_relationship_flag(index.relationship) == facts.kind)
                        .then(|| {
                            index
                                .labels
                                .iter()
                                .find(|label| facts.names.contains(*label))
                        })
                        .flatten()
                        .map(|label| (index, label.as_str()))
                })
                .collect();
            let Some((_index, label)) = candidates.first() else {
                continue;
            };
            // Suppression: a static read of a candidate index's property on
            // this variable — the index can still serve that predicate.
            let served = candidates.iter().any(|(index, _)| {
                index
                    .properties
                    .iter()
                    .any(|p| facts.static_reads.contains(p))
            });
            if served {
                continue;
            }
            let named = match facts.kind {
                EntityKind::Relationship => "indexed relationship type",
                EntityKind::Node => "indexed label",
            };
            notifications.push(Notification::new(
                NotificationKind::DynamicProperty,
                format!(
                    "Using a dynamic property makes it impossible to use an index \
                     lookup for this query ({named} is: (:{label}))"
                ),
            ));
            return;
        }
    }

    /// The declaration pass: attribute positive names to the named
    /// elements (an anonymous element has none to attribute; its
    /// predicate scope is observed in the second pass).
    fn declare_element(&mut self, payload: PatternPayload<'_>) {
        match payload {
            PatternPayload::Node(node) => {
                let Some(variable) = node.variable.as_ref() else {
                    return;
                };
                self.declare(
                    variable,
                    EntityKind::Node,
                    node.label_expr.as_ref(),
                    &node.labels,
                );
            }
            PatternPayload::Relationship(rel) => {
                let Some(variable) = rel.variable.as_ref() else {
                    return;
                };
                self.declare(
                    variable,
                    EntityKind::Relationship,
                    rel.type_expr.as_ref(),
                    &rel.types,
                );
            }
            // A quantified group binds no variable of its own; its
            // per-iteration `WHERE` is observed in the second pass.
            PatternPayload::Quantified(_) => {}
        }
    }

    /// Find-or-declare one variable and attribute its slot's positive
    /// names: the label/type expression's positive sub-terms when the
    /// rewrite produced one, else the flat list — the one slot
    /// precedence, but the evidence set, not the mention set (a `!`
    /// sub-term or a negated predicate is not evidence the variable
    /// carries the label).
    fn declare(
        &mut self,
        variable: &SmallName,
        kind: EntityKind,
        name_expr: Option<&LabelExpr>,
        flat_names: &[SmallName],
    ) {
        let facts = self
            .variables
            .entry(variable.clone())
            .or_insert_with(|| VariableFacts {
                names: BTreeSet::new(),
                kind,
                dynamic: false,
                static_reads: BTreeSet::new(),
            });
        facts.kind = kind;
        resolve_names_with(
            name_expr,
            flat_names,
            &mut facts.names,
            positive_label_names,
        );
    }

    /// The observation pass: every element's inline predicate and property
    /// map are predicate scopes — on anonymous elements too, since a
    /// pattern-element `WHERE` may read an outer variable — and a
    /// quantified group's own `WHERE` is one too.
    fn observe_element(&mut self, payload: PatternPayload<'_>) {
        match payload {
            PatternPayload::Node(node) => {
                self.observe_scopes(node.where_clause.as_ref(), node.properties.as_ref());
            }
            PatternPayload::Relationship(rel) => {
                self.observe_scopes(rel.where_clause.as_ref(), rel.properties.as_ref());
            }
            PatternPayload::Quantified(quantified) => {
                self.observe_scopes(quantified.where_clause.as_ref(), None);
            }
        }
    }

    /// One element's present predicate scopes.
    fn observe_scopes(&mut self, where_clause: Option<&Expr>, properties: Option<&Expr>) {
        for scope in [where_clause, properties].into_iter().flatten() {
            self.observe_predicate(scope, false, false, &[]);
        }
    }

    /// The one lookup helper every predicate arm reads through; `None`
    /// when the subexpression is not a bare pattern variable of this
    /// part, or its name is shadowed by a binding inside the predicate.
    fn variable_facts_mut(
        &mut self,
        expr: &Expr,
        shadowed: &[SmallName],
    ) -> Option<&mut VariableFacts> {
        let Expr::Variable(variable) = expr else {
            return None;
        };
        if shadowed.contains(variable) {
            return None;
        }
        self.variables.get_mut(variable)
    }

    /// One predicate scope's contributions, classified at walk time: a
    /// property read inside a dynamic key expression is a value read (the
    /// `in_key` flag carries the subtree distinction, so no counter is
    /// reconciled afterwards); a read anywhere else is a predicate an
    /// index could serve; a positive label predicate contributes its
    /// names (the `negated` flag); a dynamic key access marks its base
    /// variable. Children walk scope-aware
    /// ([`walk_scope_children`]): a comprehension, quantifier or reduce
    /// body binds its own identifiers, which shadow the pattern variables
    /// for the body's reads.
    fn observe_predicate(
        &mut self,
        expr: &Expr,
        in_key: bool,
        negated: bool,
        shadowed: &[SmallName],
    ) {
        match expr {
            // A dynamic property-key access in predicate position: the
            // lowered `Subscript` over a bare variable (`n['k' + n.name]`)
            // or the `DynamicProperty` form (SET/REMOVE targets). A
            // subscript over anything else (`n.tags[0]`) is element
            // access, not a dynamic key.
            Expr::DynamicProperty { expr, key } | Expr::Subscript { expr, index: key } => {
                if let Some(facts) = self.variable_facts_mut(expr, shadowed) {
                    facts.dynamic = true;
                }
                self.observe_predicate(key, true, negated, shadowed);
                self.observe_predicate(expr, in_key, negated, shadowed);
            }
            Expr::Property { expr, key } => {
                if !in_key {
                    if let Some(facts) = self.variable_facts_mut(expr, shadowed) {
                        facts.static_reads.insert(key.to_string());
                    }
                }
                self.observe_predicate(expr, in_key, negated, shadowed);
            }
            Expr::LabelPredicate {
                expr, label_expr, ..
            } => {
                // A WHERE label predicate contributes even when the pattern
                // element itself names no label — but only when it is not
                // negated, and only its positive sub-terms.
                if !negated {
                    if let Some(facts) = self.variable_facts_mut(expr, shadowed) {
                        positive_label_names(label_expr, &mut facts.names);
                    }
                }
                self.observe_predicate(expr, in_key, negated, shadowed);
            }
            Expr::Unary { op, expr } => {
                let negated = negated || matches!(op, relate_ir::UnaryOp::Not);
                self.observe_predicate(expr, in_key, negated, shadowed);
            }
            other => {
                // Scope-aware children: a binding scope's identifiers
                // shadow the pattern variables inside that subtree only;
                // an enclosing child sees the parent's shadow set as-is.
                walk_scope_children(other, &mut |child, scope| match scope {
                    ChildScope::Enclosing => {
                        self.observe_predicate(child, in_key, negated, shadowed);
                    }
                    ChildScope::Element(bound) => {
                        let mut inner = shadowed.to_vec();
                        inner.extend(bound);
                        self.observe_predicate(child, in_key, negated, &inner);
                    }
                });
            }
        }
    }
}

/// The positive label names of a label expression — every sub-term except
/// those under a `Not` (a negated predicate is not evidence the variable
/// carries the label; live 2026.10.1 fires only on positive evidence,
/// probed 2026-11-07). [`super::collect_label_names`] is the mention set
/// the unknown-name detectors want; this is the evidence set this
/// detector wants.
fn positive_label_names(expr: &LabelExpr, out: &mut BTreeSet<String>) {
    match expr {
        LabelExpr::Label(name) => {
            out.insert(name.to_string());
        }
        LabelExpr::Not(_) | LabelExpr::Wildcard | LabelExpr::Dynamic { .. } => {}
        LabelExpr::And(items) | LabelExpr::Or(items) => {
            for item in items {
                positive_label_names(item, out);
            }
        }
    }
}
