//! Decoded bytes of checkpoint rows (ADR-0183 Decision 2), measured from the
//! in-memory graph without touching a file. Rewrite-unit planning sizes each
//! unit and cuts it into files with one [`DecodedBudget`] walk, so the cuts
//! and the sizes come from the same sum.

use relate_core::{Error, Result};
use relate_executor::Graph;

use crate::snapshot::Kind;
use crate::storage_context;

#[cfg(test)]
use super::invalid;
use super::{check_row, encode::fixed_column_bytes, envelope, row_fits};

#[cfg(test)]
thread_local! {
    static MEASURED_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Live rows [`DecodedBudget`] measured since the last call.
#[cfg(test)]
pub(crate) fn take_measured_rows() -> usize {
    MEASURED_ROWS.replace(0)
}

impl Kind {
    /// Full decoded row size, or `None` when `id` is not visible at `ts`:
    /// the fixed Int64 columns plus the executor's variable payload (labels
    /// or type name, and tagged property JSON). Only [`DecodedBudget`]
    /// measures rows.
    fn checkpoint_bytes(self, graph: &Graph, id: i64, ts: u64) -> Result<Option<u64>> {
        let variable = match self {
            Self::Node => graph.node_checkpoint_bytes(id, ts)?,
            Self::Rel => graph.rel_checkpoint_bytes(id, ts)?,
        };
        variable
            .map(|n| {
                n.checked_add(fixed_column_bytes(self))
                    .ok_or_else(|| Error::storage("checkpoint byte count overflow"))
            })
            .transpose()
    }
}

/// Decoded bytes of consecutive live rows, held under a byte target, the
/// kind's page-envelope row cap (ADR-0534: a wide part's many leaf columns
/// make rows, not bytes, the binding bound), and the ADR-0086 row and
/// decoded envelopes.
pub(crate) struct DecodedBudget<'g> {
    graph: &'g Graph,
    ts: u64,
    kind: Kind,
    /// The most rows one file of this kind holds within ADR-0086's
    /// `MAX_PAGES` — the wide leaf count makes rows, not bytes, the
    /// binding bound (ADR-0534). Hoisted once: the bound never changes
    /// for a budget's files.
    row_limit: usize,
    limit: u64,
    /// Rows admitted to the file being filled, and their decoded bytes.
    rows: usize,
    bytes: u64,
}

fn add(bytes: u64, row: u64) -> Result<u64> {
    bytes
        .checked_add(row)
        .ok_or_else(|| Error::storage("decoded byte total overflow"))
}

impl<'g> DecodedBudget<'g> {
    pub(crate) fn new(graph: &'g Graph, ts: u64, kind: Kind, target: u64, wide: bool) -> Self {
        Self {
            graph,
            ts,
            kind,
            row_limit: kind.page_envelope_rows(wide),
            limit: target.min(envelope().decoded),
            rows: 0,
            bytes: 0,
        }
    }

    /// The two-dimensional admission policy, stated once: a file of
    /// `rows` rows and `bytes` decoded bytes fits when both stay within
    /// the target and the kind's page-envelope row cap.
    fn admits(&self, rows: usize, bytes: u64) -> bool {
        bytes <= self.limit && rows <= self.row_limit
    }

    /// Decoded bytes of `id`'s row, or `None` when it is not visible at `ts`.
    fn measure(&self, id: i64) -> Result<Option<u64>> {
        let row = self.kind.checkpoint_bytes(self.graph, id, self.ts)?;
        #[cfg(test)]
        if row.is_some() {
            MEASURED_ROWS.set(MEASURED_ROWS.get() + 1);
        }
        Ok(row)
    }

    /// Cut the rows of `ids` into files in one walk that measures each row
    /// once: a row that would take its file past the target, the decoded
    /// envelope, or the row cap starts the next file, so a row over the
    /// target by itself is a file alone. Ids not visible at `ts` cost
    /// nothing, and a file only starts at a live row. Returns the id each
    /// file after the first starts at; the budget is left holding the last
    /// file. A row that cannot fit the row or decoded envelope by itself
    /// is an error naming its kind and id.
    pub(crate) fn cut(&mut self, ids: &[i64]) -> Result<Vec<i64>> {
        let mut starts = Vec::new();
        for &id in ids {
            let Some(row) = self.measure(id)? else {
                continue;
            };
            check_row(row)
                .map_err(|e| storage_context(e, format_args!("{} {id}", self.kind.noun())))?;
            let next = add(self.bytes, row)?;
            if !self.admits(self.rows + 1, next) && !self.is_empty() {
                starts.push(id);
                (self.rows, self.bytes) = (1, row);
            } else {
                (self.rows, self.bytes) = (self.rows + 1, next);
            }
        }
        Ok(starts)
    }

    /// Whether the rows held fit the target and the row cap; only a
    /// single row over the target does not.
    pub(crate) fn within(&self) -> bool {
        self.admits(self.rows, self.bytes)
    }

    /// Admit every row of `ids`, or none of them. A row over the ADR-0086
    /// row envelope, the target, or the row cap does not fit either,
    /// rather than failing: `ids` belong to a part the caller then leaves
    /// as it is.
    pub(crate) fn fill_all(&mut self, ids: &[i64]) -> Result<bool> {
        let (mut rows, mut bytes) = (self.rows, self.bytes);
        for &id in ids {
            let Some(row) = self.measure(id)? else {
                continue;
            };
            if !row_fits(row) {
                return Ok(false);
            }
            bytes = add(bytes, row)?;
            rows += 1;
            if !self.admits(rows, bytes) {
                return Ok(false);
            }
        }
        (self.rows, self.bytes) = (rows, bytes);
        Ok(true)
    }

    /// Whether no row is admitted to the file being filled.
    fn is_empty(&self) -> bool {
        self.rows == 0
    }
}

/// Decoded bytes of the live rows of `ids`, which must fit the ADR-0086
/// decoded envelope together.
#[cfg(test)]
pub(crate) fn decoded_preflight(graph: &Graph, ts: u64, ids: &[i64], kind: Kind) -> Result<u64> {
    let mut budget = DecodedBudget::new(graph, ts, kind, u64::MAX, false);
    if !budget.cut(ids)?.is_empty() {
        return Err(invalid("actual decoded bytes"));
    }
    Ok(budget.bytes)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::commit_ops;
    use super::super::{with_test_envelope, MAX_DECODED};
    use super::*;
    use relate_core::Value;

    /// Six nodes of one size and a relationship between the first two, and
    /// the decoded bytes of one node row.
    fn graph() -> (Graph, u64, u64) {
        let graph = Graph::default();
        graph.begin_write().unwrap();
        let props = std::collections::BTreeMap::from([("p".into(), Value::String("x".into()))]);
        for _ in 0..6 {
            graph.create_node(Vec::new(), props.clone()).unwrap();
        }
        graph.create_rel(0, 1, "R".into(), props).unwrap();
        let ts = commit_ops(&graph);
        let row = Kind::Node.checkpoint_bytes(&graph, 0, ts).unwrap().unwrap();
        (graph, ts, row)
    }

    #[test]
    fn cut_returns_the_id_each_later_file_starts_at() {
        let (graph, ts, row) = graph();
        let mut budget = DecodedBudget::new(&graph, ts, Kind::Node, 2 * row, false);
        // Id 100 is not visible, so it costs nothing and starts no file.
        assert_eq!(budget.cut(&[0, 1, 2, 3, 4, 100, 5]).unwrap(), [2, 4]);
        assert_eq!((budget.rows, budget.bytes), (2, 2 * row));
    }

    #[test]
    fn a_row_over_the_row_envelope_fails_a_cut_and_does_not_fit_a_fill() {
        let (graph, ts, row) = graph();
        with_test_envelope(1 << 20, row - 1, MAX_DECODED, || {
            for (kind, named) in [(Kind::Node, "node 0: "), (Kind::Rel, "relationship 0: ")] {
                let err = DecodedBudget::new(&graph, ts, kind, u64::MAX, false)
                    .cut(&[0])
                    .unwrap_err();
                let expected = format!("{named}invalid or out-of-bounds 384 MiB decoded row");
                assert!(err.to_string().contains(&expected), "{err}");
            }
            let mut budget = DecodedBudget::new(&graph, ts, Kind::Node, u64::MAX, false);
            assert!(!budget.fill_all(&[0]).unwrap());
            assert!(budget.is_empty());
        });
    }

    #[test]
    fn fill_all_admits_every_row_or_none_and_counts_them() {
        let (graph, ts, row) = graph();
        let mut budget = DecodedBudget::new(&graph, ts, Kind::Node, 3 * row, false);
        assert!(budget.is_empty());
        // Ids that are not visible admit no row.
        assert!(budget.fill_all(&[100, 101]).unwrap());
        assert!(budget.is_empty());
        assert!(budget.fill_all(&[0]).unwrap());
        assert!(!budget.fill_all(&[1, 2, 3]).unwrap());
        assert_eq!((budget.rows, budget.bytes), (1, row));
        assert!(budget.fill_all(&[1, 2]).unwrap());
        assert_eq!((budget.rows, budget.bytes), (3, 3 * row));
        assert!(budget.within());
    }
}
