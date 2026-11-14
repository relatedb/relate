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
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::table::GroupBill;

/// One shared operand: what every group's buffer over it has in common.
pub(super) struct PoolSite<'e> {
    operand: &'e Expr,
    /// The indices of the sites that read this pool, assigned as
    /// [`super::columns::AccTable::new`] routes them — the reverse edge of
    /// the sites' pool reference, so a pool's failure parks its readers
    /// without scanning every site. Whether the pool may park at all is
    /// the readers' own lenience, read from them where it is needed.
    pub(super) readers: Vec<usize>,
}

impl<'e> PoolSite<'e> {
    /// A pool is born with the one site that first routed to it, so the
    /// reverse edge of the sites' pool reference has no reader-less
    /// spelling: a pool with no reader could never answer for its own
    /// lenience.
    pub(super) fn new(operand: &'e Expr, reader: usize) -> Self {
        Self {
            operand,
            readers: vec![reader],
        }
    }

    /// Whether a call over `operand` reads this pool.
    pub(super) fn reads(&self, operand: &Expr) -> bool {
        self.operand == operand
    }
}

/// How a pool's collecting stands: open, or ended one of the two ways it
/// can — a non-numeric operand arrived (each reading call reports its own
/// `{name}() needs numbers` once its percentile argument validated), or
/// the failure was parked on the reading sites (they answer with the
/// parked error, never from the sample).
#[derive(Default)]
pub(super) enum Collecting {
    #[default]
    Open,
    NonNumeric,
    Parked,
}

/// One group's buffer over one pool's operand: the numbers, how many are
/// charged, and how collecting stands.
#[derive(Default)]
pub(super) struct PoolState {
    sample: PercentileSample,
    /// How many of the sample's numbers are already charged.
    charged: usize,
    ending: Collecting,
}

impl PoolState {
    /// End collecting, one typed way: the buffer is released with its
    /// charge cursor, whichever ending it was.
    pub(super) fn stop_collecting(&mut self, ending: Collecting) {
        self.ending = ending;
        self.sample = PercentileSample::default();
        self.charged = 0;
    }

    /// How many numbers the buffer holds.
    #[cfg(test)]
    pub(super) fn held(&self) -> usize {
        self.sample.len()
    }

    /// Offer one input row: evaluate the operand once and keep the number.
    pub(super) fn offer(
        &mut self,
        site: &PoolSite<'_>,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
    ) -> Result<()> {
        if !matches!(self.ending, Collecting::Open) {
            return Ok(());
        }
        graph.cancel_tick()?;
        let value = eval(graph, row, site.operand, params)?;
        if self.sample.offer(&value).is_err() {
            self.stop_collecting(Collecting::NonNumeric);
        }
        Ok(())
    }

    /// Charge what the buffer kept since the last settle to the
    /// aggregation's one bill: the buffer's growth, once, however many
    /// calls read it. The bill reserves the same bytes on the workspace
    /// ledger when admission is on.
    pub(super) fn settle(&mut self, graph: &ExecGraph<'_>, bill: &mut GroupBill) -> Result<()> {
        let len = self.sample.len();
        if len > self.charged {
            bill.add(
                graph,
                crate::workspace::buffer_growth(self.charged, len, size_of::<f64>() as u64),
            )?;
            self.charged = len;
        }
        Ok(())
    }

    /// The group's resolved sample: sorted once, read by every call. A
    /// parked failure is not a sample ending — its sites hold the error
    /// and answer with it, and this sample was released — so it maps to
    /// the open ending here and never reaches a reader.
    pub(super) fn finish(self) -> SortedPool {
        SortedPool {
            vals: self.sample.into_sorted(),
            ending: match self.ending {
                Collecting::NonNumeric => SampleEnding::NonNumeric,
                Collecting::Open | Collecting::Parked => SampleEnding::Open,
            },
        }
    }
}

/// A resolved pool's ending: collecting never ended, or it ended on a
/// non-numeric operand. Only the endings a reader can answer for — a
/// parked pool resolves through its sites' failure map, not its sample.
#[derive(Default)]
pub(super) enum SampleEnding {
    #[default]
    Open,
    NonNumeric,
}

/// A pool at resolve time: the ascending sample, and how collecting ended —
/// the endings a reader can answer for, so no reader reads one of the
/// endings back out of a bool.
pub(super) struct SortedPool {
    pub(super) vals: Vec<f64>,
    pub(super) ending: SampleEnding,
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
