//! OpenCypher AST (ADR-0007): the statement and clause model, with the pure
//! `Expr`-shape recognizers beside it. The expression model itself
//! ([`Expr`] and its two carried types) lives in [`expr`], the pattern
//! model ([`PathPattern`] and its element, label and type vocabularies) in
//! [`pattern`], and both are re-exported here, so the public path stays
//! `ast::*`.

use relate_names::SmallName;

use crate::search::SearchSpec;

mod call;
mod expr;
mod pattern;
pub use call::*;
pub use expr::*;
pub use pattern::*;

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub parts: Vec<SingleQuery>,
    /// `union_all[i]` applies between `parts[i]` and `parts[i+1]`.
    pub union_all: Vec<bool>,
    /// CIP-213 sequential statement composition.
    pub next: Option<Box<Query>>,
}

impl Query {
    /// The end of this query's `NEXT` chain — its true terminal stage
    /// (CIP-213), not its own first stage. UNION arms are `parts` *inside*
    /// one `Query`, so the chain walk needs no union guard. The one
    /// IR-level spelling of the cursor the planner's projection checks and
    /// the executor's stage handling mirror.
    pub fn final_stage(&self) -> &Query {
        let mut tail = self;
        while let Some(next) = tail.next.as_deref() {
            tail = next;
        }
        tail
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SingleQuery {
    pub clauses: Vec<Clause>,
}

impl SingleQuery {
    /// The clause a CALL subquery body opens with, when it is an
    /// *importing WITH*: the first non-`USE` clause, and only if it is an
    /// explicit `WITH` (`LET`/`FILTER` desugar to `With` but are ordinary
    /// projections, never importing WITHs). One derivation for both the
    /// planner's 42I28 rule and the notification layer's import-set
    /// analysis; the typed payload saves every caller re-discriminating the
    /// clause variant.
    pub fn leading_importing_with(&self) -> Option<LeadingWith<'_>> {
        let clause = self
            .clauses
            .iter()
            .find(|clause| !matches!(clause, Clause::Use { .. }))?;
        let Clause::With {
            distinct,
            items,
            order,
            skip,
            limit,
            where_clause,
            origin: WithOrigin::Explicit,
            ..
        } = clause
        else {
            return None;
        };
        Some(LeadingWith {
            distinct: *distinct,
            items,
            order,
            skip: skip.as_ref(),
            limit: limit.as_ref(),
            where_clause: where_clause.as_ref(),
        })
    }

    /// The names this part's importing WITH pulls in from `outer`. The one
    /// home for the import policy: a bare item imports its variable
    /// ([`Projection::imported_name`]), a `*` imports the whole outer
    /// scope, and anything else (the aliased forms 42I28 rejects) imports
    /// nothing. Returns an empty set when the part has no importing WITH.
    pub fn leading_imports(
        &self,
        outer: &std::collections::BTreeSet<String>,
    ) -> std::collections::BTreeSet<String> {
        use std::collections::BTreeSet;
        let Some(with) = self.leading_importing_with() else {
            return BTreeSet::new();
        };
        let mut imports = BTreeSet::new();
        for item in with.items {
            if matches!(item.expr, Expr::Star) {
                imports.extend(outer.iter().cloned());
            } else if let Some(name) = item.imported_name() {
                // Map-bridge: the import sets stay String-keyed because
                // their consumer, the notification layer's import-set
                // analysis (`statement_notifications/shape.rs`), is a
                // String scope-domain sink; the typed sink switch is
                // tracked as LABS-936.
                imports.insert(name.to_string());
            }
        }
        imports
    }
}

/// The typed payload of [`SingleQuery::leading_importing_with`]: the
/// importing WITH's fields, borrowed from the clause.
pub struct LeadingWith<'a> {
    pub distinct: bool,
    pub items: &'a [Projection],
    pub order: &'a [OrderItem],
    pub skip: Option<&'a Expr>,
    pub limit: Option<&'a Expr>,
    pub where_clause: Option<&'a Expr>,
}

/// A `CALL … YIELD` tail: no `YIELD` at all, `YIELD *`, or an explicit
/// list of (procedure output name, optional alias) pairs. The typed shape
/// replaces the old `"*"`-string sentinel, so a real column named `*`
/// cannot be confused with the wildcard.
#[derive(Debug, Clone, PartialEq)]
pub enum YieldItems {
    /// No `YIELD` clause: the procedure's default yields apply.
    Default,
    /// `YIELD *`: every output the procedure declares.
    Star,
    /// An explicit `YIELD a [AS x], b ...` list, in source order.
    Named(Vec<(SmallName, Option<SmallName>)>),
}

impl YieldItems {
    /// The explicit items, or an empty slice for `Default`/`Star`.
    pub fn items(&self) -> &[(SmallName, Option<SmallName>)] {
        match self {
            YieldItems::Named(items) => items,
            YieldItems::Default | YieldItems::Star => &[],
        }
    }

    /// Raw output names of an explicit list, in order (empty otherwise).
    pub fn names(&self) -> impl Iterator<Item = &SmallName> {
        self.items().iter().map(|(name, _)| name)
    }

    /// Each explicit item's raw name and effective (alias-else-name) name.
    pub fn effective_pairs(&self) -> impl Iterator<Item = (&SmallName, &SmallName)> {
        self.items()
            .iter()
            .map(|(name, alias)| (name, alias.as_ref().unwrap_or(name)))
    }

    /// Effective (post-alias) output names of an explicit list.
    pub fn effective_names(&self) -> impl Iterator<Item = &SmallName> {
        self.effective_pairs().map(|(_, effective)| effective)
    }

    /// Whether any `YIELD` tail was written (`Star` or `Named`).
    pub fn has_yield(&self) -> bool {
        !matches!(self, YieldItems::Default)
    }

    /// The yield tail resolved against a procedure's `default` output
    /// columns: `YIELD *` takes every default (the same as no `YIELD`),
    /// an explicit list is itself. The one classification for callers that
    /// need the resolved pairs — never a boolean collapsed to `items()`
    /// (Star's items are empty by definition).
    pub fn resolved_pairs(&self, defaults: &[&str]) -> Vec<(String, Option<String>)> {
        // Map-bridge: the executor's procedure-call column handling stays
        // String-keyed (catalog-facing output columns), so both arms render
        // text here — the one feed the stage-2 scope-map change (ADR-0509)
        // does not touch.
        match self {
            YieldItems::Named(items) => items
                .iter()
                .map(|(name, alias)| {
                    (
                        name.to_string(),
                        // allocation-free read of the optional alias
                        alias.as_ref().map(SmallName::to_string),
                    )
                })
                .collect(),
            YieldItems::Star | YieldItems::Default => defaults
                .iter()
                .map(|column| (column.to_string(), None))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Clause {
    /// Relate's rich catalog/admin AST, retained losslessly by the
    /// Relate front-end and executed against Relate metadata.
    Catalog(Box<relate_parser::Clause>),
    Match {
        optional: bool,
        match_mode: MatchMode,
        patterns: Vec<PathPattern>,
        where_clause: Option<Expr>,
        /// CIP-79 `USING INDEX [SEEK]` hints, carried for the plan-time
        /// notification that reports a hint no catalog index can serve
        /// (`01N31`, ADR-0363); the planner itself steers nothing by them.
        using_index: Vec<relate_parser::IndexHint>,
        /// CIP-79 `USING JOIN ON` hints, carried for the same purpose: the
        /// planner never fulfils one, so the `01N30` notification reports
        /// each.
        using_join: Vec<relate_parser::JoinHint>,
        /// CIP-224/CIP-240 vector index search attached to this MATCH.
        search: Option<Box<SearchClause>>,
    },
    Create {
        patterns: Vec<PathPattern>,
    },
    Merge {
        pattern: PathPattern,
        on_create: Vec<SetItem>,
        on_match: Vec<SetItem>,
    },
    Call {
        name: SmallName,
        args: CallArgs,
        yield_items: YieldItems,
        optional: bool,
        where_clause: Option<Expr>,
    },
    Delete {
        detach: bool,
        expressions: Vec<Expr>,
    },
    Set {
        items: Vec<SetItem>,
    },
    Remove {
        items: Vec<RemoveItem>,
    },
    With {
        distinct: bool,
        /// CIP-236: explicit grouping keys (Some(vec![]) = `GROUP BY ()`
        /// empty set; None = infer from the non-aggregating items).
        group_by: Option<Vec<Expr>>,
        items: Vec<Projection>,
        order: Vec<OrderItem>,
        skip: Option<Expr>,
        limit: Option<Expr>,
        where_clause: Option<Expr>,
        origin: WithOrigin,
    },
    Unwind {
        expression: Expr,
        alias: SmallName,
    },
    Return {
        distinct: bool,
        /// CIP-236 explicit grouping keys (same encoding as `With`).
        group_by: Option<Vec<Expr>>,
        items: Vec<Projection>,
        order: Vec<OrderItem>,
        skip: Option<Expr>,
        limit: Option<Expr>,
    },
    /// Named `(label, property)` catalog entry (ADR-0040).
    CreateIndex {
        name: Option<String>,
        if_not_exists: bool,
        label: SmallName,
        property: SmallName,
    },
    DropIndex {
        if_exists: bool,
        target: DropIndexTarget,
    },
    ShowIndexes,
    /// CIP-140: terminal empty projection (0 columns).
    Finish,
    /// `LOAD CSV [WITH HEADERS] FROM … AS …`.
    LoadCsv {
        with_headers: bool,
        url: Expr,
        variable: SmallName,
        field_terminator: Option<String>,
    },
    /// `CALL { … }` / `OPTIONAL CALL { … }` (CIP-141 / CIP-32).
    CallSubquery {
        query: Box<Query>,
        optional: bool,
        scope: CallScope,
        in_transactions: Option<InTransactions>,
    },
    /// CIP-135 top-level WHEN/THEN/ELSE.
    Conditional {
        branches: Vec<(Expr, Query)>,
        else_query: Option<Box<Query>>,
    },
    /// Updating FOREACH loop.
    Foreach {
        variable: SmallName,
        list: Expr,
        clauses: Vec<Clause>,
    },
    /// Single-graph runtime: validates scope, then acts as a no-op.
    Use {
        graph: Expr,
    },
}

/// Native Relate representation of a SEARCH subclause (CIP-224, the
/// CIP-278 `<source specification clause>` extension).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchClause {
    pub binding_var: SmallName,
    /// CIP-278 `<source specification clause>`: one index search, or a
    /// `COMBINE` of two or more weighted sources fused with WRRF.
    pub spec: SearchSpec,
    pub score_var: Option<SmallName>,
    /// The MATCH's pattern goes beyond CIP-225 Rule 0's plain shape (the
    /// `complex-search-patterns` family) -- classified once at lowering by
    /// the same shape test the preview gate rejects. The executor's top-K
    /// rule keys on it: a plain pattern's `LIMIT` is a hard candidate
    /// window, with the MATCH's outer `WHERE`, label or score predicate
    /// post-filtering, while a complex pattern's `LIMIT` counts the
    /// candidates the whole MATCH keeps.
    pub complex_pattern: bool,
}

/// `DROP INDEX name` or `DROP INDEX ON :Label(prop)` (ADR-0040).
#[derive(Debug, Clone, PartialEq)]
pub enum DropIndexTarget {
    Name(String),
    Schema {
        label: SmallName,
        property: SmallName,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub expr: Expr,
    pub alias: Option<SmallName>,
    /// Verbatim source text of the unaliased projection expression, as
    /// captured by the parser. Used by [`Projection::output_name`] as the
    /// column name for expression shapes without a dedicated label
    /// (`count{()}`, list/map literals, `CASE`, etc.). Synthetic
    /// projections built after parsing leave this `None`.
    pub source_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WithOrigin {
    #[default]
    Explicit,
    Let,
    Filter,
    /// The horizon a standalone `<order by and page statement>` lowers to:
    /// a `WITH *` carrying the clause's `ORDER BY`/`SKIP`/`LIMIT`.
    ///
    /// Distinct from [`WithOrigin::Explicit`] because the two are not the
    /// same clause to a reader of the plan — a body opening with a page
    /// statement imports nothing *by name*, while a user-written `WITH *`
    /// does pull the enclosing scope in. Live keeps them apart
    /// (`CALL { ORDER BY 1 RETURN 1 AS x }` is accepted, with the
    /// scopeless-subquery deprecation; `CALL { WITH * ORDER BY 1 RETURN 1
    /// AS x }` is the importing-`WITH` rejection), and this origin is what
    /// lets the rules see that difference after lowering.
    Page,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderItem {
    pub expr: Expr,
    pub ascending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetItem {
    /// `SET n.k = v` / `SET n[expr] = v` -- one property assignment; the
    /// target is always a property access (`Expr::Property` /
    /// `Expr::DynamicProperty`).
    Property { target: Expr, value: Expr },
    /// `SET n = <map>` -- full property replacement; the target is always
    /// a bare variable. The front mints this where the parser already
    /// keeps the fact (`SetItem.property` is `None`), so no consumer
    /// re-derives the split by sniffing the target expression's shape.
    ReplaceAll { target: Expr, value: Expr },
    /// `SET n += <map>` -- merge the map into the target's existing
    /// properties; the target is always a bare variable (the parser only
    /// sets `merge` on the bare-variable form).
    MergeMap { target: Expr, value: Expr },
    Labels {
        target: Expr,
        labels: Vec<LabelExpr>,
    },
    LabelsFunction {
        target: Expr,
        value: Expr,
        op: LabelsFunctionOp,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelsFunctionOp {
    Overwrite,
    Extend,
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RemoveItem {
    Property {
        target: Expr,
    },
    Labels {
        target: Expr,
        labels: Vec<LabelExpr>,
    },
}

/// The `<var>.<key>` property access shape: the variable's and the
/// property key's borrowed names, or `None` for any other expression.
/// The seek/fold/vector operand recognizers derive from here.
pub fn var_prop(expr: &Expr) -> Option<(&SmallName, &SmallName)> {
    let Expr::Property { expr, key, .. } = expr else {
        return None;
    };
    let Expr::Variable(v) = expr.as_ref() else {
        return None;
    };
    Some((v, key))
}

/// [`var_prop`] with the key as the shared [`relate_names::NamedToken`]:
/// the written name beside its plan-resolved token, for the executor reads
/// that skip the by-name interner lookup.
pub fn var_prop_named(expr: &Expr) -> Option<(&SmallName, relate_names::NamedToken)> {
    let Expr::Property { key_id, .. } = expr else {
        return None;
    };
    var_prop(expr).map(|(var, key)| (var, (key.clone(), *key_id)))
}

/// The owned spelling of the `<var>.<key>` pair (filters and plan
/// items own their strings). A cross-crate contract, not a local
/// convenience: the planner passes it as the recognizer fn to
/// `orient_side` and the executor's `unwind` builds owned plan items
/// from it, so the public owned spelling stays named rather than
/// mapped inline at each call site.
pub fn var_prop_owned(expr: &Expr) -> Option<(String, String)> {
    // Map-bridge: plan items and filters own their strings (the planner's
    // plan structures stay String-keyed until their own SmallName stage);
    // the text render happens once, here, not per lookup.
    var_prop(expr).map(|(var, key)| (var.to_string(), key.to_string()))
}

/// The four Cypher 25 chronological comparison operators, the one
/// vocabulary shared by the lowering and the executor's evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChronoOp {
    Before,
    After,
    SameInstant,
    SameRepresentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Plus,
    Minus,
    IsNull,
    IsNotNull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    Xor,
    And,
    Eq,
    Neq,
    Lt,
    Gt,
    Lte,
    Gte,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Concat,
    In,
    StartsWith,
    EndsWith,
    Contains,
    Regex,
}

impl Projection {
    /// Column name of this projection item.
    ///
    /// Resolution order (matches Neo4j):
    /// 1. explicit `AS alias`
    /// 2. [`canonical_name`] — a bare variable (`RETURN n` → `n`), a
    ///    `v.key` property access (`RETURN n.name` → `n.name`), or a map
    ///    projection on a variable (`RETURN n {.name}` → `n`; the map is a
    ///    view of that variable, and a CALL subquery exports it under the
    ///    variable's name), the shapes Neo4j names regardless of source
    ///    formatting
    /// 3. the verbatim source text captured by the parser (`RETURN 1+2`
    ///    → `1+2`, `RETURN count{()}` → `count{()}`); covers every other
    ///    expression shape without a parser-synthesized name
    /// 4. `column_<idx>` for synthetic post-parse projections that have
    ///    no `source_text`
    pub fn output_name(&self, idx: usize) -> String {
        if let Some(a) = &self.alias {
            // Rendering (a user-visible column name), not a lookup: the
            // text arms resolve through Display (ADR-0509 §2).
            return a.to_string();
        }
        if let Some(name) = canonical_name(&self.expr) {
            return name;
        }
        if let Some(src) = &self.source_text {
            return src.clone();
        }
        format!("column_{idx}")
    }

    /// The name this item imports from the enclosing scope, when it is a
    /// legal *importing-WITH* item: a bare, unaliased variable reference.
    /// `None` for an aliased item — a shape 42I28 rejects, never an import
    /// under a silently different name. The one home for that policy: the
    /// planner's 42I28 classifier and the notification layer's import-set
    /// analysis both read it.
    pub fn imported_name(&self) -> Option<&SmallName> {
        match (&self.expr, &self.alias) {
            (Expr::Variable(name), None) => Some(name),
            _ => None,
        }
    }
}

/// The canonical names Neo4j spells identically regardless of source
/// formatting: a bare variable (`n`), `*`, a `v.key` property access, and
/// a map projection on a variable (`n {.name, .age}` → `n`). `None` for
/// any other expression shape.
fn canonical_name(expr: &Expr) -> Option<String> {
    // Rendering (a user-visible column name): the text arms resolve
    // through Display (ADR-0509 §2), once per name, not per lookup.
    match expr {
        Expr::Star => Some("*".into()),
        Expr::Variable(v) => Some(v.to_string()),
        Expr::MapProjection { expr, .. } => match expr.as_ref() {
            Expr::Variable(v) => Some(v.to_string()),
            _ => None,
        },
        other => var_prop(other).map(|(v, key)| format!("{v}.{key}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_metric_names_round_trip_through_from_name() {
        // `ALL` is the one table `from_name` parses and `name` renders, so
        // this fails when a variant is added to one side only.
        for (keyword, metric) in VectorMetric::ALL {
            assert_eq!(metric.name(), keyword);
            assert_eq!(VectorMetric::from_name(keyword), Some(metric));
        }
        let names: std::collections::BTreeSet<_> = VectorMetric::ALL
            .iter()
            .map(|(keyword, _)| *keyword)
            .collect();
        assert_eq!(names.len(), VectorMetric::ALL.len(), "names are unique");
        // Parsing is case-insensitive; an unknown identifier is not a metric.
        assert_eq!(
            VectorMetric::from_name("euclidean_squared"),
            Some(VectorMetric::EuclideanSquared)
        );
        assert_eq!(VectorMetric::from_name("not-a-metric"), None);
    }

    fn path(variable: Option<&str>, subpath_variable: Option<&str>) -> PathPattern {
        PathPattern {
            variable: variable.map(SmallName::from_name),
            subpath_variable: subpath_variable.map(SmallName::from_name),
            elements: vec![],
            nodes: vec![],
            rels: vec![],
            where_clause: None,
            mode: None,
            selector: None,
            is_shortest_path_function: false,
        }
    }

    #[test]
    fn path_names_lists_outer_then_subpath_variable() {
        let both = path(Some("p"), Some("q"));
        assert_eq!(
            both.path_names()
                .map(SmallName::to_string)
                .collect::<Vec<_>>(),
            ["p", "q"]
        );
        assert!(both.has_path_name());

        let subpath_only = path(None, Some("q"));
        assert_eq!(
            subpath_only
                .path_names()
                .map(SmallName::to_string)
                .collect::<Vec<_>>(),
            ["q"]
        );
        assert!(subpath_only.has_path_name());

        let outer_only = path(Some("p"), None);
        assert_eq!(
            outer_only
                .path_names()
                .map(SmallName::to_string)
                .collect::<Vec<_>>(),
            ["p"]
        );
        assert!(outer_only.has_path_name());

        let unnamed = path(None, None);
        assert!(unnamed.path_names().next().is_none());
        assert!(!unnamed.has_path_name());
    }
}
