//! Scan predicate specialization (the q8 61x family): residual conjuncts
//! over one pattern variable compile once per walk into inline predicates
//! evaluated against the candidate's live record under ONE store read —
//! interned one-slot property reads, no row materialization, no
//! tree-walking expression evaluation for the candidates the predicate
//! rejects. The generic path (`node_ok` + bind + `residual_ok`) still
//! runs for every candidate that passes — the binding work, and every
//! conjunct the gate did not already settle under the same read — so a
//! specialized walk's rows are the generic walk's rows by construction;
//! the specialization only removes work, never changes verdicts. A
//! compiled conjunct is *settled*: the gate evaluated it against the
//! candidate's own record, the compile rules guarantee the residual's
//! re-evaluation would see identical inputs, and the walk's residual
//! selection (`ready_conjuncts`' `settled` parameter, with
//! `residual_ok_skipping` the single-candidate spelling) skips the
//! redundant re-run — measured on GBL SF0.1 q8 at ~10% of the walk (the
//! re-parse of every surviving candidate's date at both binds).
//!
//! The compiled forms are deliberately narrow — the shapes the q8 family
//! actually carries — and everything else declines to the generic path:
//!
//! - `v.key <op> <seek constant>` — property on the left, a seek
//!   constant (literal, parameter, or the front-end's folded temporal
//!   value) on the right; `<op>` is the one canonical operator table
//!   (`is_pred_op`) the folds and the batch filter also admit — never a
//!   second list here — so `IN` (a parameter-resolved list bound) and
//!   `=~` (a string pattern bound) gate too. A mirrored comparison
//!   declines, conservative like the batch chain's operand-asymmetric
//!   declines.
//! - `<temporal constructor>(v.key) <op> <seek constant>` — the property
//!   constructs per candidate through the one shared construction policy
//!   (`temporal::construct_named`), so `date(p.birthday)` keeps its typed
//!   per-row parse while `date("1990-01-01")` is the precomputed bound.
//!
//! Three compile-time rules keep the specialization row-identical to the
//! generic evaluation:
//!
//! - **All-or-nothing per variable.** Every conjunct over exactly the
//!   node's variable compiles, or none does. A partial compile would
//!   evaluate a later compiled predicate before an earlier uncompiled
//!   one, and an expression that errors (`p.b * 2 > 1` on a string)
//!   would raise where the generic order would have short-circuited.
//! - **Constants only on the bound side** (`Expr::seek_value`): no
//!   variables, no calls — so nothing sampled per row can differ between
//!   the gate's evaluation and the residual's re-evaluation, and the
//!   constructor side is one of the six base constructors over one
//!   property argument, a pure parse.
//! - **Order safety across variables.** A conjunct the gate does not
//!   evaluate may precede a compiled one only when an earlier node bind
//!   already settled it (it names node variables of this path only,
//!   never this variable). Otherwise the gate's candidate-time
//!   predicates would run before a conjunct the generic order runs first
//!   at the node's bind — a cross-variable comparison, a rel-variable
//!   conjunct (rel binds run no residual pass), or a constant — and a
//!   short-circuit or a raise on it would diverge. Such a conjunct ahead
//!   of a compiled one returns the variable to the generic path.

use std::collections::{BTreeMap, HashMap, HashSet};

use relate_core::{temporal, NodeId, Result, Value};
use relate_ir::{BinaryOp, Expr, NodePattern, PathPattern};
use relate_names::SmallName;
use relate_planner::Bound;

use crate::eval::{binary_pure, expr_vars};
use crate::overlay::{ExecGraph, ScanNode};
use crate::row::Row;

use super::agg_fold::is_pred_op;
use super::pushdown::Residual;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    /// A/B test hook (the `with_pivot_disabled` precedent): force the
    /// generic path so a differential test can compare the specialized
    /// walk's rows against it.
    static SPEC_DISABLED: Cell<bool> = const { Cell::new(false) };
    /// Engagement counter: how many candidates a gated store read has
    /// settled — a test asserts the specialization actually fires
    /// (a silently-declining spec would pass any differential trivially).
    static SPEC_GATES: Cell<u64> = const { Cell::new(0) };
}

/// Test hook: the gates taken since the last [`take_spec_gates`].
#[cfg(test)]
pub(super) fn take_spec_gates() -> u64 {
    SPEC_GATES.with(|count| count.replace(0))
}

/// `with_spec_disabled` for differential tests: the closure's walks take
/// no compiled predicates, the generic path only.
#[cfg(test)]
pub(super) fn with_spec_disabled<R>(f: impl FnOnce() -> R) -> R {
    super::test_support::with_flag_disabled(&SPEC_DISABLED, f)
}

#[cfg(test)]
fn spec_disabled() -> bool {
    SPEC_DISABLED.with(Cell::get)
}

/// One compiled conjunct over the node's variable.
enum InlinePred {
    /// `v.key <op> <seek constant>`: the property read against the
    /// precomputed constant.
    Prop {
        key: String,
        kid: Option<u32>,
        op: BinaryOp,
        bound: Value,
    },
    /// `<temporal constructor>(v.key) <op> <seek constant>`: the property
    /// constructs through the shared policy, then compares.
    Temporal {
        ctor: String,
        key: String,
        kid: Option<u32>,
        op: BinaryOp,
        bound: Value,
    },
}

impl InlinePred {
    /// Evaluate against one candidate's record. The comparison is the
    /// shared `binary` over the same operands the generic evaluation
    /// produces, so truthiness, null handling, and errors are identical.
    fn ok(&self, node: &ScanNode<'_>) -> Result<bool> {
        let value = match self {
            Self::Prop {
                key,
                kid,
                op,
                bound,
            } => binary_pure(*op, node.prop(key, *kid), bound.clone())?,
            Self::Temporal {
                ctor,
                key,
                kid,
                op,
                bound,
            } => {
                // The property read stays borrowed ([`ScanNode::prop_value`]):
                // the construction borrows its argument and the padded-ISO
                // fast path parses a `&str`, so the hot per-candidate read
                // never clones the stored string. `Value::Null` is the
                // absent property — the same value `prop` would clone.
                let null = Value::Null;
                let arg = node.prop_value(key, *kid).unwrap_or(&null);
                // The general construction, the one shape every
                // non-fast argument takes.
                let construct = || temporal::construct_named(ctor, std::slice::from_ref(arg));
                // The stored padded-ISO form constructs through the
                // fast value (`padded_iso_date_value` — the same value
                // the general policy builds, pinned by its differential
                // test); every other shape parses through the policy.
                let left = match (arg, ctor.as_str()) {
                    (Value::String(s), "date") => match temporal::padded_iso_date_value(s) {
                        Some(value) => value,
                        None => construct()?,
                    },
                    _ => construct()?,
                };
                binary_pure(*op, left, bound.clone())?
            }
        };
        Ok(matches!(value, Value::Bool(true)))
    }

    /// Compile one conjunct for variable `var`, or decline (`None`).
    fn compile(
        graph: &ExecGraph<'_>,
        var: &SmallName,
        conjunct: &Expr,
        params: &BTreeMap<String, Value>,
    ) -> Option<Self> {
        let Expr::Binary { op, left, right } = conjunct else {
            return None;
        };
        // The one canonical operator table (`is_pred_op`), the same
        // vocabulary the folds and the batch filter admit — never a
        // second list here. Every operator it admits evaluates through
        // the shared `binary`, the same comparison the tree-walking
        // eval makes, so `IN` (membership) and `=~` (whole-string
        // regex) gate as soundly as the comparisons do.
        if !is_pred_op(*op) {
            return None;
        }
        // The bound: a seek constant — literal, parameter, or the
        // front-end's folded temporal value. Anything else (a variable, a
        // call) declines, which is what keeps the gate's verdict stable
        // against the residual's re-evaluation. The one canonical
        // constant policy folds scalars only, so `IN`'s list engages
        // through a parameter; a literal list is not a seek constant
        // and declines.
        let bound = right.seek_value(params)?;
        // The property side: `v.key` directly, or one of the base
        // temporal constructors over exactly `v.key` (a pure parse of the
        // candidate's own value, so `date(p.birthday)` keeps its typed
        // per-row construction).
        let (ctor, key, key_id) = match left.as_ref() {
            Expr::Property { expr, key, key_id } if is_var(expr, var) => (None, key, *key_id),
            Expr::Function {
                name,
                distinct: false,
                args,
            } if args.len() == 1 && name.with_str(temporal::is_constructor) => {
                match &args[0] {
                    // Text render: the compiled predicate owns its ctor
                    // and key strings (interner-facing String domain).
                    Expr::Property { expr, key, key_id } if is_var(expr, var) => {
                        (Some(name.to_string()), key, *key_id)
                    }
                    _ => return None,
                }
            }
            _ => return None,
        };
        let kid = key_id.id_or_else(|| key.with_str(|text| graph.base().prop_key_id(text)));
        Some(match ctor {
            None => Self::Prop {
                key: key.to_string(),
                kid,
                op: *op,
                bound,
            },
            Some(ctor) => Self::Temporal {
                ctor,
                key: key.to_string(),
                kid,
                op: *op,
                bound,
            },
        })
    }
}

/// Whether `expr` is exactly `Variable(var)`.
fn is_var(expr: &Expr, var: &SmallName) -> bool {
    matches!(expr, Expr::Variable(v) if v == var)
}

/// One node variable's compiled scan material: the pattern's static
/// labels interned once, and the conjuncts over exactly that variable.
pub(crate) struct VarSpec {
    labels: Vec<(String, Option<u32>)>,
    preds: Vec<InlinePred>,
}

impl VarSpec {
    /// The candidate gate for one node: the pattern's labels, then the
    /// compiled predicates in residual order, under the one store read
    /// the caller opened. The label-first order is `node_ok`'s — a
    /// predicate that errors must not run for a candidate the label
    /// check would have rejected.
    fn ok(&self, node: &ScanNode<'_>) -> Result<bool> {
        if !self
            .labels
            .iter()
            .all(|(label, lid)| node.has_label(label, *lid))
        {
            return Ok(false);
        }
        for pred in &self.preds {
            if !pred.ok(node)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The pattern's own variable names, split by element kind — the one
/// readiness fact the per-variable order-safety gate consults: node binds
/// run the residual pass (a conjunct over already-bound node variables is
/// settled truthy, or the walk died, before any later node's candidates),
/// rel binds do not (their conjuncts first evaluate at the NEXT node
/// bind). Built once per [`WalkSpec::compile`].
struct PathVars {
    nodes: HashSet<String>,
    rels: HashSet<String>,
}

impl PathVars {
    fn of(path: &PathPattern) -> Self {
        let mut nodes = HashSet::new();
        let mut rels = HashSet::new();
        // Text render: the compiled var sets are String-keyed.
        for np in &path.nodes {
            if let Some(v) = &np.variable {
                nodes.insert(v.to_string());
            }
        }
        for rp in &path.rels {
            if let Some(v) = &rp.variable {
                rels.insert(v.to_string());
            }
        }
        Self { nodes, rels }
    }
}

/// The per-walk compiled scan predicates, keyed by node variable. Built
/// once per `match_resolved_path` (per row per path), carried by
/// [`super::expand::PathMatch`], and consulted by the three walk loops'
/// candidate gates. `settled` names the residual conjuncts (by their
/// index in the walk's `Residual::conjuncts` slice) the gates already
/// evaluated — the walk's residual pass skips them (see
/// [`WalkSpec::compile`]).
pub(crate) struct WalkSpec {
    vars: HashMap<String, VarSpec>,
    settled: Vec<usize>,
}

impl WalkSpec {
    /// Compile for one path walk. A node variable gets an entry only when
    /// the pattern is scan-shaped (a static label conjunction or no label
    /// expression, no pattern property map — those keep `node_ok`'s
    /// property checks), its variable is not already bound by the entry
    /// row (the gate would decline it; the generic path owns rebinds),
    /// and **every** conjunct over exactly that variable compiles — the
    /// all-or-nothing rule that keeps the evaluation order, and so the
    /// error surface, the generic path's.
    ///
    /// The compiled conjuncts are also the walk's `settled` set: the gate
    /// evaluated each of them against the candidate's live record under
    /// the one store read, and the compile rules (seek constants only on
    /// the bound side, all-or-nothing, order safety) exist exactly so
    /// that the residual's re-evaluation would see identical inputs —
    /// so the re-run is redundant work, never a second verdict, and the
    /// walk's residual selection skips it (`ready_conjuncts`' `settled`
    /// parameter). A variable bound by the entry row never settles its
    /// conjuncts: no gate ran for them.
    pub(crate) fn compile(
        graph: &ExecGraph<'_>,
        path: &PathPattern,
        params: &BTreeMap<String, Value>,
        residual: &Residual<'_>,
        entry_bound: Bound<'_>,
    ) -> Self {
        #[cfg(test)]
        if spec_disabled() {
            // The A/B hook kills the whole specialization — no gates, and
            // no settled skips: the disabled run is the generic path
            // exactly, or the differential compares two specializations.
            return Self {
                vars: HashMap::new(),
                settled: Vec::new(),
            };
        }
        // The pattern's own names, split by element kind — the one
        // readiness fact the per-variable order-safety gate consults:
        // node binds run the residual pass (a conjunct over already-bound
        // node variables is settled truthy, or the walk died, before any
        // later node's candidates), rel binds do not (their conjuncts
        // first evaluate at the NEXT node bind).
        let path_vars = PathVars::of(path);
        let mut settled = Vec::new();
        let vars = path
            .nodes
            .iter()
            .filter_map(|np| {
                Self::compile_node(
                    graph,
                    np,
                    &path_vars,
                    params,
                    residual,
                    entry_bound,
                    &mut settled,
                )
            })
            .collect();
        Self { vars, settled }
    }

    /// The residual conjunct indices the gates settled — the walk's
    /// residual pass skips exactly these ([`WalkSpec::compile`]).
    pub(crate) fn settled_conjuncts(&self) -> &[usize] {
        &self.settled
    }

    /// One node pattern's compiled material with its variable name, or
    /// `None` when the variable has nothing to inline (no variable,
    /// ineligible shape, or a conjunct that declines).
    ///
    /// The order-safety gate (the third compile rule): the gate evaluates
    /// this variable's compiled conjuncts at candidate time, before the
    /// residual's written-order pass runs at the node's bind. A conjunct
    /// the gate does not evaluate — one naming other names — is safe
    /// ahead of a compiled one only when it is already settled by an
    /// earlier node bind: it names node variables of this path only,
    /// none of them this variable. Anything else — a conjunct naming
    /// this variable (a cross-variable comparison; which side binds
    /// first is the walk orientation's choice), a conjunct naming a rel
    /// variable (rel binds run no residual pass, so it first evaluates
    /// at the next node bind, this one), or a conjunct naming no path
    /// variable at all (a constant, an outer-scope or subquery-local
    /// name — ready at every bind) — evaluates after the gate's
    /// predicates in the specialized walk and before them in the generic
    /// one, so a short-circuit or a raise on it would diverge. Such a
    /// conjunct preceding a compiled one returns the variable to the
    /// generic path.
    fn compile_node(
        graph: &ExecGraph<'_>,
        np: &NodePattern,
        path_vars: &PathVars,
        params: &BTreeMap<String, Value>,
        residual: &Residual<'_>,
        entry_bound: Bound<'_>,
        settled: &mut Vec<usize>,
    ) -> Option<(String, VarSpec)> {
        let var = np.variable.as_ref()?;
        // Entry-bound variables never take a gate: the row already owns
        // the binding, `gate_spec` would decline it per candidate, and no
        // gate means nothing settled.
        if var.with_str(|text| entry_bound(text)) {
            return None;
        }
        // The scan shapes only: a dynamic label expression and a pattern
        // property map keep `node_ok`'s own checks.
        if np.properties.is_some() {
            return None;
        }
        if np
            .label_expr
            .as_ref()
            .is_some_and(|expr| !expr.is_static_conjunction())
        {
            return None;
        }
        let mut named = HashSet::new();
        let mut preds = Vec::new();
        // Set once a conjunct the gate cannot evaluate — and that is
        // evaluable at this node's bind — precedes this variable's
        // compiled conjuncts: the evaluation order the gate would flip.
        let mut gate_order_broken = false;
        for (index, conjunct) in residual.conjuncts.iter().enumerate() {
            named.clear();
            expr_vars(conjunct, &mut named);
            // `named` is the expr_vars String sink; the IR variable
            // probes it through the text bridge.
            if named.len() != 1 || !var.with_str(|text| named.contains(text)) {
                // Not this variable's conjunct. Safe ahead of a compiled
                // one only when an earlier node bind settles it: it names
                // at least one node variable of this path, never this
                // variable (a cross-variable comparison's readiness is
                // the walk orientation's choice), never a rel variable
                // (rel binds run no residual pass), and at least one name
                // at all (a constant or an outer/subquery-only name is
                // ready at every bind).
                let safe_ahead = !var.with_str(|text| named.contains(text))
                    && !named.is_empty()
                    && !named.iter().any(|name| path_vars.rels.contains(name))
                    && named.iter().any(|name| path_vars.nodes.contains(name));
                if !safe_ahead {
                    gate_order_broken = true;
                }
                continue;
            }
            // The order-safety gate: the compiled conjunct would evaluate
            // before the conjunct that precedes it, so the variable takes
            // the generic path wholesale.
            if gate_order_broken {
                return None;
            }
            // All-or-nothing: one declined conjunct over this variable
            // returns the whole variable to the generic path.
            preds.push(InlinePred::compile(graph, var, conjunct, params)?);
            // The gate evaluates this conjunct for every candidate it
            // accepts: the residual's re-run is redundant, and the walk's
            // residual pass skips it by this index.
            settled.push(index);
        }
        if preds.is_empty() {
            return None;
        }
        let labels = np
            .labels
            .iter()
            .map(|(label, _)| {
                (
                    label.to_string(),
                    label.with_str(|text| graph.base().label_id(text)),
                )
            })
            .collect();
        Some((var.to_string(), VarSpec { labels, preds }))
    }

    /// The gate material one walk loop hoists OUT of its candidate loop
    /// (the per-candidate work must be the store read alone, not a
    /// schema lookup per candidate): `None` when no specialization
    /// applies — no variable, uncompiled, or already row-bound (the
    /// generic path owns rebinds and their tombstone reads). The A/B
    /// hook is answered once at [`WalkSpec::compile`]: a disabled walk
    /// compiles nothing, gates nothing, and skips nothing.
    pub(crate) fn gate_spec<'s>(&'s self, np: &NodePattern, row: &Row) -> Option<&'s VarSpec> {
        let var = np.variable.as_ref()?;
        if var.with_str(|text| row.contains_key(text)) {
            return None;
        }
        var.with_str(|text| self.vars.get(text))
    }
}

/// One candidate under hoisted gate material ([`WalkSpec::gate_spec`]):
/// the single store read that settles it. `None` material settles every
/// candidate — the generic path runs unchanged.
pub(crate) fn candidate_ok(
    graph: &ExecGraph<'_>,
    gate: Option<&VarSpec>,
    id: NodeId,
) -> Result<bool> {
    match gate {
        Some(spec) => {
            #[cfg(test)]
            SPEC_GATES.with(|count| count.set(count.get() + 1));
            graph.node_scan_check(id, |node| spec.ok(&node))
        }
        None => Ok(true),
    }
}
