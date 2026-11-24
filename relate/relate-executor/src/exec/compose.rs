//! The `CALL` row-composition policy (the shared spine of the generic
//! registry path, the fulltext and vector scan calls, and `db.ontology.get`):
//! resolve the effective YIELD list once, then per input row and per produced
//! column→value map, clone the input, insert each yielded cell under
//! `alias.unwrap_or(column)` (`Null` for a column the producer did not emit),
//! apply the optional-WHERE gate, and pad one null row when an OPTIONAL call
//! produced nothing for the input row.
//!
//! The policy is a [`FeedSource`] (the feed spine in `feed.rs`, ADR-0555):
//! a projection follower — the same `projection_follows` guard `UNWIND` and
//! `LOAD CSV` park by — pulls the composed rows one at a time and they are
//! never held, so the input-rows × procedure-rows cross product costs
//! constant memory however wide it is. The materializing form
//! ([`ComposeSource::compose_into`]) charges each input row's production to
//! the running total before its first row is built (ADR-0463).

use std::collections::BTreeMap;
use std::rc::Rc;

use relate_core::{Result, Value};
use relate_ir::{Expr, YieldItems};

use crate::ctx::ExecCtx;
use crate::eval::where_clause_holds;
use crate::row::Row;

use super::feed::{held_rows, FeedSource};

/// What one input row's call produced, as the composition's input: row maps
/// to compose under the YIELD keys — owned, when this row's bound arguments
/// computed them (a built-in, a fulltext or vector scan) — or shared, when
/// they are a stored set this row's arguments matched, by index (a
/// registered procedure's rows, a no-argument built-in's once-produced
/// set).
pub(crate) enum Produced {
    Rows(Vec<BTreeMap<String, Value>>),
    Shared {
        rows: Rc<Vec<BTreeMap<String, Value>>>,
        matched: Vec<usize>,
    },
}

impl Produced {
    /// Whether a fresh production contributes nothing — read at `open`,
    /// where the cursor is zero.
    fn is_empty(&self) -> bool {
        self.remaining(0) == 0
    }

    /// The row count from `cursor` on — the open composition's charge
    /// count, and its emptiness test once spent.
    fn remaining(&self, cursor: usize) -> usize {
        match self {
            Produced::Rows(maps) => maps.len() - cursor,
            Produced::Shared { matched, .. } => matched.len() - cursor,
        }
    }

    /// The next produced row map at `cursor`, advancing it — `None` once
    /// spent.
    fn next_map(&mut self, cursor: &mut usize) -> Option<&BTreeMap<String, Value>> {
        let next = match self {
            Produced::Rows(maps) => maps.get(*cursor),
            Produced::Shared { rows, matched } => matched.get(*cursor).map(|idx| &rows[*idx]),
        };
        if next.is_some() {
            *cursor += 1;
        }
        next
    }
}

/// One open composition: the input row being expanded (the template every
/// composed row clones), its produced row sources in pull order, and
/// whether any composed row passed the WHERE gate — the OPTIONAL pad's own
/// fact (the pad row, when it fires, counts as passed, so a further pull
/// is exhaustion).
struct OpenCompose {
    input: Row,
    produced: Produced,
    cursor: usize,
    passed_any: bool,
}

/// One resolved YIELD column: the producer's lookup key and the binding
/// the composed row carries (`alias`, else the column) — the two roles a
/// same-typed pair would leave to the comment.
struct YieldColumn {
    column: String,
    key: String,
}

/// The `CALL` composition as a [`FeedSource`]: the composition policy
/// spelled once, over a per-row producer. The producer is answered per
/// input row at `open`; the composed rows are built one at a time at
/// `pull`, never collected.
pub(crate) struct ComposeSource<'a> {
    /// The resolved YIELD table, one [`YieldColumn`] per yielded column —
    /// resolved once with the caller's `default_yields`; every pull reads
    /// the same table.
    resolved: Vec<YieldColumn>,
    optional: bool,
    where_clause: Option<&'a Expr>,
    produce: Producer<'a>,
    current: Option<OpenCompose>,
}

/// The per-input-row producer the source drives: answered once per input
/// row at `open`, its rows composed one at a time at `pull`.
type Producer<'a> = Box<dyn FnMut(&Row) -> Result<Produced> + 'a>;

impl<'a> ComposeSource<'a> {
    pub(crate) fn new(
        default_yields: &[&str],
        yield_items: &YieldItems,
        optional: bool,
        where_clause: Option<&'a Expr>,
        produce: impl FnMut(&Row) -> Result<Produced> + 'a,
    ) -> Self {
        let resolved: Vec<YieldColumn> = yield_items
            .resolved_pairs(default_yields)
            .into_iter()
            .map(|(column, alias)| YieldColumn {
                key: alias.unwrap_or_else(|| column.clone()),
                column,
            })
            .collect();
        Self {
            resolved,
            optional,
            where_clause,
            produce: Box::new(produce),
            current: None,
        }
    }

    /// The clause's materializing form: the feed spine's `materialize`, for
    /// a consumer that cannot fold rows as they arrive — each input row's
    /// production is charged to the running total before its first row is
    /// built (ADR-0463).
    pub(crate) fn compose_into(self, ctx: ExecCtx<'_>, rows: Vec<Row>) -> Result<Vec<Row>> {
        held_rows(ctx, rows, Box::new(self), None)
    }

    /// One produced row's yielded cells: cloned per yielded column only,
    /// never a full-row deep clone — a wide procedure row costs one cell
    /// per yielded column. A declared column the produced row does not
    /// carry reads Null — the same leniency a registered procedure's
    /// stored rows have always had for a missing column, never the
    /// OPTIONAL pad's spelling (the pad builds its explicit nulls itself).
    fn yielded_cells(resolved: &[YieldColumn], values: &BTreeMap<String, Value>) -> Vec<Value> {
        resolved
            .iter()
            .map(|column| values.get(&column.column).cloned().unwrap_or(Value::Null))
            .collect()
    }
}

impl FeedSource for ComposeSource<'_> {
    fn open(&mut self, _ctx: ExecCtx<'_>, row: Row) -> Result<bool> {
        // The open state is cleared first, so `current` holds a composition
        // exactly when the last open succeeded: a row that contributes
        // nothing leaves no stale composition behind, and the charge and
        // pull reads below state the source's own fact, not a caller
        // convention.
        self.current = None;
        let produced = (self.produce)(&row)?;
        // An empty production contributes nothing — skipped, never billed —
        // unless the call is OPTIONAL, which pads one null row once the
        // source is spent.
        if produced.is_empty() && !self.optional {
            return Ok(false);
        }
        self.current = Some(OpenCompose {
            input: row,
            produced,
            cursor: 0,
            passed_any: false,
        });
        Ok(true)
    }

    fn charge_facts(&self) -> Result<(usize, usize)> {
        // Read off the open composition alone — the materializing form's
        // own fact: the streamed path never counts what it will not bill.
        // The count bills the composed rows the walk will build; the WHERE
        // gate can only remove a row, never add one, and re-running it per
        // candidate here would build every row twice, so the materializing
        // form bills the pre-gate count — an upper bound, never an
        // undercount. The stored set behind `Shared` is the procedure's own
        // closed output (ADR-0555's exemption); only the composed rows are
        // charged. The OPTIONAL pad row (at most one per input row, only
        // when nothing passed) is the one unbilled row.
        let current = self
            .current
            .as_ref()
            .expect("charge_facts is read on an open source");
        Ok((
            current.produced.remaining(current.cursor),
            current.input.len().saturating_add(self.resolved.len()),
        ))
    }

    fn pull(&mut self, ctx: ExecCtx<'_>) -> Result<Option<Row>> {
        let Some(current) = self.current.as_mut() else {
            return Ok(None);
        };
        loop {
            // The cancel sample sits at the loop head — before the skip —
            // so a long cross product stays cancellable.
            ctx.graph.cancel_tick()?;
            // The pad row is the composition of an empty produced map —
            // every column absent, each cell the explicit Null — through
            // the same insert path as every composed row, once: it counts
            // as the one row that passed, so a further pull is exhaustion.
            // The pad is not a WHERE candidate: it is the row the call
            // guarantees when nothing matched, never a predicate's job to
            // drop.
            let (cells, pad) = match current.produced.next_map(&mut current.cursor) {
                Some(map) => (Self::yielded_cells(&self.resolved, map), false),
                None if !current.passed_any && self.optional => {
                    current.passed_any = true;
                    (vec![Value::Null; self.resolved.len()], true)
                }
                None => return Ok(None),
            };
            let mut row = current.input.clone();
            for (column, cell) in self.resolved.iter().zip(cells) {
                row.insert(column.key.clone(), cell);
            }
            if pad {
                return Ok(Some(row));
            }
            // The one WHERE spelling: a non-boolean predicate is the typed
            // rejection every other WHERE site raises, never a silent drop.
            if where_clause_holds(ctx.graph, &row, self.where_clause, ctx.params)? {
                current.passed_any = true;
                return Ok(Some(row));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate_core::{CapacityKind, Error, Value};

    use super::super::test_support::{
        exec_autocommit, exec_cypher_on, profiled_write_routed, try_exec_cypher_on,
    };
    use crate::graph::{Graph, Procedure};
    use crate::ExecGraph;

    /// The clause-under-test's registered procedure: a closed stored set of
    /// `rows` rows, one `v` column, no arguments — the shape whose
    /// composition with the driving rows is the input-rows × procedure-rows
    /// product the streaming form must not hold.
    fn big_call_graph(rows: i64) -> Graph {
        let graph = Graph::default();
        graph.register_procedure(Procedure {
            name: "big.proc".to_string(),
            arg_names: Vec::new(),
            arg_types: Vec::new(),
            yield_names: vec!["v".to_string()],
            rows: (0..rows)
                .map(|i| BTreeMap::from([("v".to_string(), Value::Int(i))]))
                .collect(),
        });
        graph
    }

    /// REL8-1845 (ADR-0463): the composition's materializing form charges
    /// the cross product before its first row is built — a plain (folding
    /// incapable) projection over 20 driving rows x a 50k-row procedure
    /// must refuse at a lowered cap instead of building the million rows.
    #[test]
    fn a_plain_projection_over_the_cross_product_charges_the_composed_rows() {
        let mut graph = big_call_graph(50_000);
        graph.configure_expression_memory_limit(Some(1 << 20));
        let view = ExecGraph::read(&graph);
        let error = try_exec_cypher_on(
            &view,
            "UNWIND range(1, 20) AS i CALL big.proc() YIELD v RETURN v",
        )
        .err()
        .expect("a materialized CALL composition must be refused at the lowered cap");
        assert!(
            matches!(
                &error,
                Error::Capacity {
                    kind: CapacityKind::Expression,
                    ..
                }
            ),
            "expected the typed expression-capacity refusal, got: {error}"
        );
    }

    /// REL8-1845 (ADR-0555): the same procedure, the same cap, the same
    /// clause — only the consumer differs (the file-identical pair the
    /// LOAD CSV charge test uses). A folding consumer keeps the call's
    /// generator, so the million composed rows are never held and the
    /// answer is one integer.
    #[test]
    fn a_folding_consumer_streams_the_cross_product_whatever_its_size() {
        let mut graph = big_call_graph(50_000);
        graph.configure_expression_memory_limit(Some(1 << 20));
        let view = ExecGraph::read(&graph);
        let result = exec_cypher_on(
            &view,
            "UNWIND range(1, 20) AS i CALL big.proc() YIELD v RETURN count(v) AS c",
        );
        assert_eq!(result.rows, vec![vec![Value::Int(1_000_000)]]);
    }

    /// REL8-1845 (ADR-0555): the ordered window folds the composed rows
    /// the same way — the answer is the top 5 of a 50k-row procedure under
    /// the cap the plain projection refuses at.
    #[test]
    fn an_ordered_window_keeps_the_top_rows_whatever_the_procedure_size() {
        let mut graph = big_call_graph(50_000);
        graph.configure_expression_memory_limit(Some(1 << 20));
        let view = ExecGraph::read(&graph);
        let result = exec_cypher_on(
            &view,
            "CALL big.proc() YIELD v RETURN v ORDER BY v DESC LIMIT 5",
        );
        let top: Vec<Vec<Value>> = (49_995..50_000)
            .rev()
            .map(|v| vec![Value::Int(v)])
            .collect();
        assert_eq!(result.rows, top);
    }

    /// REL8-1845: a streamed `CALL` reports its own clause facts after its
    /// consumer drained it — the rows it composed, at its own PROFILE
    /// position — while the consuming aggregation reports its own (the
    /// LOAD CSV parked-feed shape).
    #[test]
    fn a_streamed_call_records_its_own_clause_facts() {
        let graph = Graph::default();
        exec_autocommit(&graph, "CREATE (:User), (:Admin)").expect("the fixture's CREATE succeeds");
        let records = profiled_write_routed(
            &graph,
            "CALL db.labels() YIELD label RETURN count(label) AS c",
        );
        assert_eq!(
            records.len(),
            2,
            "the CALL and its consumer, one record each"
        );
        let call = &records[0];
        assert_eq!(call.clause, 0);
        assert_eq!(
            call.rows, 2,
            "the parked feed's facts report after the drain"
        );
        assert_eq!(records[1].clause, 1);
    }

    /// The OPTIONAL pad through the parked path: a call whose stored rows
    /// match none of the bound arguments pads exactly one null row — one
    /// row for the aggregation to count — while the plain call composes
    /// nothing.
    #[test]
    fn an_optional_call_pads_one_null_row_through_a_folding_consumer() {
        let graph = Graph::default();
        graph.register_procedure(Procedure {
            name: "custom.pad".to_string(),
            arg_names: vec!["x".to_string()],
            arg_types: vec!["ANY".to_string()],
            yield_names: vec!["out".to_string()],
            rows: vec![BTreeMap::from([
                ("x".to_string(), Value::Int(1)),
                ("out".to_string(), Value::Int(10)),
            ])],
        });
        let view = ExecGraph::read(&graph);
        let plain = exec_cypher_on(&view, "CALL custom.pad(2) YIELD out RETURN count(*) AS c");
        assert_eq!(plain.rows, vec![vec![Value::Int(0)]]);
        let optional = exec_cypher_on(
            &view,
            "OPTIONAL CALL custom.pad(2) YIELD out RETURN count(*) AS c",
        );
        assert_eq!(optional.rows, vec![vec![Value::Int(1)]]);
    }

    /// The void classification is the canonical one (`is_void_procedure`,
    /// the same helper the facade's compile-time validator reads): a
    /// registered procedure that declares no yields is void to the
    /// dispatcher too, stored rows or not — it binds its arguments and
    /// produces no rows, so the driving table answers unchanged instead of
    /// the dispatcher-only spelling that composed one bare clone per
    /// matched stored row.
    #[test]
    fn a_no_yield_registered_procedure_is_void_like_the_facade_classifies_it() {
        let graph = Graph::default();
        graph.register_procedure(Procedure {
            name: "custom.pass".to_string(),
            arg_names: vec!["x".to_string()],
            arg_types: vec!["ANY".to_string()],
            yield_names: Vec::new(),
            rows: vec![
                BTreeMap::from([("x".to_string(), Value::Int(1))]),
                BTreeMap::from([("x".to_string(), Value::Int(1))]),
            ],
        });
        let view = ExecGraph::read(&graph);
        // Two stored rows match x=1 — the old dispatcher-only spelling
        // composed both (count 2); the facade's classification answers the
        // one driving row (count 1).
        let matched = exec_cypher_on(&view, "CALL custom.pass(1) RETURN count(*) AS c");
        assert_eq!(matched.rows, vec![vec![Value::Int(1)]]);
    }
}
