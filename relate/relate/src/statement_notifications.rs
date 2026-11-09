//! Statement-scoped query notifications (ADR-0363 decisions 3, 5, 6 and 13).
//!
//! This module owns the kinds that have no operator to hang on: an unknown
//! label, relationship type or property key, a deprecated function or
//! procedure, and the scope-less `CALL { … }` subquery. Derivation runs only
//! where a plan tree is built — [`crate::explain::explain_result`] and
//! [`crate::explain::profile_result`] — and [`merge`] joins the result with
//! the tree's operator-scoped entries in the ADR-0363 decision 6 order:
//!
//! 1. the tree's pre-order entries (the `Id` column's order);
//! 2. statement-scoped entries by category (`UNRECOGNIZED` before
//!    `DEPRECATION`), then code, then description;
//! 3. identical `(kind, description)` pairs collapse to the first for
//!    name-based kinds, through the one
//!    [`crate::notifications::dedup_identical`] helper; occurrence-scoped
//!    kinds (a deprecated call, a clause-shaped deprecation, a per-pattern
//!    hint) keep every occurrence.
//!
//! Unknown-name evidence is **read-side only** (ADR-0363 decision 13): the
//! labels, relationship types and pattern property keys of `MATCH` patterns
//! (including patterns nested in pattern predicates, comprehensions and
//! `shortestPath`) and read property accesses. A `CREATE`/`MERGE` target
//! pattern and a `SET`/`REMOVE` property target contribute nothing — those
//! names are the write's own output — while a read on a write's right-hand
//! side still counts.
//!
//! The sources are the facts the existing surfaces already expose — never a
//! second vocabulary:
//!
//! - **Labels and relationship types** read the view's catalog
//!   ([`ExecGraph::has_label`] / [`ExecGraph::has_rel_type`]): the committed
//!   store catalog plus the names an explicit transaction's scratch has
//!   written. Every write records a first-seen name, every restore path
//!   (checkpoint rows and WAL redo) replays it, and the catalog never
//!   removes a name — so a miss proves the name has never existed in this
//!   view and a warning can never fire for a known one.
//! - **Property keys** read the view's interner
//!   ([`ExecGraph::has_property_key`]) — the committed interner plus scratch
//!   writes — a **superset** of the keys
//!   `db.propertyKeys` reports. `db.propertyKeys` walks every stored node
//!   and relationship; the interner is fed by the same write path. Every
//!   stored key is interned by `CompactProps::from_parts` on CREATE/SET/
//!   MERGE, and the checkpoint/WAL restore paths reconstruct properties
//!   through the same interner, so a key the store has never held is
//!   absent and a **miss is a true miss**. The converse does not hold:
//!   labels, relationship types and the internal relationship `ts` key
//!   share the table, and a key removed from every entity stays interned.
//!   A hit therefore suppresses a warning (a false negative at worst) but
//!   can never produce a false `UnknownPropertyKeyWarning`. That one-sided
//!   guarantee is exactly what this diagnostic needs: it must never warn
//!   about a key the store holds.
//! - **Deprecated functions and procedures** read
//!   [`relate_executor::builtin_function_deprecation`] and
//!   [`relate_executor::builtin_procedure_deprecation`] — the
//!   `deprecated_by` replacements behind `SHOW FUNCTIONS` and
//!   `SHOW PROCEDURES` (`None` means not deprecated). A user-registered
//!   procedure of the same name shadows the built-in exactly as
//!   `validate_calls` does.
//! - **The scope-less subquery** is the lowered `CallScope` without a scope
//!   clause (`!CallScope::has_scope_clause()`); the scoped forms
//!   (`CALL (n) { … }`, `CALL () { … }`) never fire.
//!
//! `DeprecatedFunction` and `DeprecatedProcedure` map to the vocabulary's
//! `FeatureDeprecationWarning` 01N01 rows; a catalog entry is deprecated
//! exactly when it names a `deprecated_by` replacement. The vocabulary has
//! no 01N02 row yet, so a replacement-less entry cannot be expressed
//! (ADR-0363 decision 5).
//!
//! The clause-shape detectors added for LABS-711 cover the deprecated
//! projection subclause references (`ORDER BY`/`WHERE` ambiguous and
//! complex references), the `CREATE` self-reference, repeated relationship
//! variables, unsatisfiable relationship type expressions, subquery
//! variable shadowing, `SEARCH` identifier shadowing, and the
//! `shortestPath` unbounded/exhaustive cases. The `USING INDEX` /
//! `USING JOIN ON` hint detectors report each hint no plan can serve
//! (`01N31`/`01N30`), one row per hint.

mod dynamic_property;
mod hints;
mod shape;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use relate_executor::{
    builtin_function_deprecation, builtin_procedure_deprecation, DeprecatedBy, DeprecationForm,
    ExecGraph, IndexFacts,
};
use relate_ir::{
    expr_vars, walk_pattern_payloads, Clause, Expr, ExprRole, LabelExpr, MapProjectionItem,
    NodePattern, PathPattern, PatternPayload, RelPattern,
};
use relate_model::wire::{Notification, NotificationCategory, NotificationKind};
use relate_names::{SmallName, Token};
use relate_planner::LogicalPlan;

use crate::notifications::dedup_identical;
use crate::plan_node::pattern_has_unbounded_expansion;

/// Merge the tree's operator-scoped entries with this module's
/// statement-scoped entries (ADR-0363 decision 6): operator entries keep
/// their pre-order first, statement entries sort by category, code and
/// description, and identical `(kind, description)` pairs collapse to the
/// first occurrence for name-based kinds. The tree's half is already
/// deduplicated by [`crate::plan_node::tree_notifications`], so only the
/// statement half takes the second pass. The one identity helper is
/// [`crate::notifications::dedup_identical`].
pub(crate) fn merge(
    operator: Vec<Notification>,
    mut statement: Vec<Notification>,
) -> Vec<Notification> {
    statement.sort_by(|a, b| statement_order_key(a).cmp(&statement_order_key(b)));
    let mut merged = operator;
    merged.extend(dedup_identical(statement));
    merged
}

/// The statement-scoped entries of one planned statement (ADR-0363
/// decision 5). The plan is walked through the planner's canonical
/// part/stage walk and the IR's exhaustive clause-tree walk, so `NEXT`
/// stages, `UNION` arms, `CALL { … }` bodies, `WHEN` branches, `FOREACH`
/// bodies and expression subqueries all contribute.
pub(crate) fn statement_notifications(
    plan: &LogicalPlan,
    view: &ExecGraph<'_>,
    params: &BTreeMap<String, relate_core::Value>,
) -> relate_core::Result<Vec<Notification>> {
    // The IR walk takes two callbacks; collect the visited clauses and
    // expression roles first so the one `Facts` accumulator is borrowed by
    // a single fold below (no duplicated traversal, no interior mutability
    // just to split the two callback borrows).
    let mut part_clauses: Vec<Vec<(&relate_ir::Clause, bool)>> = Vec::new();
    let mut exprs = Vec::new();
    let mut parts = Vec::new();
    // A clause is nested when it sits inside a `CALL { … }` body, a `WHEN`
    // branch or an expression subquery. The walk reports that as data on its
    // clause callback (never inferred from callback order), so a walker
    // change cannot silently reclassify clauses. The operator-scoped
    // cartesian warning already covers the top-level `MATCH`, so only the
    // nested ones need the statement-scoped detector.
    //
    // The clauses are collected per plan part (the statement, or one
    // `UNION` arm / `NEXT` stage): a part is one scope boundary at which
    // the 03N95 accumulator flushes; a nested body is the other — every
    // nested clause observes into a fresh accumulator (an approximation
    // that errs by not merging two `MATCH` clauses of one nested body,
    // never by merging two scopes; the walk reports nesting, not body
    // identity, and importing a body-identity walk is deferred).
    relate_planner::walk_plan_parts(plan, &mut |part| {
        parts.push(part);
        let mut group = Vec::new();
        relate_ir::walk_clauses_with_roles(
            part,
            &mut |clause, nested| group.push((clause, nested)),
            &mut |expr, role| exprs.push((expr, role)),
        );
        part_clauses.push(group);
    });
    let mut facts = Facts::default();
    shape::observe_subquery_scopes(&mut facts.notifications, &parts);
    // One typed-index snapshot per statement (catalog state is
    // statement-scoped, not part- or clause-scoped): every part's fire
    // reads the same slice.
    let indexes = view.index_facts()?;
    for group in part_clauses {
        let mut previous_nested = false;
        for (clause, nested) in group {
            // A scope boundary: entering or leaving a nested body, or
            // moving between two nested clauses (which the walk cannot
            // tell apart as same or different bodies).
            if nested != previous_nested || nested {
                facts.flush_dynamic(&indexes);
            }
            previous_nested = nested;
            facts.observe_clause(clause, view, nested, params, &indexes)?;
        }
        facts.flush_dynamic(&indexes);
    }
    for (expr, role) in exprs {
        facts.observe_expr(expr, role);
    }
    Ok(facts.finish(view))
}

/// The ADR-0363 decision 6 statement-scoped sort key: category rank,
/// then code, then instance description.
fn statement_order_key(notification: &Notification) -> (u8, &'static str, &str) {
    (
        category_rank(notification.category()),
        notification.code(),
        notification.description(),
    )
}

/// `UNRECOGNIZED` before `DEPRECATION` before `SCHEMA` before `HINT`.
/// Performance kinds are operator-scoped and never reach this list;
/// ranking them last keeps the order total and deterministic if a future
/// classification does.
fn category_rank(category: NotificationCategory) -> u8 {
    match category {
        NotificationCategory::Unrecognized => 0,
        NotificationCategory::Deprecation => 1,
        NotificationCategory::Schema => 2,
        NotificationCategory::Hint => 3,
        NotificationCategory::Performance => 4,
    }
}

/// The names and deprecations one statement mentions, accumulated before
/// the catalog/interner lookups decide which names are unknown.
#[derive(Default)]
struct Facts {
    labels: BTreeSet<String>,
    rel_types: BTreeSet<String>,
    property_keys: BTreeSet<String>,
    /// The deprecation notifications, already formed; [`merge`] orders and
    /// deduplicates them.
    notifications: Vec<Notification>,
    /// The 03N95 detector's statement-wide accumulator: every `MATCH`
    /// clause observes into it, and `finish` fires it once.
    dynamic: dynamic_property::DynamicPropertyFacts,
}

impl Facts {
    /// One clause's contributions: a `MATCH` pattern's read payloads, a
    /// `CALL` deprecation, and the scope-less `CALL { … }` subquery. A
    /// `CREATE`/`MERGE` target pattern contributes nothing (ADR-0363
    /// decision 13): its names are the write's own output.
    fn observe_clause(
        &mut self,
        clause: &Clause,
        view: &ExecGraph<'_>,
        nested: bool,
        params: &BTreeMap<String, relate_core::Value>,
        indexes: &[IndexFacts],
    ) -> relate_core::Result<()> {
        // Schema DDL notes (`00NA0`-`00NA2`) read the same catalog facts
        // their execution gates read; the executor's one entry dispatches
        // every clause shape (the catalog passthrough and the IR's own
        // compact `CreateIndex`/`DropIndex`), so no clause shape is
        // re-assembled across the crate boundary.
        self.notifications
            .extend(relate_executor::ddl_notifications(view, clause, params)?);
        match clause {
            Clause::Match {
                patterns,
                where_clause,
                using_index,
                using_join,
                ..
            } => {
                for pattern in patterns {
                    self.observe_pattern(pattern);
                }
                shape::observe_match(
                    &mut self.notifications,
                    patterns,
                    where_clause.as_ref(),
                    nested,
                );
                self.dynamic.observe_match(patterns, where_clause.as_ref());
                // The hint kinds have no scope boundary to accumulate
                // across (each hint is its own clause's own fact), so
                // they fire here against the statement's index snapshot.
                hints::observe(&mut self.notifications, using_index, using_join, indexes);
            }
            Clause::With {
                distinct,
                items,
                order,
                where_clause,
                ..
            } => shape::observe_projection(
                &mut self.notifications,
                items,
                order,
                where_clause.as_ref(),
                *distinct,
            ),
            Clause::Return {
                distinct,
                items,
                order,
                ..
            } => shape::observe_projection(&mut self.notifications, items, order, None, *distinct),
            Clause::Create { patterns } => shape::observe_create(&mut self.notifications, patterns),
            Clause::Call { name, .. } => self.observe_call(view, name),
            // A MERGE property map reading the entity it merges
            // (`MERGE (a {prop: a.prop})`) is the deprecated self-reference
            // (MergeAcceptance [6]). The shape is a deprecation fact, not
            // unknown-name evidence, so it is observed here even though a
            // MERGE target pattern contributes no name evidence (decision
            // 13).
            Clause::Merge { pattern, .. } => {
                if let Some(name) = Self::merge_self_reference(pattern) {
                    self.notifications.push(Notification::new(
                        NotificationKind::MergeSelfReference,
                        format!(
                            "Merging an entity (`{name}`) and referencing that entity in a \
                             property definition in the same MERGE is deprecated."
                        ),
                    ));
                }
            }
            Clause::CallSubquery { scope, .. } => {
                if !scope.has_scope_clause() {
                    self.notifications.push(Notification::new(
                        NotificationKind::CallSubqueryWithoutScope,
                        "The `CALL { … }` subquery has no variable scope clause.",
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The variable a MERGE pattern reads from its own property map
    /// (`MERGE (a {prop: a.prop})`), if any. MERGE patterns are simple,
    /// non-quantified paths, so the compact node/relationship views hold
    /// every payload exactly once.
    fn merge_self_reference(pattern: &PathPattern) -> Option<String> {
        let elements = pattern
            .nodes
            .iter()
            .map(|node| (node.variable.as_ref(), node.properties.as_ref()))
            .chain(
                pattern
                    .rels
                    .iter()
                    .map(|rel| (rel.variable.as_ref(), rel.properties.as_ref())),
            );
        for (variable, properties) in elements {
            let (Some(variable), Some(properties)) = (variable, properties) else {
                continue;
            };
            let mut vars = HashSet::new();
            expr_vars(properties, &mut vars);
            if variable.with_str(|text| vars.contains(text)) {
                return Some(variable.to_string());
            }
        }
        None
    }

    /// One expression's contributions. Unknown-name evidence — property
    /// keys and nested read patterns — comes only from a read expression;
    /// a `SET`/`REMOVE` target or a `CREATE`/`MERGE` pattern payload is a
    /// write target (ADR-0363 decision 13). Deprecations and the scoped
    /// `CALL` fact are clause-shape facts, not name evidence, so they are
    /// observed in every role.
    fn observe_expr(&mut self, expr: &Expr, role: ExprRole) {
        match expr {
            Expr::Property { key, .. } if role == ExprRole::Read => {
                self.property_keys.insert(key.to_string());
            }
            Expr::Function { name, .. } => {
                if let Some(replacement) = name.with_str(builtin_function_deprecation) {
                    self.notifications.push(
                        Notification::new(
                            NotificationKind::DeprecatedFunction,
                            format!("The function `{name}()` is deprecated."),
                        )
                        .with_remedy(deprecation_remedy(replacement)),
                    );
                }
            }
            // `expr { .key }` is a property access the expression walker
            // does not flatten into `Expr::Property`.
            Expr::MapProjection { items, .. } if role == ExprRole::Read => {
                for item in items {
                    if let MapProjectionItem::Property { key } = item {
                        self.property_keys.insert(key.to_string());
                    }
                }
            }
            Expr::PatternPred { path } | Expr::PatternComp { path, .. }
                if role == ExprRole::Read =>
            {
                self.observe_pattern(path)
            }
            // `shortestPath(...)` used as an expression has no operator to
            // carry the unbounded-pattern warning the MATCH-pattern form
            // gets, so it is raised here.
            Expr::ShortestPath { path, .. } if role == ExprRole::Read => {
                self.observe_pattern(path);
                if pattern_has_unbounded_expansion(path) {
                    self.notifications.push(Notification::new(
                        NotificationKind::UnboundedVariableLengthPattern,
                        "A shortest-path pattern is unbounded.",
                    ));
                }
            }
            _ => {}
        }
    }

    /// A built-in `CALL` deprecation. A registered procedure shadows the
    /// built-in for validation and execution, so it shadows its
    /// deprecation fact too; a non-deprecated or unknown name contributes
    /// nothing.
    fn observe_call(&mut self, view: &ExecGraph<'_>, name: &SmallName) {
        if name.with_str(|text| view.procedure(text)).is_some() {
            return;
        }
        let Some(replacement) = name.with_str(builtin_procedure_deprecation) else {
            return;
        };
        self.notifications.push(
            Notification::new(
                NotificationKind::DeprecatedProcedure,
                format!("The procedure `{name}` is deprecated."),
            )
            .with_remedy(deprecation_remedy(replacement)),
        );
    }

    /// One read path pattern's node and relationship payloads, through the
    /// IR's one post-rewrite payload visitor: it owns the elements-vs-compact
    /// precedence (so a `WHERE` equality or label predicate the planner
    /// folded into the compact view is seen) and the quantified-group
    /// recursion, so no payload is visited twice.
    fn observe_pattern(&mut self, pattern: &PathPattern) {
        walk_pattern_payloads(pattern, &mut |payload| match payload {
            PatternPayload::Node(node) => self.observe_node(node),
            PatternPayload::Relationship(rel) => self.observe_rel(rel),
            PatternPayload::Quantified(_) => {}
        });
    }

    fn observe_node(&mut self, node: &NodePattern) {
        resolve_names(node.label_expr.as_ref(), &node.labels, &mut self.labels);
        collect_pattern_property_keys(node.properties.as_ref(), &mut self.property_keys);
    }

    fn observe_rel(&mut self, rel: &RelPattern) {
        resolve_names(rel.type_expr.as_ref(), &rel.types, &mut self.rel_types);
        collect_pattern_property_keys(rel.properties.as_ref(), &mut self.property_keys);
    }

    /// Fire and reset the 03N95 accumulator — at a scope boundary (a plan
    /// part or a nested body) or the statement's end, whichever comes
    /// first.
    fn flush_dynamic(&mut self, indexes: &[IndexFacts]) {
        self.dynamic.fire(&mut self.notifications, indexes);
        self.dynamic = dynamic_property::DynamicPropertyFacts::default();
    }

    /// Turn the accumulated names into notifications: only names the view
    /// does not know, plus the already-formed deprecations. The three
    /// catalog-resolved classes are one table, so each class names its
    /// predicate, kind and noun once.
    fn finish(self, view: &ExecGraph<'_>) -> Vec<Notification> {
        let mut notifications = self.notifications;
        for (names, class) in [
            (self.labels, CatalogName::Label),
            (self.rel_types, CatalogName::RelationshipType),
            (self.property_keys, CatalogName::PropertyKey),
        ] {
            for name in names {
                if !class.exists(view, &name) {
                    notifications.push(Notification::new(
                        class.kind(),
                        format!(
                            "The {} `{name}` does not exist in this database.",
                            class.noun()
                        ),
                    ));
                }
            }
        }
        notifications
    }
}

/// The three name classes the unknown-name loop resolves against the view's
/// catalog: one row per predicate/kind/noun triple.
#[derive(Clone, Copy)]
enum CatalogName {
    Label,
    RelationshipType,
    PropertyKey,
}

impl CatalogName {
    fn exists(self, view: &ExecGraph<'_>, name: &str) -> bool {
        match self {
            CatalogName::Label => view.has_label(name),
            CatalogName::RelationshipType => view.has_rel_type(name),
            CatalogName::PropertyKey => view.has_property_key(name),
        }
    }

    fn kind(self) -> NotificationKind {
        match self {
            CatalogName::Label => NotificationKind::UnknownLabelWarning,
            CatalogName::RelationshipType => NotificationKind::UnknownRelationshipTypeWarning,
            CatalogName::PropertyKey => NotificationKind::UnknownPropertyKeyWarning,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            CatalogName::Label => "label",
            CatalogName::RelationshipType => "relationship type",
            CatalogName::PropertyKey => "property key",
        }
    }
}

/// The static names of a label/type slot: the label expression when the
/// rewrite produced one, else the flat list. One precedence rule for node
/// labels and relationship types, parameterized over the leaf collector —
/// the mention set (`collect_label_names`) for the unknown-name detectors,
/// the positive-evidence set for the 03N95 detector.
fn resolve_names_with(
    expr: Option<&LabelExpr>,
    names: &[(SmallName, Token)],
    out: &mut BTreeSet<String>,
    collect: fn(&LabelExpr, &mut BTreeSet<String>),
) {
    // Text render: the fact sets are the String catalog-comparison domain.
    match expr {
        Some(expr) => collect(expr, out),
        None => out.extend(names.iter().map(|(name, _)| name.to_string())),
    }
}

/// The mention-set spelling of [`resolve_names_with`] — the unknown-name
/// detectors' form.
fn resolve_names(
    expr: Option<&LabelExpr>,
    names: &[(SmallName, Token)],
    out: &mut BTreeSet<String>,
) {
    resolve_names_with(expr, names, out, collect_label_names);
}

/// The instance remedy for a catalog replacement: an identifier/call is
/// rendered as code, a clause is named as a clause, and the catalog's
/// display prose never reaches the backticks.
fn deprecation_remedy(replacement: DeprecatedBy) -> String {
    match replacement.form() {
        DeprecationForm::Identifier(identifier) => format!("Use `{identifier}` instead."),
        DeprecationForm::Clause(clause) => format!("Use the `{clause}` clause instead."),
    }
}

/// Every static label name in a label/type expression, including the names
/// inside `And`/`Or`/`Not` and `NOT :Label` forms. Dynamic (`$(…)`) and
/// wildcard parts name no static label.
pub(super) fn collect_label_names(expr: &LabelExpr, out: &mut BTreeSet<String>) {
    match expr {
        LabelExpr::Label(name) => {
            out.insert(name.to_string());
        }
        LabelExpr::Wildcard | LabelExpr::Dynamic { .. } => {}
        LabelExpr::Not(inner) => collect_label_names(inner, out),
        LabelExpr::And(items) | LabelExpr::Or(items) => {
            for item in items {
                collect_label_names(item, out);
            }
        }
    }
}

/// The keys a pattern property map mentions. A parameter map (`$props`)
/// names no static key and contributes nothing.
fn collect_pattern_property_keys(properties: Option<&Expr>, out: &mut BTreeSet<String>) {
    if let Some(Expr::Map(items)) = properties {
        out.extend(items.iter().map(|(key, _)| key.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decision 6: operator entries keep their pre-order, statement
    /// entries follow in category/code/description order, and identical
    /// pairs collapse to the first. Only the statement half takes the
    /// dedup pass; the tree's half arrived already deduplicated.
    #[test]
    fn merge_orders_statement_entries_after_operator_entries_and_dedups() {
        let operator = vec![
            Notification::new(NotificationKind::CartesianProduct, "first").with_operator_id(0),
            Notification::new(NotificationKind::AllNodesScan, "second").with_operator_id(1),
        ];
        let statement = vec![
            Notification::new(
                NotificationKind::DeprecatedFunction,
                "The function `id()` is deprecated.",
            ),
            Notification::new(
                NotificationKind::UnknownPropertyKeyWarning,
                "The property key `b` does not exist in this database.",
            ),
            Notification::new(
                NotificationKind::UnknownLabelWarning,
                "The label `A` does not exist in this database.",
            ),
            // The same statement entry twice: one survives.
            Notification::new(
                NotificationKind::UnknownLabelWarning,
                "The label `A` does not exist in this database.",
            ),
        ];
        let merged = merge(operator, statement);
        assert_eq!(
            merged.iter().map(Notification::kind).collect::<Vec<_>>(),
            [
                NotificationKind::CartesianProduct,
                NotificationKind::AllNodesScan,
                NotificationKind::UnknownLabelWarning,
                NotificationKind::UnknownPropertyKeyWarning,
                NotificationKind::DeprecatedFunction,
            ]
        );
        assert_eq!(merged.len(), 5);
    }

    /// The merge deduplicates only the statement half: the tree's operator
    /// entries are already deduplicated by `tree_notifications`, so merge
    /// must not take a second pass over them.
    #[test]
    fn merge_leaves_the_operator_half_to_the_tree_dedup() {
        let duplicate = || Notification::new(NotificationKind::AllNodesScan, "first");
        let merged = merge(vec![duplicate(), duplicate()], Vec::new());
        assert_eq!(
            merged.len(),
            2,
            "operator entries arrive deduplicated and are not re-scanned"
        );
    }

    /// A replacement renders by its syntactic form: an identifier/call is
    /// code, a clause is named as a clause, and catalog prose never reaches
    /// the remedy.
    #[test]
    fn deprecation_remedy_renders_identifier_and_clause_forms() {
        assert_eq!(
            deprecation_remedy(DeprecatedBy::identified(
                "elementId",
                "elementId or consider using an application-generated id",
            )),
            "Use `elementId` instead."
        );
        assert_eq!(
            deprecation_remedy(DeprecatedBy::identifier("db.cdc.current")),
            "Use `db.cdc.current` instead."
        );
        assert_eq!(
            deprecation_remedy(DeprecatedBy::clause("SEARCH")),
            "Use the `SEARCH` clause instead."
        );
    }

    /// Every static label name is collected from `And`/`Or`/`Not`; the
    /// wildcard and dynamic forms contribute none.
    #[test]
    fn label_expression_names_are_collected_recursively() {
        let mut names = BTreeSet::new();
        collect_label_names(
            &LabelExpr::And(vec![
                LabelExpr::Label("A".into()),
                LabelExpr::Or(vec![
                    LabelExpr::Label("B".into()),
                    LabelExpr::Not(Box::new(LabelExpr::Label("C".into()))),
                    LabelExpr::Wildcard,
                ]),
            ]),
            &mut names,
        );
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["A", "B", "C"]);
    }
}
