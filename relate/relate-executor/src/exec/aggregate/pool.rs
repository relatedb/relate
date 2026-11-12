//! The percentile operand pool: one retained `f64` buffer per (group,
//! operand expression), shared by every plain percentile call in that group
//! that reads the same operand.
//!
//! A percentile only needs its operands as numbers, and every call over one
//! operand sees the same sample, so k calls keep one 8-byte-per-row buffer
//! instead of k `Value` buffers. The pool lives in the group's slot of its
//! [`super::columns::AccTable`] column, so it is opened with the group and
//! is never visible to another group key.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::Expr;

use crate::eval::{eval, PercentileSample};
use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::Row;

/// One shared operand: what every group's buffer over it has in common.
#[derive(Clone, Copy)]
pub(super) struct PoolSite<'e> {
    operand: &'e Expr,
    /// Whether every site reading the pool is lenient: only then does an
    /// evaluation failure park instead of failing the statement (the one
    /// rule, [`super::columns::AccTable::offer`]).
    pub(super) lenient: bool,
}

impl<'e> PoolSite<'e> {
    pub(super) fn new(operand: &'e Expr, lenient: bool) -> Self {
        Self { operand, lenient }
    }

    /// Whether a call over `operand` reads this pool.
    pub(super) fn reads(&self, operand: &Expr) -> bool {
        self.operand == operand
    }
}

/// One group's buffer over one pool's operand: the numbers, how many are
/// charged, and how collecting ended.
#[derive(Default)]
pub(super) struct PoolState {
    sample: PercentileSample,
    /// How many of the sample's numbers are already charged.
    charged: usize,
    /// Collecting ended: a non-numeric operand arrived (each reading call
    /// reports its own `{name}() needs numbers` once its percentile argument
    /// validated), or the failure was parked on the reading sites.
    stopped: bool,
    non_numeric: bool,
}

impl PoolState {
    /// Stop collecting: the failure was parked on the reading sites.
    pub(super) fn stop(&mut self) {
        self.stopped = true;
    }

    /// Offer one input row: evaluate the operand once and keep the number.
    pub(super) fn offer(
        &mut self,
        site: &PoolSite<'_>,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
    ) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        graph.cancel_tick()?;
        let value = eval(graph, row, site.operand, params)?;
        if self.sample.offer(&value).is_err() {
            self.stopped = true;
            self.non_numeric = true;
        }
        Ok(())
    }

    /// Charge what the buffer kept since the last settle to the
    /// aggregation's running total: the buffer's growth, once, however many
    /// calls read it.
    pub(super) fn settle(
        &mut self,
        graph: &ExecGraph<'_>,
        total: &mut RetainedTotal,
    ) -> Result<()> {
        let len = self.sample.len();
        if len > self.charged {
            total.add_bytes(
                graph,
                crate::workspace::buffer_growth(self.charged, len, size_of::<f64>() as u64),
            )?;
            self.charged = len;
        }
        Ok(())
    }

    /// The group's resolved sample: sorted once, read by every call.
    pub(super) fn finish(self) -> SortedPool {
        SortedPool {
            vals: self.sample.into_sorted(),
            non_numeric: self.non_numeric,
        }
    }
}

/// A pool at resolve time: the ascending sample, and whether a non-numeric
/// operand ended collecting.
pub(super) struct SortedPool {
    pub(super) vals: Vec<f64>,
    pub(super) non_numeric: bool,
}

/// Whether evaluating `operand` twice in one row gives one answer, so that
/// two calls may read one evaluation. A whitelist of the pure, row-local
/// forms (variables, properties, parameters, literals, operators over
/// them): a clock or random call, a subquery, or an extension function
/// stays one evaluation per call, exactly as before.
pub(super) fn shareable(operand: &Expr) -> bool {
    match operand {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Parameter(_)
        | Expr::Variable(_) => true,
        Expr::Property { expr, .. } | Expr::Unary { expr, .. } => shareable(expr),
        Expr::Binary { left, right, .. } => shareable(left) && shareable(right),
        _ => false,
    }
}
