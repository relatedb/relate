//! One clause's plan-node content: the typed operator fact a clause
//! renders to, the details and identifiers it carries, and the projection
//! tail (`ORDER BY`/`SKIP`/`LIMIT`) that rides it.
//!
//! Split out of `plan_node` so the clause dispatch — the module's largest
//! single match — reads beside the vocabulary it builds, and the parent
//! keeps the tree assembly.

use std::borrow::Cow;

use relate_ir::{is_agg_expr, Clause, Expr, Projection, WithOrigin};
use relate_model::wire::Notification;

use crate::plan_node::warning::PlanWarning;
use crate::stats::PlanStats;

/// One clause's plan-node content: the operator its typed fact renders to,
/// the details, and every fact the node carries beyond it. [`clause_node`]
/// returns this one channel instead of mutating out-of-band locals after the
/// builder chain.
pub(super) struct ClauseNode {
    /// The typed operator fact; [`ClauseOperator::name`] renders it.
    pub(super) operator: ClauseOperator,
    pub(super) details: String,
    pub(super) identifiers: Vec<String>,
    pub(super) estimated_rows: Option<u64>,
    pub(super) warnings: Vec<PlanWarning>,
    pub(super) notifications: Vec<Notification>,
    /// The store's active pattern-engine rung (LABS-1117), set on a
    /// `MATCH` clause and `None` on every other.
    pub(super) pattern_engine: Option<relate_executor::PatternEngine>,
}

impl ClauseNode {
    /// The one field-filling constructor: every node starts with the same
    /// defaults, and the operator fact is the only thing that varies.
    fn new(operator: impl Into<ClauseOperator>, details: String, identifiers: Vec<String>) -> Self {
        Self {
            operator: operator.into(),
            details,
            identifiers,
            estimated_rows: None,
            warnings: Vec::new(),
            notifications: Vec::new(),
            pattern_engine: None,
        }
    }
}

impl From<&'static str> for ClauseOperator {
    fn from(name: &'static str) -> Self {
        Self::Named(Cow::Borrowed(name))
    }
}

impl From<Cow<'static, str>> for ClauseOperator {
    fn from(name: Cow<'static, str>) -> Self {
        Self::Named(name)
    }
}

/// The typed operator fact a clause lowers to. The grouping fact is the
/// variant — stamped where `grouped` is known — so the eager-aggregation
/// notification reads it instead of comparing rendered operator names
/// (ADR-0363 decision 4).
pub(super) enum ClauseOperator {
    /// A grouping projection (`GROUP BY` keys or an aggregating item).
    EagerAggregation,
    /// Every other operator, under its own name.
    Named(Cow<'static, str>),
}

impl ClauseOperator {
    /// The operator name the plan table and the wire form render.
    pub(super) fn name(&self) -> &str {
        match self {
            Self::EagerAggregation => EAGER_AGGREGATION_OPERATOR,
            Self::Named(name) => name,
        }
    }
}

/// The rendered name of [`ClauseOperator::EagerAggregation`] — the one place
/// the grouping fact's name is spelled. The name is a display detail: the
/// eager notification reads the typed variant, never this string.
const EAGER_AGGREGATION_OPERATOR: &str = "EagerAggregation";

/// The node for one logical clause (ADR-0344 phase 3: clause-granularity
/// operators, Neo4j names where the mapping is natural, this engine's own
/// elsewhere — each naming its subject: patterns carry their text, a scan
/// names its labels, an aggregation names its grouping keys, an estimate
/// where ADR-0218 `PatternStats` offers one).
pub(super) fn clause_node(clause: &Clause, stats: &dyn PlanStats) -> ClauseNode {
    match clause {
        Clause::Match {
            optional,
            patterns,
            where_clause,
            search,
            ..
        } => {
            // ADR-0486: an index-servable start narrates as the seek —
            // one derivation the operator rename and the estimate both
            // read, so the narration and the estimate gate together on
            // the same `Option`.
            let seek = super::seek::index_seek(patterns, where_clause.as_ref(), stats);
            let (subject_operator, mut details) =
                super::subject::match_subject(patterns, *optional, seek.as_ref());
            if *optional {
                details += ", optional";
            }
            if search.is_some() {
                details += ", SEARCH";
            }
            if where_clause.is_some() {
                details += ", WHERE filter";
            }
            ClauseNode {
                estimated_rows: super::subject::match_estimate(
                    patterns,
                    stats,
                    seek.as_ref().and_then(|seek| seek.rows),
                ),
                warnings: super::warning::match_warnings(
                    patterns,
                    where_clause.as_ref(),
                    stats,
                    seek.as_ref(),
                ),
                notifications: super::notifications::match_notifications(patterns, stats),
                pattern_engine: stats.pattern_engine(),
                ..ClauseNode::new(subject_operator, details, Vec::new())
            }
        }
        Clause::Create { patterns } => ClauseNode::new(
            "Create",
            super::subject::pattern_texts(patterns),
            Vec::new(),
        ),
        Clause::Merge {
            pattern,
            on_create,
            on_match,
            ..
        } => {
            let mut details = vec![super::subject::pattern_text(pattern)];
            if !on_create.is_empty() {
                details.push("ON CREATE SET".to_string());
            }
            if !on_match.is_empty() {
                details.push("ON MATCH SET".to_string());
            }
            ClauseNode::new("Merge", details.join(", "), Vec::new())
        }
        Clause::Call { name, .. } => {
            ClauseNode::new("ProcedureCall", format!("{name}()"), Vec::new())
        }
        Clause::Delete {
            detach,
            expressions,
        } => ClauseNode::new(
            if *detach {
                Cow::Borrowed("DetachDelete")
            } else {
                Cow::Borrowed("Delete")
            },
            format!("{} expression(s)", expressions.len()),
            Vec::new(),
        ),
        Clause::Set { items } => ClauseNode::new(
            "SetProperty",
            format!("{} item(s)", items.len()),
            Vec::new(),
        ),
        Clause::Remove { items } => ClauseNode::new(
            "RemoveProperty",
            format!("{} item(s)", items.len()),
            Vec::new(),
        ),
        Clause::With {
            distinct,
            group_by,
            items,
            where_clause,
            origin,
            order,
            skip,
            limit,
        } => {
            // A page statement's horizon renders as the projection it is:
            // its own origin separates it for the rules, not for the plan
            // text.
            let operator = match origin {
                WithOrigin::Explicit | WithOrigin::Page => "Projection",
                WithOrigin::Let => "Let",
                WithOrigin::Filter => "Filter",
            };
            projection_node(
                operator,
                *distinct,
                items,
                group_by.as_deref(),
                &ProjectionTail {
                    where_filter: where_clause.is_some(),
                    ordered: !order.is_empty(),
                    skipped: skip.is_some(),
                    limited: limit.is_some(),
                },
            )
        }
        Clause::Unwind { alias, .. } => {
            ClauseNode::new("Unwind", format!("AS {alias}"), vec![alias.to_string()])
        }
        Clause::Return {
            distinct,
            group_by,
            items,
            order,
            skip,
            limit,
        } => projection_node(
            "ProduceResults",
            *distinct,
            items,
            group_by.as_deref(),
            &ProjectionTail {
                where_filter: false,
                ordered: !order.is_empty(),
                skipped: skip.is_some(),
                limited: limit.is_some(),
            },
        ),
        Clause::CreateIndex {
            name,
            label,
            property,
            ..
        } => ClauseNode::new(
            "CreateIndex",
            match name {
                Some(name) => format!("{name} FOR :{label}({property})"),
                None => format!("FOR :{label}({property})"),
            },
            Vec::new(),
        ),
        Clause::DropIndex { .. } => ClauseNode::new("DropIndex", String::new(), Vec::new()),
        Clause::ShowIndexes => ClauseNode::new("ShowIndexes", String::new(), Vec::new()),
        Clause::Finish => ClauseNode::new("ProduceResults", "FINISH".to_string(), Vec::new()),
        Clause::LoadCsv {
            with_headers,
            variable,
            ..
        } => ClauseNode::new(
            "LoadCSV",
            if *with_headers {
                format!("WITH HEADERS AS {variable}")
            } else {
                format!("AS {variable}")
            },
            vec![variable.to_string()],
        ),
        Clause::CallSubquery {
            optional,
            in_transactions,
            ..
        } => {
            let mut details = Vec::new();
            if *optional {
                details.push("optional");
            }
            if in_transactions.is_some() {
                details.push("IN TRANSACTIONS");
            }
            ClauseNode::new("CallSubquery", details.join(", "), Vec::new())
        }
        Clause::Conditional {
            branches,
            else_query,
        } => {
            let mut details = format!("{} branch(es)", branches.len());
            if else_query.is_some() {
                details += ", ELSE";
            }
            ClauseNode::new("Conditional", details, Vec::new())
        }
        Clause::Foreach { variable, .. } => {
            ClauseNode::new("Foreach", variable.to_string(), Vec::new())
        }
        Clause::Use { .. } => ClauseNode::new("Use", String::new(), Vec::new()),
        // Catalog commands reuse the planner's canonical clause keyword
        // (the same name the old rendering showed), never a Debug dump.
        Clause::Catalog(command) => ClauseNode::new(
            "Catalog",
            relate_planner::clause_keyword(command).to_string(),
            Vec::new(),
        ),
    }
}

/// The paging-and-filter tail a `WITH`/`RETURN` clause can carry — one
/// named bundle so the node builder stays under the argument budget.
struct ProjectionTail {
    where_filter: bool,
    ordered: bool,
    skipped: bool,
    limited: bool,
}

/// The grouping keys an aggregating projection names in its details, or
/// `None` when the clause does not group — one derivation from the clause's
/// own inputs, so the flag and the keys cannot disagree about whether it
/// groups. An explicit `GROUP BY` list and the non-aggregating items an
/// inferred grouping is read from both name the same columns, which is what
/// the details render.
fn grouping(items: &[Projection], group_by: Option<&[Expr]>) -> Option<Vec<String>> {
    if group_by.is_none() && !items.iter().any(|it| is_agg_expr(&it.expr)) {
        return None;
    }
    Some(
        items
            .iter()
            .enumerate()
            .filter(|(_, it)| !is_agg_expr(&it.expr))
            .map(|(idx, it)| it.output_name(idx))
            .collect(),
    )
}

/// A projection-shaped node: `Projection`/`ProduceResults` (or
/// `EagerAggregation` when the clause groups, naming its grouping keys),
/// with the paging tail and filters named in the details. The grouping
/// fact, the item count and the keys are derived here from the clause's own
/// `items`/`group_by`, so both projection arms pass only what differs.
fn projection_node(
    operator: &'static str,
    distinct: bool,
    items: &[Projection],
    group_by: Option<&[Expr]>,
    tail: &ProjectionTail,
) -> ClauseNode {
    // One decision, two outputs: the grouping fact stamps the operator and
    // names the keys in the details.
    let (operator, grouping_details) = match grouping(items, group_by) {
        Some(keys) if !keys.is_empty() => (
            ClauseOperator::EagerAggregation,
            format!(", keys: {}, aggregation", keys.join(", ")),
        ),
        Some(_) => (
            ClauseOperator::EagerAggregation,
            ", aggregation".to_string(),
        ),
        None => (operator.into(), String::new()),
    };
    let mut d = format!("{} item(s){grouping_details}", items.len());
    if distinct {
        d += ", distinct";
    }
    if tail.where_filter {
        d += ", WHERE filter";
    }
    if tail.ordered {
        d += ", ordered";
    }
    if tail.skipped {
        d += ", skip";
    }
    if tail.limited {
        d += ", limit";
    }
    ClauseNode::new(operator, d, Vec::new())
}
