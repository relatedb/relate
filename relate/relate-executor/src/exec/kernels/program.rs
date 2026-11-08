//! The contracted kernels' predicate programs (ADR-0473): the closed
//! vocabulary a node's admission check compiles to, and the monomorphic
//! dispatch that keeps every per-candidate check a static, inlined call —
//! never a per-row `dyn Fn`.
//!
//! One program is a pattern's static label conjunction plus **at most one**
//! property predicate (`<var>.<key> <op> <row-invariant bound>` or
//! `<var>.<key> IS [NOT] NULL`). A second predicate on the same variable, or
//! any conjunct shape outside the vocabulary (functions over the row, `OR`,
//! `NOT`, property maps, comparisons of two properties), declines the whole
//! clause — the per-row walker keeps it, answers identical (the differential
//! tests pin that). The one-predicate bound is what keeps the monomorphic
//! table closed: three program kinds, three instantiations of every kernel
//! loop, no filter-axis template grid (ADR-0473's rejection).
//!
//! The bound is evaluated **lazily, once per stage**: a stage with no
//! candidates never evaluates it, which is the per-row path's behavior (an
//! empty candidate set never reaches `residual_ok`). Error parity otherwise
//! follows the scan-filter chain's stated caveat (`exec/batch.rs`): rows
//! kept and dropped are identical, but a pathological `WHERE` whose
//! conjuncts error on disjoint candidate subsets can surface a different
//! conjunct's message than the walk order would.

use relate_core::{NodeId, Result, Value};
use relate_ir::{BinaryOp, Expr, NodePattern, UnaryOp};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::eval::{binary_pure, eval, expr_vars, is_nondeterministic, orient, var_prop};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::agg_fold::is_pred_op;

/// One node's compiled property predicate: the bound stays an expression
/// until the stage prepares it (the lazy, once-per-stage evaluation).
#[derive(Clone, Debug)]
pub(crate) enum Prog {
    /// Labels only — no property predicate.
    Bare,
    /// `<var>.<key> <op> <bound>` with a row-invariant, deterministic
    /// bound. The comparison is the shared [`binary`] — the one canonical
    /// evaluator, three-valued logic included (a null operand makes the
    /// comparison null, and a `WHERE` keeps only `true`).
    Cmp {
        key: String,
        op: BinaryOp,
        bound: Expr,
    },
    /// `<var>.<key> IS [NOT] NULL` — answered from the property read
    /// alone; nothing can error.
    Null { key: String, is_null: bool },
}

/// One pattern node plus its compiled program: what a kernel stage admits
/// candidates against.
pub(crate) struct NodeTarget<'a> {
    pub(crate) pattern: &'a NodePattern,
    pub(crate) prog: Prog,
}

impl NodeTarget<'_> {
    /// Whether the node admits every node: no label, dynamic label
    /// expression, property map, inline element `WHERE`, or compiled
    /// predicate.
    pub(crate) fn unfiltered(&self) -> bool {
        self.pattern.labels.is_empty()
            && self.pattern.label_expr.is_none()
            && self.pattern.properties.is_none()
            && self.pattern.where_clause.is_none()
            && matches!(self.prog, Prog::Bare)
    }
}

/// The interned form of a pattern's static label conjunction, resolved
/// once per stage ([`NodePred::prepare`]): one interner lookup per label,
/// never per candidate (ADR-0473 — the per-candidate string interning was
/// the measured gap between the first kernel cut and the speed-of-light
/// kernels). A label nothing in the committed store ever carried has no
/// id — no committed node can carry it either, so its check fails without
/// a store read; a write scratch's overlay is the exception (write-path
/// labels stay strings there, interned only at commit), and its nodes
/// answer through the string arm instead.
pub(crate) struct InternedLabels<'a> {
    labels: &'a [SmallName],
    ids: Vec<Option<u32>>,
}

impl InternedLabels<'_> {
    fn resolve<'a>(ctx: ExecCtx<'_>, labels: &'a [SmallName]) -> InternedLabels<'a> {
        InternedLabels {
            labels,
            ids: labels
                .iter()
                .map(|label| label.with_str(|text| ctx.graph.base().interned_label_id(text)))
                .collect(),
        }
    }

    /// The static label conjunction and visibility check — `node_ok`'s two
    /// label arms for an unbound variable: every pattern label present
    /// (`None` for an invisible node fails exactly like an absent label),
    /// and a pattern with no labels at all checking bare visibility.
    fn admits(&self, graph: &ExecGraph<'_>, node: NodeId) -> bool {
        if self.labels.is_empty() {
            return graph.node_exists(node);
        }
        self.labels
            .iter()
            .zip(&self.ids)
            .all(|(label, id)| match id {
                Some(id) => matches!(
                    label.with_str(|text| graph.node_has_label_id(node, *id, text)),
                    Some(true)
                ),
                // A label the base interner never recorded can still ride
                // a write scratch: write-path labels stay strings in the
                // overlay, interned only at commit, so the string arm of
                // `node_has_label` serves them. Without a scratch nothing
                // can carry an uninterned label, so the check fails
                // without a store read.
                None => {
                    graph.has_write_overlay()
                        && matches!(
                            label.with_str(|text| graph.node_has_label(node, text)),
                            Some(true)
                        )
                }
            })
    }
}

/// The per-candidate admission check, monomorphized (ADR-0473). A kernel
/// loop is generic over `P`; the stage dispatches once (one `match` over
/// the compiled [`Prog`]) and every candidate's check inside the loop is
/// this trait's method — statically dispatched and inlined, never a
/// per-row `dyn Fn`.
///
/// The check order is the walker's own, and that order is what keeps an
/// erroring bound's *presence* identical to the per-row path's: the label
/// conjunction first (`node_ok`'s arms — a candidate the labels reject
/// never reaches the predicate, exactly as `match_node` runs `node_ok`
/// before `residual_ok`), then the property predicate, with the
/// row-invariant bound evaluated lazily — once, at the first candidate
/// that reaches the comparison. A candidate set that never reaches the
/// comparison never evaluates the bound, the per-row path's behavior for
/// a `WHERE` whose rows all stop earlier.
pub(crate) trait NodePred: Copy {
    /// The per-run prepared form: the bound's value, evaluated at most
    /// once per stage.
    type Prepared;

    /// Evaluate the row-invariant bound. Called only from
    /// [`NodePred::admits`]'s lazy path (and once per run).
    fn prepare(&self, ctx: ExecCtx<'_>) -> Result<Self::Prepared>;

    /// Whether `node` admits.
    fn admits(
        &self,
        prepared: &mut Option<Self::Prepared>,
        ctx: ExecCtx<'_>,
        node: NodeId,
    ) -> Result<bool>;
}

/// Labels only.
#[derive(Clone, Copy)]
pub(crate) struct LabelsOnly<'a> {
    labels: &'a [SmallName],
}

/// Labels plus one property comparison.
#[derive(Clone, Copy)]
pub(crate) struct PropCmp<'a> {
    labels: &'a [SmallName],
    key: &'a str,
    op: BinaryOp,
    bound: &'a Expr,
}

/// Labels plus one null check.
#[derive(Clone, Copy)]
pub(crate) struct PropNull<'a> {
    labels: &'a [SmallName],
    key: &'a str,
    is_null: bool,
}

impl<'a> NodePred for LabelsOnly<'a> {
    type Prepared = InternedLabels<'a>;

    fn prepare(&self, ctx: ExecCtx<'_>) -> Result<Self::Prepared> {
        Ok(InternedLabels::resolve(ctx, self.labels))
    }

    fn admits(
        &self,
        prepared: &mut Option<Self::Prepared>,
        ctx: ExecCtx<'_>,
        node: NodeId,
    ) -> Result<bool> {
        if prepared.is_none() {
            *prepared = Some(self.prepare(ctx)?);
        }
        let prepared = prepared.as_ref().expect("prepared before the label check");
        Ok(prepared.admits(ctx.graph, node))
    }
}

/// The prepared comparison: the bound's value, evaluated once per run,
/// plus the interned labels (checked before the comparison) — and the
/// check itself, over the prepared facts alone.
pub(crate) struct CmpPrepared<'a> {
    labels: InternedLabels<'a>,
    key: &'a str,
    op: BinaryOp,
    value: Value,
}

impl CmpPrepared<'_> {
    /// The comparison check: the label conjunction first, then the
    /// canonical property read against the once-evaluated bound.
    fn admits(&self, ctx: ExecCtx<'_>, node: NodeId) -> Result<bool> {
        if !self.labels.admits(ctx.graph, node) {
            return Ok(false);
        }
        // A null bound makes every comparison null, and a `WHERE` drops
        // null — the shortcut is `binary`'s own null arm, without paying
        // the property read.
        if self.value.is_null() {
            return Ok(false);
        }
        // The canonical property read: a missing property is null, and a
        // null operand is `binary`'s null arm again.
        let have = ctx.graph.node_prop(node, self.key).unwrap_or(Value::Null);
        if have.is_null() {
            return Ok(false);
        }
        Ok(binary_pure(self.op, have, self.value.clone())?.truthy() == Some(true))
    }
}

impl<'a> NodePred for PropCmp<'a> {
    type Prepared = CmpPrepared<'a>;

    fn prepare(&self, ctx: ExecCtx<'_>) -> Result<Self::Prepared> {
        // The bound is variable-free by the decoder, so the empty row it
        // cannot read is inert — the same evaluation the per-row path
        // makes against each matched row, minus the row it never reads.
        let value = eval(
            ctx.graph,
            &Row::new(),
            self.bound,
            ctx.params,
            ctx.cypher_version,
        )?;
        Ok(CmpPrepared {
            labels: InternedLabels::resolve(ctx, self.labels),
            key: self.key,
            op: self.op,
            value,
        })
    }

    fn admits(
        &self,
        prepared: &mut Option<Self::Prepared>,
        ctx: ExecCtx<'_>,
        node: NodeId,
    ) -> Result<bool> {
        if prepared.is_none() {
            *prepared = Some(self.prepare(ctx)?);
        }
        let prepared = prepared.as_ref().expect("prepared before the comparison");
        prepared.admits(ctx, node)
    }
}

impl<'a> NodePred for PropNull<'a> {
    type Prepared = PreparedNull<'a>;

    fn prepare(&self, ctx: ExecCtx<'_>) -> Result<Self::Prepared> {
        Ok(PreparedNull {
            labels: InternedLabels::resolve(ctx, self.labels),
            key: self.key,
            is_null: self.is_null,
        })
    }

    fn admits(
        &self,
        prepared: &mut Option<Self::Prepared>,
        ctx: ExecCtx<'_>,
        node: NodeId,
    ) -> Result<bool> {
        if prepared.is_none() {
            *prepared = Some(self.prepare(ctx)?);
        }
        let prepared = prepared.as_ref().expect("prepared before the null check");
        prepared.admits(ctx, node)
    }
}

/// The prepared null check: the interned labels plus the key and the
/// written polarity — the check itself reads only the property.
pub(crate) struct PreparedNull<'a> {
    labels: InternedLabels<'a>,
    key: &'a str,
    is_null: bool,
}

impl PreparedNull<'_> {
    /// The null check: the label conjunction first, then the property
    /// read's presence compared to the written polarity.
    fn admits(&self, ctx: ExecCtx<'_>, node: NodeId) -> Result<bool> {
        if !self.labels.admits(ctx.graph, node) {
            return Ok(false);
        }
        let is_null = ctx
            .graph
            .node_prop(node, self.key)
            .is_none_or(|value: Value| value.is_null());
        Ok(is_null == self.is_null)
    }
}

/// The monomorphic admission of one [`NodeTarget`] — the one runtime
/// dispatch per stage: the caller passes it to its kernel loop, after
/// which every per-candidate check inside the loop is
/// [`NodePred::admits`], statically dispatched and inlined. The three
/// arms are the whole vocabulary; a program outside them is
/// unconstructible (the decoder declines before a stage exists).
#[derive(Clone, Copy)]
pub(crate) enum Admission<'a> {
    Bare(LabelsOnly<'a>),
    Cmp(PropCmp<'a>),
    Null(PropNull<'a>),
}

/// The [`Admission`] wrapper's prepared form — the small enum that folds
/// the three kinds' prepared forms into one, so a kernel loop holds one
/// `Option<AdmissionPrepared>` beside one `Admission` and never
/// re-dispatches per candidate.
pub(crate) enum AdmissionPrepared<'a> {
    Bare(InternedLabels<'a>),
    Cmp(CmpPrepared<'a>),
    Null(PreparedNull<'a>),
}

impl AdmissionPrepared<'_> {
    /// The prepared form's check — the variant's own, over its prepared
    /// facts.
    fn admits(&self, ctx: ExecCtx<'_>, node: NodeId) -> Result<bool> {
        match self {
            AdmissionPrepared::Bare(labels) => Ok(labels.admits(ctx.graph, node)),
            AdmissionPrepared::Cmp(prepared) => prepared.admits(ctx, node),
            AdmissionPrepared::Null(prepared) => prepared.admits(ctx, node),
        }
    }
}

impl<'a> NodePred for Admission<'a> {
    type Prepared = AdmissionPrepared<'a>;

    fn prepare(&self, ctx: ExecCtx<'_>) -> Result<Self::Prepared> {
        Ok(match self {
            Admission::Bare(pred) => AdmissionPrepared::Bare(pred.prepare(ctx)?),
            Admission::Cmp(pred) => AdmissionPrepared::Cmp(pred.prepare(ctx)?),
            Admission::Null(pred) => AdmissionPrepared::Null(pred.prepare(ctx)?),
        })
    }

    fn admits(
        &self,
        prepared: &mut Option<Self::Prepared>,
        ctx: ExecCtx<'_>,
        node: NodeId,
    ) -> Result<bool> {
        if prepared.is_none() {
            *prepared = Some(self.prepare(ctx)?);
        }
        let prepared = prepared
            .as_ref()
            .expect("the wrapper prepares before the check");
        prepared.admits(ctx, node)
    }
}

impl<'a> Admission<'a> {
    /// The dispatched admission of `target`'s compiled program.
    pub(crate) fn of(target: &'a NodeTarget<'a>) -> Admission<'a> {
        let labels = target.pattern.labels.as_slice();
        match &target.prog {
            Prog::Bare => Admission::Bare(LabelsOnly { labels }),
            Prog::Cmp { key, op, bound } => Admission::Cmp(PropCmp {
                labels,
                key,
                op: *op,
                bound,
            }),
            Prog::Null { key, is_null } => Admission::Null(PropNull {
                labels,
                key,
                is_null: *is_null,
            }),
        }
    }
}

/// Decode one flattened `WHERE` conjunct into the kernels' vocabulary:
/// the conjunct's variable plus its compiled [`Prog`]. `None` declines —
/// the shape is outside the vocabulary, and the caller keeps the per-row
/// path for the whole clause.
///
/// The one canonical decode of a property conjunct against a
/// row-invariant bound: the mirror policy is the shared `orient`
/// (a property on the right of a symmetric comparison flips the operator;
/// a property on the right of an operand-asymmetric one — `IN`, the string
/// predicates — declines), and the bound's row-invariance, determinism,
/// and scope-freedom are proven here once. The scan-filter chain's gate
/// (`exec/batch.rs`) reads the same proof for its own bounds.
pub(crate) fn decode_conjunct(expr: &Expr) -> Option<(String, Prog)> {
    match expr {
        Expr::Unary { op, expr } => {
            let is_null = match op {
                UnaryOp::IsNull => true,
                UnaryOp::IsNotNull => false,
                _ => return None,
            };
            let (var, key) = var_prop(expr)?;
            Some((
                var.to_string(),
                Prog::Null {
                    key: key.to_string(),
                    is_null,
                },
            ))
        }
        Expr::Binary { op, left, right } if is_pred_op(*op) => {
            let ((var, key), other, op) = orient(var_prop, left, right, *op)?;
            if !bound_is_row_invariant(other) {
                return None;
            }
            Some((
                var.to_string(),
                Prog::Cmp {
                    key: key.to_string(),
                    op,
                    bound: other.clone(),
                },
            ))
        }
        _ => None,
    }
}

/// Whether the bound expression may evaluate once per clause instead of
/// per row: no variables at all (so no row can change it), no
/// nondeterministic call (`rand()` may differ per row), and no
/// scope-bearing variant (a subquery hides its internals from the walk).
pub(crate) fn bound_is_row_invariant(bound: &Expr) -> bool {
    let mut named = std::collections::HashSet::new();
    expr_vars(bound, &mut named);
    named.is_empty()
        && !bound.any(&mut |expr| {
            matches!(expr, Expr::Function { name, .. }
                if name.with_str(is_nondeterministic))
                || expr.is_scope_bearing()
        })
}

#[cfg(test)]
mod tests {
    use relate_front::parse_and_lower;
    use relate_ir::Clause;

    use super::*;

    /// The flattened `WHERE` conjuncts of a planned single-clause `MATCH`.
    fn where_conjuncts(cypher: &str) -> Vec<Expr> {
        let query = parse_and_lower(cypher).unwrap();
        let plan = relate_planner::plan(query, &mut |_| false).unwrap();
        match &plan.parts[0][0] {
            Clause::Match {
                where_clause: Some(w),
                ..
            } => crate::exec::pushdown::flatten_and_owned(w),
            other => panic!("not a MATCH with WHERE: {other:?}"),
        }
    }

    fn decoded(cypher: &str) -> Vec<Option<(String, Prog)>> {
        where_conjuncts(cypher)
            .iter()
            .map(decode_conjunct)
            .collect()
    }

    fn is_bare(prog: &Prog) -> bool {
        matches!(prog, Prog::Bare)
    }

    // The vocabulary: every comparison operator the shared pred-op table
    // admits decodes, mirrored comparisons orient, and null checks decode.
    #[test]
    fn comparison_and_null_conjuncts_decode() {
        let cases = decoded(
            "MATCH (n) WHERE n.k > 1 AND n.s CONTAINS 'x' AND n.t STARTS WITH 'a' \
             AND n.l IN [1, 2] AND n.z IS NOT NULL RETURN n",
        );
        assert_eq!(cases.len(), 5);
        for case in &cases {
            assert!(case.is_some(), "every conjunct decodes: {case:?}");
        }
        let Some((var, Prog::Cmp { op, .. })) = &cases[0] else {
            panic!("first conjunct is a comparison");
        };
        assert_eq!(var, "n");
        assert_eq!(*op, BinaryOp::Gt);
        let Some((_, Prog::Null { is_null, .. })) = &cases[4] else {
            panic!("last conjunct is a null check");
        };
        assert!(!*is_null);
    }

    #[test]
    fn mirrored_comparisons_orient_instead_of_declining() {
        let cases = decoded("MATCH (n) WHERE 1 < n.k RETURN n");
        let Some((var, Prog::Cmp { op, .. })) = &cases[0] else {
            panic!("a mirrored comparison orients");
        };
        assert_eq!(var, "n");
        assert_eq!(*op, BinaryOp::Gt, "1 < n.k is n.k > 1");
    }

    // The declines: two properties compared (bound not row-invariant), a
    // function over the row, OR, NOT, a nondeterministic bound, a
    // parameterized bound stays (row-invariant), and a property on the
    // right of an operand-asymmetric comparison.
    #[test]
    fn out_of_vocabulary_conjuncts_decline() {
        let declines = [
            "MATCH (n) WHERE n.a > n.b RETURN n",
            "MATCH (n) WHERE size(n.k) > 3 RETURN n",
            "MATCH (n) WHERE n.k > 1 OR n.j < 2 RETURN n",
            "MATCH (n) WHERE NOT n.k > 1 RETURN n",
            "MATCH (n) WHERE n.k > toInteger(rand()) RETURN n",
            "MATCH (n) WHERE [1] IN n.k RETURN n",
        ];
        for cypher in declines {
            for case in decoded(cypher) {
                assert!(case.is_none(), "{cypher} declines: {case:?}");
            }
        }
        // A parameter bound is row-invariant: it decodes.
        for case in decoded("MATCH (n) WHERE n.k > $min RETURN n") {
            assert!(case.is_some(), "a parameter bound decodes");
        }
    }

    #[test]
    fn bare_and_clone_shapes() {
        assert!(is_bare(&Prog::Bare));
        // Prog is Clone (the matcher stores it beside its pattern).
        let prog = Prog::Cmp {
            key: "k".to_string(),
            op: BinaryOp::Eq,
            bound: Expr::Int(1),
        };
        assert!(matches!(prog.clone(), Prog::Cmp { .. }));
    }
}
