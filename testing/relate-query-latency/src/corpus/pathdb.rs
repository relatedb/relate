//! The pathdb lane's own policy (ADR-0479): open the graph-benchmark-ldbc store, hold the
//! seed table, and decide each entry's run-vs-plan from it — everything
//! this lane constructs and routes lives in its lane module (the seed
//! table included, `mod seeds` below), the same shape as the fraud
//! lane's, so the runner's arms stay one-line delegations and the runner
//! itself stays lane-neutral.
//!
//! This module imports the corpus tree's shared execution vocabulary
//! (`graph`'s [`GraphHandle`], [`Routed`], [`Target`]) and its own seed
//! table; it never imports the runner or the fraud lane.

use std::collections::BTreeMap;

use relate::Value;

use super::files::refuse_stale_ids;
use super::graph::{CorpusLane, GraphHandle, Routed, Target};
use super::report;
use super::{Classification, CorpusQuery, SeedPolicy, Workload};
use crate::suite::GraphOptions;
use crate::Result;

mod seeds;

/// The pathdb lane: the graph-benchmark-ldbc store (boxed — a store's database and
/// connection dwarf every other lane's state), the seed table consulted
/// at every route, the seed diffs the median policy re-resolved
/// differently than the params file records, and the store-origin
/// provenance line.
pub(super) struct PathdbLane {
    graph: Box<GraphHandle>,
    table: seeds::SeedTable,
    seed_diffs: Vec<report::SeedDiff>,
    line: String,
}

/// The seed table's contract with the workload's query files, checked
/// in both directions at build: every seeded corpus entry's derived
/// seed holds the format's invariants ([`seeds::SeedTable::seed_of`]),
/// and every params id names a query file — stale entry data cannot
/// ride along unvalidated (the shared stale-id refusal; a query file
/// with no entry at this scale factor plans, never runs — this lane's
/// own subset direction).
fn validate(table: &seeds::SeedTable, queries: &[CorpusQuery]) -> Result<()> {
    for q in queries {
        table
            .seed_of(&q.id)
            .map_err(|e| format!("{} (SF{}): {e}", q.id, table.sf().as_str()))?;
    }
    refuse_stale_ids(
        table.queries.keys().map(String::as_str),
        queries,
        "a params entry",
    )
}

impl PathdbLane {
    /// Build the lane: load this scale factor's seed table, validate the
    /// corpus against it, open the graph-benchmark-ldbc store, and — under the median
    /// policy — re-resolve every seed key, recording the diffs. A query
    /// absent from the params file is not run at this scale factor (the
    /// SF1 file holds the spike's 26-query subset); its classification
    /// still gates the plan check.
    pub(super) fn new(
        opts: &GraphOptions,
        seed_policy: SeedPolicy,
        queries: &[CorpusQuery],
    ) -> Result<Self> {
        let table = seeds::SeedTable::load(&Workload::Pathdb.dir(), opts.sf)?;
        validate(&table, queries)?;
        let (mut graph, origin) = GraphHandle::open_graph_benchmark_ldbc(opts)?;
        let line = report::graph_benchmark_ldbc_store_line(origin);
        let mut seed_diffs = Vec::new();
        if seed_policy == SeedPolicy::Median {
            seeds::reseed(&mut graph.conn, &table, &mut seed_diffs)?;
        }
        Ok(Self {
            graph: Box::new(graph),
            table,
            seed_diffs,
            line,
        })
    }
}

impl CorpusLane for PathdbLane {
    /// The report's one-line graph provenance.
    fn graph_line(&self) -> String {
        self.line.clone()
    }

    /// The seed diffs the median policy re-resolved differently than the
    /// params file records — the lane's own report fact (empty under the
    /// recorded policy, which resolves nothing).
    fn seed_diffs(&self) -> &[report::SeedDiff] {
        &self.seed_diffs
    }

    /// The entry's full disposition: an `ok` entry runs only when this
    /// scale factor holds its seed — the typed seeded-ness fact
    /// ([`seeds::SeedTable::seed_of`]) the Run arm builds the entry's
    /// one-key parameter map from; everything else resolves through the
    /// shared non-executing table (a `kept` entry plans even when
    /// seeded — kept entries never execute at any scale factor; a pin
    /// executes to fail with its authored error, which fires before any
    /// parameter is bound, so a pin needs no seed).
    fn route<'a>(&'a mut self, q: &'a CorpusQuery) -> Result<Routed<'a>> {
        let Self { graph, table, .. } = self;
        Ok(match (&q.classification, table.seed_of(&q.id)?) {
            (Classification::Ok, Some(seed)) => Routed::Run(Target {
                conn: &mut graph.conn,
                params: BTreeMap::from([("seed".to_string(), Value::Int(seed))]),
                check: None,
            }),
            (other, _) => Routed::non_executing(other, &mut graph.conn),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{ok_query, pin_query, query};
    use super::super::Classification;
    use super::*;
    use crate::workload::ScaleFactor;

    fn load_table(dir: &tempfile::TempDir) -> seeds::SeedTable {
        seeds::SeedTable::load(dir.path(), ScaleFactor::Sf0_1).unwrap()
    }

    const KEYS: &str = r#"{"_source": {}, "seed_keys": {
        "Person|knows": {"sf0.1": {"id": 9, "degree": 3, "label": "Person"},
                          "sf1": null}}}"#;

    /// The pathdb lane's disposition table, routed from a fabricated
    /// lane state: an `ok` entry runs only when this scale factor holds
    /// its seed (the seed riding on the target as its parameters); a
    /// `kept` entry plans even when seeded; a pin executes to fail.
    #[test]
    fn the_pathdb_route_resolves_the_full_disposition() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute("CREATE (:N {ID: 1})").unwrap();
        let params = r#"{"_source": {}, "queries": {
            "p009": {"seed": 9, "seed_key": "Person|knows"}}}"#;
        let dir = seeds::write_seed_table(params, KEYS);
        let mut lane = PathdbLane {
            graph: Box::new(GraphHandle { _db: db, conn }),
            table: load_table(&dir),
            seed_diffs: Vec::new(),
            line: String::new(),
        };
        // An `ok` entry with a seed at this scale factor runs, the seed
        // as its parameters.
        let seeded = ok_query("p009", "RETURN 1");
        let Routed::Run(target) = lane.route(&seeded).unwrap() else {
            panic!("a seeded ok entry runs");
        };
        assert_eq!(
            target.params.get("seed"),
            Some(&Value::Int(9)),
            "the recorded seed is the entry's parameters"
        );
        drop(target);
        // An `ok` entry without one plans (never runs).
        assert!(matches!(
            lane.route(&ok_query("p012", "RETURN 1")).unwrap(),
            Routed::Plan(_)
        ));
        // A `kept` entry plans even when its seed is present in the
        // table — kept entries never execute at any scale factor.
        assert!(matches!(
            lane.route(&query("p009", Classification::Kept, "RETURN 1"))
                .unwrap(),
            Routed::Plan(_)
        ));
        // A pin executes, to fail with its authored error.
        assert!(matches!(
            lane.route(&pin_query("p009", "RETURN 1")).unwrap(),
            Routed::Pin(_, "UnsafeRepeatableElements")
        ));
    }

    /// A params id naming no query file is stale entry data riding
    /// along unvalidated — refused at build, before anything opens.
    #[test]
    fn a_params_entry_naming_no_query_file_is_refused() {
        let params = r#"{"_source": {}, "queries": {
            "p009": {"seed": 9, "seed_key": "Person|knows"},
            "p999": {"seed": 9, "seed_key": "Person|knows"}}}"#;
        let dir = seeds::write_seed_table(params, KEYS);
        let table = load_table(&dir);
        let err = validate(&table, &[ok_query("p009", "RETURN 1")]).unwrap_err();
        assert!(
            err.contains("p999: a params entry with no query file"),
            "{err}"
        );
        // Every params id backed by a query file: the check passes.
        assert!(validate(
            &table,
            &[ok_query("p009", "RETURN 1"), ok_query("p999", "RETURN 1")]
        )
        .is_ok());
    }

    /// A seeded corpus entry whose derived seed breaks the format's
    /// invariants is refused at build, naming the entry and its scale
    /// factor.
    #[test]
    fn a_seeded_entry_with_a_broken_derived_seed_is_refused() {
        let params = r#"{"_source": {}, "queries": {
            "p009": {"seed": 8, "seed_key": "Person|knows"}}}"#;
        let dir = seeds::write_seed_table(params, KEYS);
        let table = load_table(&dir);
        let err = validate(&table, &[ok_query("p009", "RETURN 1")]).unwrap_err();
        assert!(
            err.contains("p009 (SF0.1)") && err.contains("disagrees with its key"),
            "{err}"
        );
    }
}
