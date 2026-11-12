//! Early evaluation of MATCH-WHERE conjuncts inside the GPM walk: a
//! conjunct whose referenced names are all bound **and bind exactly once**
//! in this pattern is evaluated the moment its last name binds — pre-hop
//! on the anchor side (the boundary node an expansion is about to walk
//! from), not once per completed path at the target side.
//!
//! Exactness (the ADR-0469 allReduce inliner's discipline, generalized
//! from allReduce folds to plain conjuncts):
//!
//! * **Bind-once gate.** A conjunct qualifies only when every name it
//!   references is either bound in the entering row or the variable of a
//!   top-level node element, or of a plain (non-var-length, non-quantified)
//!   top-level relationship element — and *no* referenced name is a
//!   quantified group's name (those rebind per iteration; the post-filter
//!   reads the last binding, an early evaluation would read an earlier
//!   one) or the pattern's path variable (bound after the whole pattern).
//!   A qualified conjunct's inputs at the early point are therefore
//!   exactly the inputs the post-filter sees, so its verdict there is its
//!   verdict here.
//! * **Null bindings never decide.** Readiness requires every referenced
//!   name bound *non-null*: a null binding of a pattern variable may be a
//!   CIP-28 non-local singleton pre-binding (ADR-0335 — a later-declared
//!   name referenced inside the quantification is pre-bound to null and
//!   rebound at its own site) or a deleted entity's tombstone; either
//!   value differs from the post-filter's, so such a conjunct stays a
//!   post-filter.
//! * **Errors defer, never prune** (the allReduce poison rule, walk-wide):
//!   a conjunct that raises is deferred for the rest of the walk and the
//!   untouched post-filter reproduces the error on whatever rows survive
//!   — pruning never changes *which errors surface*.
//! * The post-filter **stays in place**: the early evaluation only ever
//!   removes prefixes whose every completed row the post-filter would
//!   drop (a failed or null conjunct verdict), and a failed prefix
//!   enumerates no further hops — the branch abort.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};

use relate_core::{Result, Value};
use relate_ir::{Expr, PathPattern, PatternElement};
use relate_names::SmallName;

use crate::eval::{expr_vars, predicate_holds};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::pushdown::Residual;

/// One early-evaluable conjunct of the MATCH WHERE: the expression plus
/// the scope names its verdict depends on (readiness is every one of them
/// bound non-null at the evaluation point).
pub(super) struct EarlyPredicate<'c> {
    expr: &'c Expr,
    vars: Vec<String>,
}

/// The early conjuncts of one GPM walk, extracted per entering row:
/// the qualifying conjuncts of the MATCH WHERE, plus the walk-wide
/// error-deferral set (a raised conjunct is never evaluated early again
/// in this walk — the post-filter, which stays in place, reproduces its
/// error on whatever rows survive).
pub(super) struct EarlyPredicates<'c> {
    specs: Vec<EarlyPredicate<'c>>,
    deferred: RefCell<HashSet<usize>>,
}

impl<'c> EarlyPredicates<'c> {
    /// Whether the early conjuncts hold on `row`, evaluating exactly the
    /// ones whose reading completed at `event` — the variable this
    /// binding point binds (a node element's or target's variable at an
    /// accept, the relationship's variable at a rel accept). `None` binds
    /// nothing, so nothing is evaluated. A conjunct that is ready but
    /// completed at an earlier point is not re-evaluated: its verdict
    /// cannot have changed (bind-once inputs), and re-evaluating it per
    /// hop would put the cost back the early evaluation exists to remove.
    pub(super) fn holds_at(
        &self,
        graph: &ExecGraph<'_>,
        row: &Row,
        event: Option<&SmallName>,
        params: &BTreeMap<String, Value>,
    ) -> Result<bool> {
        #[cfg(test)]
        if early_predicates_disabled() {
            return Ok(true);
        }
        let Some(event) = event else {
            return Ok(true);
        };
        for (index, spec) in self.specs.iter().enumerate() {
            // A conjunct that raised at an earlier binding point is
            // deferred for the rest of the walk: skipped here, never
            // re-evaluated early and never pruning, so the post-filter —
            // which stays in place — reproduces its error on whatever
            // rows survive.
            if self.deferred.borrow().contains(&index) {
                continue;
            }
            if !spec.vars.iter().any(|name| event == name.as_str()) {
                continue;
            }
            if !spec
                .vars
                .iter()
                .all(|name| row.get(name).is_some_and(|v| !v.is_null()))
            {
                continue;
            }
            #[cfg(test)]
            count_early_eval();
            match predicate_holds(graph, row, spec.expr, params) {
                // A raised conjunct defers (the allReduce poison rule,
                // walk-wide): the post-filter reproduces the error on
                // whatever rows survive, so which errors surface never
                // changes.
                Err(_) => {
                    self.deferred.borrow_mut().insert(index);
                }
                Ok(false) => return Ok(false),
                Ok(true) => {}
            }
        }
        Ok(true)
    }
}

/// The MATCH-WHERE conjuncts this pattern's walk can evaluate early: the
/// top-level `AND` conjuncts whose every referenced (scope) name is bound
/// entering the walk or binds exactly once inside it — never a rebinding
/// name (a quantified group's, the path variable's). Everything else
/// stays a post-filter, unpruned.
pub(super) fn early_predicates<'c>(
    path: &PathPattern,
    row: &Row,
    residual: &Residual<'c>,
) -> EarlyPredicates<'c> {
    // A path selector (ANY/ALL SHORTEST and friends) chooses among the
    // pattern's FULL candidate set — a MATCH-WHERE predicate narrows the
    // already-selected path and can answer nothing (CIP-60's selective
    // path patterns, scenario 8) — so no conjunct may prune the walk: an
    // early evaluation that removed a candidate would change which path
    // the selector picks. Every conjunct stays a post-filter.
    if path.selector.is_some() {
        return EarlyPredicates {
            specs: Vec::new(),
            deferred: RefCell::new(HashSet::new()),
        };
    }
    let mut rebinding: HashSet<String> = HashSet::new();
    let mut bind_once: HashSet<String> = HashSet::new();
    for element in &path.elements {
        match element {
            PatternElement::Node(node) => {
                if let Some(name) = &node.variable {
                    bind_once.insert(name.to_string());
                }
            }
            PatternElement::Relationship(rel) => {
                if let Some(name) = &rel.variable {
                    // A var-length or quantified rel's variable is a group
                    // list (rebound per iteration, completed at the end);
                    // only a plain rel's variable binds once.
                    if !rel.var_length && !rel.quantified {
                        bind_once.insert(name.to_string());
                    } else {
                        rebinding.insert(name.to_string());
                    }
                }
            }
            PatternElement::Quantified(qpp) => {
                // The group's whole rebinding vocabulary — inner element
                // names plus the subpath variable — from the one spelling
                // every consumer reads.
                let mut names = Vec::new();
                super::gpm_group_vocabulary(qpp, &mut names);
                rebinding.extend(names);
            }
        }
    }
    if let Some(name) = &path.variable {
        rebinding.insert(name.to_string());
    }
    let mut specs = Vec::new();
    for conjunct in residual.conjuncts {
        let mut vars = HashSet::new();
        expr_vars(conjunct, &mut vars);
        vars.retain(|name| residual.scope.contains(name));
        let eligible = vars.iter().all(|name| {
            // A rebinding name is never early-decided; anything the row
            // already binds or a bind-once pattern name is stable.
            !rebinding.contains(name) && (row.contains_key(name) || bind_once.contains(name))
        });
        if eligible {
            specs.push(EarlyPredicate {
                expr: conjunct,
                vars: vars.into_iter().collect(),
            });
        }
    }
    EarlyPredicates {
        specs,
        deferred: RefCell::new(HashSet::new()),
    }
}

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    /// Test hook: stand the early evaluation down (every conjunct stays a
    /// post-filter), so a differential test can run the very same
    /// statement both ways.
    static EARLY_PREDICATES_DISABLED: Cell<bool> = const { Cell::new(false) };
    /// Test counter: how many times an early conjunct was evaluated.
    static EARLY_EVALS: Cell<u64> = const { Cell::new(0) };
}

/// Run `f` with the early predicate evaluation stood down. The flag is
/// restored on unwind too, so a panicking assertion inside the closure
/// cannot leave the evaluation disabled for every later test on the
/// thread.
#[cfg(test)]
pub(super) fn with_early_predicates_disabled<R>(f: impl FnOnce() -> R) -> R {
    super::super::test_support::with_flag_disabled(&EARLY_PREDICATES_DISABLED, f)
}

#[cfg(test)]
fn early_predicates_disabled() -> bool {
    EARLY_PREDICATES_DISABLED.with(Cell::get)
}

#[cfg(test)]
fn count_early_eval() {
    EARLY_EVALS.with(|c| c.set(c.get() + 1));
}

/// Test counter take: how many early conjunct evaluations happened since
/// the last take.
#[cfg(test)]
pub(super) fn take_early_evals() -> u64 {
    EARLY_EVALS.with(|c| c.replace(0))
}
