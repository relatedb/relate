//! The corpus tree's shared execution types: the open-graph handle every
//! lane executes against, the answer check a lane attaches to an entry,
//! the execution target a run runs, the disposition a lane resolves
//! each entry to, and the lane interface itself ([`CorpusLane`]) every
//! workload's lane module implements — lane-neutral state and vocabulary
//! only (a lane's check-specific data, like the fraud lane's
//! planted-cycle ids, lives beside the handle, in the lane). Every lane
//! and the runner import from here; no lane imports another.

use std::collections::BTreeMap;
use std::path::Path;

use relate::{Connection, Database, OpenOptions, QueryResult, Value};

use super::files::{row_of, Oracle, ParamsTable};
use super::report::{FraudReport, SeedDiff};
use super::{Classification, CorpusQuery};

use crate::answers;
use crate::report::StoreOrigin;
use crate::suite::{connect, open_graph, GraphOptions};
use crate::Result;

/// One open graph a corpus run executes against: the database kept
/// alive for as long as its connection — lane-neutral state only; a
/// lane's check-specific data (the fraud lane's planted-cycle ids)
/// lives beside the handle, in the lane.
pub(super) struct GraphHandle {
    pub(super) _db: Database,
    pub(super) conn: Connection,
}

impl GraphHandle {
    /// The connect-and-wrap half every constructor runs — the connect
    /// step and its error text owned once by the suite layer's
    /// [`connect`] (which every suite-level caller routes through too);
    /// the open step differs per constructor and stays here.
    fn from_db(step: &str, db: Database) -> Result<Self> {
        let conn = connect(step, &db)?;
        Ok(Self { _db: db, conn })
    }

    /// The open-connect wrap every `:memory:` constructor runs — the one
    /// home of the open step and its error text; the two constructors
    /// differ only in their [`OpenOptions`].
    fn open_memory(step: &str, opts: OpenOptions) -> Result<Self> {
        let db = Database::open_with(":memory:", opts)
            .map_err(|e| format!("{step}: opening the store: {e}"))?;
        Self::from_db(step, db)
    }

    /// A graph opened in `:memory:`.
    pub(super) fn memory(step: &str) -> Result<Self> {
        Self::open_memory(step, OpenOptions::default())
    }

    /// A graph opened in `:memory:` with local LOAD CSV enabled and
    /// confined to `csv_root` (ADR-0081) — the ukrailway lane's imported
    /// graph, whose LOAD CSV statements read the fetched data set.
    pub(super) fn memory_with_csv_root(step: &str, csv_root: &Path) -> Result<Self> {
        Self::open_memory(
            step,
            OpenOptions {
                load_csv_dir: Some(csv_root.to_path_buf()),
                ..OpenOptions::default()
            },
        )
    }

    /// A graph opened from the graph-benchmark-ldbc-store options — the
    /// open-connect wrap every graph-benchmark-ldbc-store lane runs
    /// (pathdb, snb-bi, agg-stress, equi-join), the connect step owned by the
    /// suite layer's [`connect`]. The store origin rides along for the
    /// lane's provenance line
    /// ([`super::report::graph_benchmark_ldbc_store_line`]).
    pub(super) fn open_graph_benchmark_ldbc(opts: &GraphOptions) -> Result<(Self, StoreOrigin)> {
        let (db, origin) = open_graph(opts)?;
        Ok((Self::from_db("the graph-benchmark-ldbc store", db)?, origin))
    }
}

/// An executed entry's answer check as ONE value the runner applies
/// uniformly: the difference when the answer does not match, `None`
/// when it does. The fraud lane's value holds its own check and
/// planted-cycle data, the ukrailway, agg-stress and equi-join lanes' their
/// verified-rows checks; the pathdb and snb-bi lanes attach none
/// (pathdb's oracle is the row count, snb-bi's entries run with no
/// answer oracle).
pub(super) type AnswerCheck<'a> = Box<dyn Fn(&QueryResult) -> Option<String> + 'a>;

/// An entry's execution target: the connection it runs on, the
/// parameters it runs with (the lane resolves them at route — the
/// pathdb lane's seed and the snb-bi lane's recorded table; the
/// fraud, ukrailway, agg-stress and equi-join lanes' literal-anchored entries
/// carry none),
/// and, when the lane answer-checks executed entries, its
/// [`AnswerCheck`].
pub(super) struct Target<'a> {
    pub(super) conn: &'a mut Connection,
    pub(super) params: BTreeMap<String, Value>,
    pub(super) check: Option<AnswerCheck<'a>>,
}

/// An entry's full disposition, resolved once by the lane's route from
/// the classification it already receives.
pub(super) enum Routed<'a> {
    /// Run it: `ok` entries execute and are answer-checked where the
    /// lane carries a check.
    Run(Target<'a>),
    /// Plan only: an `ok` entry its lane does not run at this scale
    /// factor (a lane's params file may hold only a subset of the
    /// queries — an entry without one there is planned, never run), and
    /// every `kept` entry, which never executes at any scale factor.
    Plan(&'a mut Connection),
    /// Execute and require the named authored error (it fires before
    /// any parameter is bound, so a pin needs no seed).
    Pin(&'a mut Connection, &'a str),
}

impl<'a> Routed<'a> {
    /// The non-executing dispositions, stated ONCE for every lane: an
    /// `ok` entry its lane does not run plans, a `kept` entry plans, an
    /// `error-pin` entry executes to fail with its authored error. Each
    /// lane's route decides only its executable case and resolves the
    /// rest through this — a third lane inherits the table instead of
    /// copying its rows.
    pub(super) fn non_executing(
        classification: &'a Classification,
        conn: &'a mut Connection,
    ) -> Routed<'a> {
        match classification {
            Classification::Ok | Classification::Kept => Routed::Plan(conn),
            Classification::ErrorPin { error } => Routed::Pin(conn, error),
        }
    }

    /// The rows-oracle lanes' full disposition, stated ONCE for both
    /// (ukrailway, agg-stress, equi-join): an `ok` entry runs
    /// answer-checked against its oracle entry (resolved through the one
    /// lookup, [`row_of`]), everything else resolving through the shared
    /// non-executing table. The constant-inline lanes' queries genuinely
    /// carry no parameters and bind an empty map. An `ok` id with no
    /// oracle entry fails loudly (the fraud lane's rule). Each lane's
    /// route is its own oracle path derivation plus this.
    pub(super) fn rows_checked(
        conn: &'a mut Connection,
        params: Option<BTreeMap<String, Value>>,
        oracle: &'a Oracle,
        path: &Path,
        q: &'a CorpusQuery,
    ) -> Result<Routed<'a>> {
        Ok(match &q.classification {
            Classification::Ok => {
                let entry = row_of(oracle, &q.id, path)?;
                Routed::Run(Target {
                    conn,
                    params: params.unwrap_or_default(),
                    check: Some(Box::new(move |result| {
                        answers::diff(result, &entry.expected)
                    })),
                })
            }
            Classification::Kept | Classification::ErrorPin { .. } => {
                Routed::non_executing(&q.classification, conn)
            }
        })
    }
}

/// Evaluate ONE parameter expression into the value the statement binds:
/// `RETURN <expr>` over the open store's connection, once per parameter
/// at build, and the result must be exactly one row of one value — an
/// expression that answers anything else is a malformed entry, reported
/// with the query, the parameter and the expression.
pub(super) fn evaluate(conn: &mut Connection, id: &str, name: &str, expr: &str) -> Result<Value> {
    let r = conn
        .query(&format!("RETURN {expr} AS value"))
        .map_err(|e| format!("{id}'s parameter {name} = {expr:?}: {e}"))?;
    match &r.rows[..] {
        [row] if row.len() == 1 => Ok(row[0].clone()),
        [row] => Err(format!(
            "{id}'s parameter {name} = {expr:?} answered {} columns, not one",
            row.len()
        )),
        rows => Err(format!(
            "{id}'s parameter {name} = {expr:?} answered {} rows, not one",
            rows.len()
        )),
    }
}

/// Evaluate every recorded parameter expression once, over `conn` — the
/// one pass the lanes' build and the workloads' committed-file
/// self-checks both run, so the tests exercise the production path: a
/// malformed expression fails here, before any entry executes.
pub(super) fn evaluate_table(
    conn: &mut Connection,
    table: &ParamsTable,
) -> Result<BTreeMap<String, BTreeMap<String, Value>>> {
    let mut params = BTreeMap::new();
    for (id, entries) in &table.queries {
        let mut evaluated = BTreeMap::new();
        for (name, expr) in entries {
            evaluated.insert(name.clone(), evaluate(conn, id, name, expr)?);
        }
        params.insert(id.clone(), evaluated);
    }
    Ok(params)
}

/// A corpus lane: the one interface every workload's lane module
/// implements (`pathdb::PathdbLane`, `fraud::FraudGraphs`,
/// `snbbi::SnbBiLane`, `ukrailway::UkrailwayLane`, and the shared
/// `oracle_store::RowsOracleStoreLane` the rows-oracle workloads
/// build), so the runner holds
/// ONE lane — built once, routed per entry, read once for the report —
/// with no lane enum to thread through a match per fact. Construction
/// stays each lane's own `new` (a lane's setup reads its own knobs, so
/// it cannot be a trait method); everything after construction is here.
pub(super) trait CorpusLane {
    /// Resolve the entry's full disposition ONCE — each lane's own route
    /// decides its executable case (with the parameters it runs with),
    /// everything non-executing resolving through the shared
    /// [`Routed::non_executing`] table.
    fn route<'a>(&'a mut self, q: &'a CorpusQuery) -> Result<Routed<'a>>;

    /// The report's one-line graph provenance.
    fn graph_line(&self) -> String;

    /// The report's fraud block, when the fraud lane ran — the one lane
    /// carrying generator knobs; every other lane reports none.
    fn fraud_report(&self) -> Option<FraudReport> {
        None
    }

    /// The seed diffs the lane's seed policy re-resolved differently
    /// than its params file records — the one lane that runs a seed
    /// policy (pathdb); every other lane carries none.
    fn seed_diffs(&self) -> &[SeedDiff] {
        &[]
    }
}

#[cfg(test)]
mod tests {
    use super::super::files::OracleEntry;
    use super::super::fixtures::{ok_query, query};
    use super::*;

    /// The rows-oracle route, the one disposition every rows-oracle lane
    /// (ukrailway, agg-stress, equi-join) resolves through: an `ok` entry runs
    /// with its oracle entry as the answer check and the parameters its
    /// lane resolved (`None` here — this test's lane binds none), a kept
    /// entry plans, and an `ok` id with no oracle row is refused naming
    /// the id and the oracle file — never planned as an executable entry
    /// with nothing to answer-check it.
    #[test]
    fn rows_oracle_routes_run_check_plan_and_refuse_a_missing_row() {
        let mut handle = GraphHandle::memory("the rows-oracle route test's graph").unwrap();
        let oracle = Oracle::from([(
            "q1".to_string(),
            OracleEntry {
                expected: crate::answers::Expected {
                    rows: vec![],
                    order_sensitive: false,
                },
                query_sha256: "aa".into(),
            },
        )]);
        let path = Path::new("oracle.json");
        let q1 = ok_query("q1", "RETURN 1");
        let Routed::Run(target) =
            Routed::rows_checked(&mut handle.conn, None, &oracle, path, &q1).unwrap()
        else {
            panic!("an ok entry with an oracle row runs");
        };
        assert!(target.params.is_empty(), "the queries carry no parameters");
        assert!(target.check.is_some(), "q1: answer-checked");
        drop(target);
        // A kept entry plans, never runs.
        assert!(matches!(
            Routed::rows_checked(
                &mut handle.conn,
                None,
                &oracle,
                path,
                &query("q1", Classification::Kept, "RETURN 1")
            )
            .unwrap(),
            Routed::Plan(_)
        ));
        // `Routed` carries a connection, so it formats nothing: the
        // refusal is read out of the Result by arm, as loudly as
        // `unwrap_err`.
        let err = match Routed::rows_checked(
            &mut handle.conn,
            Some(BTreeMap::new()),
            &Oracle::new(),
            path,
            &ok_query("q9-not-in-the-oracle", "RETURN 1"),
        ) {
            Err(err) => err,
            Ok(_) => panic!("an ok entry with no oracle row is refused, never routed"),
        };
        assert!(
            err.contains("no oracle row in") && err.ends_with("oracle.json"),
            "the miss names the id and the oracle file: {err}"
        );
    }
}
