//! The index-backed start (ADR-0486): the one derivation that decides
//! whether a MATCH's start element narrates as an index seek — the
//! operator name, the `RANGE INDEX …` details line, and the seek's own
//! count, one `Option` the subject rendering and the estimate both read
//! so the narration and the estimate gate together.
//!
//! The pick comes from the planner's order module — [`best_seek`], the
//! one equality-before-window derivation the ladder's measured level
//! itself consumes (LABS-351); this module never re-recognizes a
//! comparison conjunct or re-ranks it against the point read.

use std::collections::BTreeMap;

use relate_ir::{BinaryOp, Expr, PathPattern};
use relate_names::SmallName;
use relate_planner::{best_seek, where_conjuncts_of, SeekPick};

use super::subject::single_static_label;
use crate::stats::PlanStats;

/// The anchored-equality point read's operator name (ADR-0486): the Neo4j
/// vocabulary for a seek to one indexed value.
const NODE_INDEX_SEEK: &str = "NodeIndexSeek";

/// The v1 comparison window's operator name (ADR-0486): the Neo4j
/// vocabulary for a seek over an indexed range.
const NODE_INDEX_SEEK_BY_RANGE: &str = "NodeIndexSeekByRange";

/// One index-backed start the plan tree narrates (ADR-0486): the seek
/// operator's name, its details line, and its own count — the narration
/// and the estimate gate together on the same `Option`, so the tree
/// never prices a narrated seek at anything but the seek's count.
pub(super) struct IndexSeek {
    /// [`NODE_INDEX_SEEK`] (the anchored-equality point read) or
    /// [`NODE_INDEX_SEEK_BY_RANGE`] (the v1 comparison window).
    pub(super) operator: &'static str,
    /// The `RANGE INDEX …` details line that leads the subject.
    pub(super) line: String,
    /// The seek's own count: the hit count for the point read, the
    /// posting-length window for the comparison — the tree's
    /// `estimated_rows` (a pre-execution, statistics-derived hint,
    /// ADR-0344 phase 3 semantics).
    pub(super) rows: u64,
}

/// The index-backed start of a MATCH (ADR-0486): a single-pattern MATCH
/// whose start element — element 0 of the post-rewrite pattern, the
/// element the rewrite chose — passes `single_static_label` with a
/// predicate a RANGE index serves. The pick's op names the operator:
/// the anchored-equality point read (the folded property map of
/// ADR-0024's fold, the degenerate tightest window on the same index)
/// carries `Eq` and names `NodeIndexSeek`; a servable comparison
/// conjunct's oriented op names `NodeIndexSeekByRange`; when an indexed
/// equality and an indexed comparison both sit on the start element,
/// the equality wins the pick — it outranks in the ladder, an ordering
/// [`best_seek`] owns and the pick's own `op` carries. Everything else
/// — multi-pattern clauses, no start element, dynamic label
/// expressions, unindexed pairs, declined counts — answers `None` and
/// keeps today's fused narration (ADR-0486's fallbacks: the plan never
/// claims a seek the funnel would decline).
pub(super) fn index_seek(
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    stats: &dyn PlanStats,
) -> Option<IndexSeek> {
    let [single] = patterns else {
        return None;
    };
    let node = single.nodes.first()?;
    single_static_label(node)?;
    // The ladder's own pick (ADR-0486): the winning identity the plan
    // decorates — never a second spelling of the min policy or the
    // equality-first ranking.
    let pick = best_seek(node, where_conjuncts_of(where_clause), stats)?;
    // The plan narration renders text: the variable and the pick's names
    // render once, here.
    seek_narration(
        node.variable.as_ref().map(|v| v.to_string()).as_deref(),
        pick,
    )
}

/// The pick's narration (ADR-0486): the operator the pick's op names,
/// the `RANGE INDEX …` details line with the bound as spelled, and
/// the pick's own count — [`index_seek`]'s half past the MATCH-shape
/// gates, everything it reads on the pick itself. Declines — `None`,
/// the fused narration — when the bound cannot spell or the op sits
/// outside the seek domain, both the [`bound_text`] decline's
/// pattern: the plan never narrates a seek it cannot name, and never
/// panics on the render.
fn seek_narration(var: Option<&str>, pick: SeekPick<'_>) -> Option<IndexSeek> {
    let SeekPick {
        label,
        prop,
        op,
        bound,
        rows,
    } = pick;
    // A bound the pick counted but the line cannot spell declines the
    // narration: the live stats' counts resolve through the same
    // `seek_value` forms, so a counted bound always spells.
    let bound = bound_text(bound)?;
    // The op names both facts at once (ADR-0486): the operator — `Eq`,
    // the folded point read, seeks one indexed value; a comparison op
    // seeks the range — and the symbol the seek line spells. One match
    // owns the pair, so no second op vocabulary exists to drift out of
    // lockstep, and an op outside the seek domain declines the
    // narration — the `bound_text` decline's pattern: the recognizer
    // (`range_prop`) gates the four comparison ops and the point read's
    // fold passes `Eq`, so no pick carries another op today; should a
    // future level (the deferred `IN`, ADR-0486) ever hand one over,
    // the narration declines to the fused form instead of panicking
    // the plan render.
    let (operator, symbol) = match op {
        BinaryOp::Eq => (NODE_INDEX_SEEK, "="),
        BinaryOp::Lt => (NODE_INDEX_SEEK_BY_RANGE, "<"),
        BinaryOp::Lte => (NODE_INDEX_SEEK_BY_RANGE, "<="),
        BinaryOp::Gt => (NODE_INDEX_SEEK_BY_RANGE, ">"),
        BinaryOp::Gte => (NODE_INDEX_SEEK_BY_RANGE, ">="),
        _ => return None,
    };
    Some(IndexSeek {
        operator,
        line: seek_line(var, label, &prop, symbol, &bound),
        rows,
    })
}

/// The seek line (ADR-0486): `RANGE INDEX <var>:<Label>(<prop>) WHERE
/// <prop> <op> <bound>` — the serving index and the predicate, the bound
/// as spelled (`$param` or the literal's text; no `, cache[…]` suffix:
/// this engine has no property cache, ADR-0344's emit-nothing rule). An
/// anonymous start element names no variable: `RANGE INDEX
/// :Label(<prop>) …`.
fn seek_line(
    var: Option<&str>,
    label: &SmallName,
    prop: &SmallName,
    op: &str,
    bound: &str,
) -> String {
    let target = match var {
        Some(var) => format!("{var}:{label}"),
        None => format!(":{label}"),
    };
    format!("RANGE INDEX {target}({prop}) WHERE {prop} {op} {bound}")
}

/// The bound as the seek line spells it (ADR-0486): `$name` for a
/// parameter and the literal's Cypher text otherwise, through the one
/// canonical literal rendering (`Value::to_cypher_literal`). `None` when
/// the bound is neither: the plan never narrates a seek whose bound it
/// cannot spell — the count resolves through the same `seek_value` forms,
/// so the live stats' `Some` counts always spell. ADR-0503: plan trees
/// never render cached plans (preamble-flagged statements never hit and
/// `explain_plan` bypasses the cache), so a tree's `$name` is always the
/// statement's own spelling — never an `$__AUTO_*__` placeholder.
fn bound_text(bound: &Expr) -> Option<String> {
    match bound {
        Expr::Parameter(name) => Some(format!("${name}")),
        other => other
            .seek_value(&BTreeMap::new())
            .map(|value| value.to_cypher_literal()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{compact, comparison, node, predicated, seeking};
    use super::*;

    /// The anchored-equality point read (ADR-0486): the folded map's
    /// indexed pair narrates `NodeIndexSeek`, the line naming the index
    /// and the predicate with the bound as spelled, the count the
    /// equality hit count.
    #[test]
    fn the_indexed_point_read_narrates_node_index_seek() {
        let stats = seeking(
            &[("Person", 2)],
            &[("Person", "name", 1)],
            &[],
            BTreeMap::new(),
        );
        let person = predicated(Some("n"), "Person", "name", Expr::String("Ada".into()));
        let patterns = [compact(vec![person], Vec::new())];
        let seek = index_seek(&patterns, None, &stats).expect("the indexed point read seeks");
        assert_eq!(seek.operator, "NodeIndexSeek");
        assert_eq!(seek.line, "RANGE INDEX n:Person(name) WHERE name = 'Ada'");
        assert_eq!(seek.rows, 1);
        // An anonymous point read names no variable in the line.
        let anon = predicated(None, "Person", "name", Expr::Int(7));
        let anon_patterns = [compact(vec![anon], Vec::new())];
        let seek = index_seek(&anon_patterns, None, &stats).expect("the pair is indexed");
        assert_eq!(
            seek.line, "RANGE INDEX :Person(name) WHERE name = 7",
            "an anonymous start element names no variable"
        );
    }

    /// The v1 comparison window (ADR-0486): a servable comparison
    /// conjunct narrates `NodeIndexSeekByRange` with the mirrored op and
    /// the bound as spelled — `$param` for a parameter, the literal's
    /// text otherwise — and the minimum count across servable conjuncts.
    #[test]
    fn the_range_window_narrates_node_index_seek_by_range() {
        let stats = seeking(
            &[("User", 10)],
            &[],
            &[("User", "age", 5), ("User", "score", 8)],
            BTreeMap::from([("p".to_string(), crate::Value::Int(3))]),
        );
        let patterns = [compact(vec![node(Some("n"), &["User"])], Vec::new())];
        // `n.age > 5`: the property-side op, the literal's text.
        let where_age = comparison("n", "age", BinaryOp::Gt, Expr::Int(5));
        let seek = index_seek(&patterns, Some(&where_age), &stats)
            .expect("the range-indexed comparison seeks");
        assert_eq!(seek.operator, "NodeIndexSeekByRange");
        assert_eq!(seek.line, "RANGE INDEX n:User(age) WHERE age > 5");
        assert_eq!(seek.rows, 5);
        // `5 > n.age` mirrors: the property-side op the seek serves.
        let flipped = Expr::Binary {
            op: BinaryOp::Gt,
            left: Box::new(Expr::Int(5)),
            right: Box::new(Expr::Property {
                key_id: Default::default(),
                expr: Box::new(Expr::Variable(SmallName::from_name("n"))),
                key: SmallName::from_name("age"),
            }),
        };
        let seek = index_seek(&patterns, Some(&flipped), &stats)
            .expect("the operand-oriented recognizer mirrors the op");
        assert_eq!(
            seek.line, "RANGE INDEX n:User(age) WHERE age < 5",
            "a right-side property spells the mirrored op"
        );
        // A parameter bound spells `$name`, never the resolved value.
        let parameterized = comparison("n", "age", BinaryOp::Gte, Expr::Parameter("p".into()));
        let seek = index_seek(&patterns, Some(&parameterized), &stats)
            .expect("a parameterized bound prices through the stats");
        assert_eq!(seek.line, "RANGE INDEX n:User(age) WHERE age >= $p");
        // Two servable conjuncts: the minimum count names the operator.
        let both = Expr::Binary {
            op: BinaryOp::And,
            left: Box::new(comparison("n", "age", BinaryOp::Gt, Expr::Int(5))),
            right: Box::new(comparison("n", "score", BinaryOp::Lt, Expr::Int(90))),
        };
        let seek = index_seek(&patterns, Some(&both), &stats).expect("both conjuncts serve");
        assert_eq!(seek.rows, 5, "the tighter window names the operator");
        assert_eq!(seek.line, "RANGE INDEX n:User(age) WHERE age > 5");
    }

    /// The point read outranks the window when both sit on the start
    /// element (ADR-0486): a point lookup is the degenerate tightest
    /// window on the same index.
    #[test]
    fn an_indexed_equality_outranks_an_indexed_comparison() {
        let stats = seeking(
            &[("User", 10)],
            &[("User", "age", 1)],
            &[("User", "score", 4)],
            BTreeMap::new(),
        );
        let start = predicated(Some("n"), "User", "age", Expr::Int(5));
        let patterns = [compact(vec![start], Vec::new())];
        let where_score = comparison("n", "score", BinaryOp::Gt, Expr::Int(1));
        let seek =
            index_seek(&patterns, Some(&where_score), &stats).expect("both predicates are indexed");
        assert_eq!(seek.operator, "NodeIndexSeek");
        assert_eq!(seek.line, "RANGE INDEX n:User(age) WHERE age = 5");
    }

    /// ADR-0486's fallbacks: every unseekable shape answers `None` and
    /// keeps today's fused narration — an unindexed pair, a two-label
    /// node, a multi-pattern clause, a bound the stats cannot resolve.
    #[test]
    fn unseekable_starts_keep_the_fused_narration() {
        let stats = seeking(
            &[("User", 10), ("Person", 2)],
            &[("Person", "name", 1)],
            &[("User", "age", 5)],
            BTreeMap::new(),
        );
        // An unindexed pair: the equality declines, the comparison's
        // property is unindexed.
        let unindexed = [compact(
            vec![predicated(
                Some("n"),
                "User",
                "name",
                Expr::String("Ada".into()),
            )],
            Vec::new(),
        )];
        let where_name = comparison("n", "name", BinaryOp::Gt, Expr::String("A".into()));
        assert!(index_seek(&unindexed, Some(&where_name), &stats).is_none());
        // A two-label node: `single_static_label` declines.
        let mut two_labels = node(Some("n"), &["User", "Person"]);
        two_labels.properties = Some(Expr::Map(vec![(
            "name".to_string(),
            Expr::String("Ada".into()),
        )]));
        let two = [compact(vec![two_labels], Vec::new())];
        assert!(index_seek(&two, None, &stats).is_none());
        // A multi-pattern clause: the `[single]` gate declines.
        let multi = vec![
            compact(vec![node(Some("n"), &["User"])], Vec::new()),
            compact(vec![node(Some("m"), &["Person"])], Vec::new()),
        ];
        let where_age = comparison("n", "age", BinaryOp::Gt, Expr::Int(5));
        assert!(index_seek(&multi, Some(&where_age), &stats).is_none());
        // A bound the stats cannot resolve (a function call): the
        // window declines with it.
        let call = comparison(
            "n",
            "age",
            BinaryOp::Gt,
            Expr::Function {
                name: SmallName::from_name("toInteger"),
                distinct: false,
                args: vec![Expr::String("100".into())],
            },
        );
        let user = [compact(vec![node(Some("n"), &["User"])], Vec::new())];
        assert!(index_seek(&user, Some(&call), &stats).is_none());
    }

    /// A pick whose op sits outside the seek domain declines the
    /// narration (ADR-0486's fallbacks): the recognizer gates the
    /// four comparison ops and the point read's fold passes `Eq`, so
    /// no live pick carries another op — but a future level (the
    /// deferred `IN`) could hand one over, and the narration then
    /// answers `None` — the `bound_text` decline's pattern, never a
    /// panic on the plan render — while end to end the same op in
    /// the WHERE clause keeps the fused narration.
    #[test]
    fn a_pick_with_an_op_outside_the_seek_domain_declines() {
        let stats = seeking(&[("User", 10)], &[], &[("User", "age", 5)], BTreeMap::new());
        let where_age = comparison("n", "age", BinaryOp::Gt, Expr::Int(5));
        let user = [compact(vec![node(Some("n"), &["User"])], Vec::new())];
        // The pick the fixtures form, its op widened outside the
        // domain — the shape a deferred level would hand the
        // narration.
        let mut pick = best_seek(
            &user[0].nodes[0],
            where_conjuncts_of(Some(&where_age)),
            &stats,
        )
        .expect("the window pick forms");
        pick.op = BinaryOp::In;
        assert!(seek_narration(
            user[0].nodes[0]
                .variable
                .as_ref()
                .map(|v| v.to_string())
                .as_deref(),
            pick
        )
        .is_none());
        // End to end, an op outside the domain in the WHERE clause
        // keeps the fused narration.
        let neq = comparison("n", "age", BinaryOp::Neq, Expr::Int(5));
        assert!(index_seek(&user, Some(&neq), &stats).is_none());
    }
}
