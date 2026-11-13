//! The snb-bi lane's own policy (ADR-0479, LABS-646 follow-up): open the
//! graph-benchmark-ldbc store, load the workload's recorded substitution parameters, and
//! decide each entry's run-vs-plan from them — the same shape as the
//! pathdb lane's, so the runner's arms stay one-line delegations. Where
//! the two differ is the parameter policy: PathDB anchors every query on
//! one substituted seed node (the degree-median policy over a seed-key
//! fact table), while SNB BI's upstream parameters are literal values
//! (dates, tag names, windows) whose graph-benchmark-ldbc-store analogues the workload
//! records per scale factor in `params-sf<SF>.json` — each value ONE
//! Cypher expression the lane evaluates once and binds natively (the
//! engine's parameter binding, ADR-0479; ADR-0247's textual substitution
//! stays graph-benchmark-ldbc-only).
//!
//! This module imports the corpus tree's shared execution vocabulary
//! (`graph`'s [`GraphHandle`], [`Routed`], [`Target`], the store-origin
//! line) and never imports the runner or the other lanes.

use std::collections::BTreeMap;

use relate::Value;

use super::files::{validate_params, ParamsTable};
use super::graph::evaluate_table;
use super::graph::{CorpusLane, GraphHandle, Routed, Target};
use super::report::graph_benchmark_ldbc_store_line;
use super::{Classification, CorpusQuery, Workload};
use crate::suite::GraphOptions;
use crate::Result;

/// The snb-bi lane: the graph-benchmark-ldbc store (boxed — a store's database and
/// connection dwarf every other lane's state) and the evaluated
/// parameters each entry runs with.
pub(super) struct SnbBiLane {
    graph: Box<GraphHandle>,
    params: BTreeMap<String, BTreeMap<String, Value>>,
    line: String,
}

impl SnbBiLane {
    /// Build the lane: load this scale factor's parameter table, validate
    /// the workload against it, open the graph-benchmark-ldbc store, and evaluate every
    /// recorded expression once — a malformed expression fails the run
    /// here, before any entry executes.
    pub(super) fn new(opts: &GraphOptions, queries: &[CorpusQuery]) -> Result<Self> {
        let table = ParamsTable::load(&Workload::SnbBi.dir(), opts.sf)?;
        validate_params(&table, queries)?;
        let (mut graph, origin) = GraphHandle::open_graph_benchmark_ldbc(opts)?;
        let params = evaluate_table(&mut graph.conn, &table)?;
        Ok(Self {
            graph: Box::new(graph),
            params,
            line: graph_benchmark_ldbc_store_line(origin),
        })
    }
}

impl CorpusLane for SnbBiLane {
    /// The report's one-line graph provenance.
    fn graph_line(&self) -> String {
        self.line.clone()
    }

    /// The entry's full disposition: an `ok` entry runs only when this
    /// scale factor holds its parameters (the lane's typed
    /// params-present fact); everything else resolves through the shared
    /// non-executing table (a `kept` entry plans even when parameterized
    /// — kept entries never execute at any scale factor).
    fn route<'a>(&'a mut self, q: &'a CorpusQuery) -> Result<Routed<'a>> {
        let Self { graph, params, .. } = self;
        Ok(match (&q.classification, params.get(&q.id)) {
            (Classification::Ok, Some(params)) => Routed::Run(Target {
                conn: &mut graph.conn,
                params: params.clone(),
                check: None,
            }),
            (other, _) => Routed::non_executing(other, &mut graph.conn),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::workload::ScaleFactor;

    use super::super::fixtures::ok_query;
    use super::super::fixtures::query;
    use super::super::graph::evaluate;
    use super::*;

    /// A temp workload dir holding one scale factor's params file, named
    /// by the loader's own convention — never re-spelled here.
    fn write_params(params: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            crate::corpus::files::params_file(dir.path(), ScaleFactor::Sf0_1),
            params,
        )
        .unwrap();
        dir
    }

    const PARAMS: &str = r#"{"_source": {"what": "provenance"}, "bi-01": {
        "datetime": "datetime(\"2011-12-01\")"},
        "bi-02": {"tagClass": "'MusicalArtist'", "lengthThreshold": "20",
                  "languages": "['ar', 'hu']"}}"#;

    #[test]
    fn params_tables_parse_their_entries_and_skip_the_source_block() {
        let dir = write_params(PARAMS);
        let table = ParamsTable::load(dir.path(), ScaleFactor::Sf0_1).unwrap();
        assert_eq!(table.queries.len(), 2, "_source is not an entry");
        assert_eq!(table.queries["bi-02"]["tagClass"], "'MusicalArtist'");
    }

    #[test]
    fn a_parameter_that_is_not_an_expression_string_is_refused() {
        // A JSON number is not an expression string — the file's one rule
        // is refused, naming the query and the parameter.
        let dir = write_params(r#"{"bi-12": {"lengthThreshold": 20}}"#);
        let err = ParamsTable::load(dir.path(), ScaleFactor::Sf0_1).unwrap_err();
        assert!(
            err.contains("bi-12's parameter lengthThreshold is not a Cypher expression string"),
            "{err}"
        );
        // An entry that is not an object of parameters at all.
        let dir = write_params(r#"{"bi-12": 20}"#);
        let err = ParamsTable::load(dir.path(), ScaleFactor::Sf0_1).unwrap_err();
        assert!(
            err.contains("bi-12's entry is not an object of parameters"),
            "{err}"
        );
    }

    #[test]
    fn a_params_entry_naming_no_query_file_is_refused() {
        let dir = write_params(PARAMS);
        let table = ParamsTable::load(dir.path(), ScaleFactor::Sf0_1).unwrap();
        let err = validate_params(&table, &[ok_query("bi-01", "RETURN 1")]).unwrap_err();
        assert!(
            err.contains("bi-02: a params entry with no query file"),
            "{err}"
        );
        assert!(validate_params(
            &table,
            &[ok_query("bi-01", "RETURN 1"), ok_query("bi-02", "RETURN 1")]
        )
        .is_ok());
    }

    /// Every parameter kind the workload records evaluates to the value
    /// the statement binds — a temporal, a string, an integer, a list —
    /// and a malformed expression fails loudly, naming query, parameter
    /// and expression.
    #[test]
    fn parameter_expressions_evaluate_to_bound_values() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        assert!(matches!(
            evaluate(&mut conn, "bi-01", "datetime", "datetime(\"2011-12-01\")").unwrap(),
            Value::Temporal(_)
        ));
        assert_eq!(
            evaluate(&mut conn, "bi-02", "tagClass", "'MusicalArtist'").unwrap(),
            Value::String("MusicalArtist".into())
        );
        assert_eq!(
            evaluate(&mut conn, "bi-12", "lengthThreshold", "20").unwrap(),
            Value::Int(20)
        );
        assert!(matches!(
            evaluate(&mut conn, "bi-12", "languages", "['ar', 'hu']").unwrap(),
            Value::List(_)
        ));
        let err = evaluate(&mut conn, "bi-02", "tagClass", "'unterminated").unwrap_err();
        assert!(
            err.contains("bi-02's parameter tagClass = \"'unterminated\""),
            "{err}"
        );
    }

    /// The lane's disposition table, routed from fabricated lane state:
    /// an `ok` entry runs only when this scale factor holds its
    /// parameters (the evaluated parameters riding on the target as its
    /// own); an `ok` entry without them plans, never runs; a `kept`
    /// entry plans even when parameterized.
    #[test]
    fn the_snb_bi_route_resolves_the_full_disposition() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        let params = BTreeMap::from([(
            "bi-01".to_string(),
            BTreeMap::from([(
                "datetime".to_string(),
                evaluate(&mut conn, "bi-01", "datetime", "datetime(\"2011-12-01\")").unwrap(),
            )]),
        )]);
        let mut lane = SnbBiLane {
            graph: Box::new(GraphHandle { _db: db, conn }),
            params,
            line: String::new(),
        };
        // A parameterized `ok` entry runs, its parameters on the target.
        let parameterized = ok_query("bi-01", "RETURN $datetime AS d");
        let Routed::Run(target) = lane.route(&parameterized).unwrap() else {
            panic!("a parameterized ok entry runs");
        };
        assert!(matches!(
            target.params.get("datetime"),
            Some(Value::Temporal(_))
        ));
        drop(target);
        // An `ok` entry with no entry at this scale factor plans.
        assert!(matches!(
            lane.route(&ok_query("bi-16", "RETURN 1")).unwrap(),
            Routed::Plan(_)
        ));
        // A `kept` entry plans even when parameterized — kept entries
        // never execute at any scale factor.
        assert!(matches!(
            lane.route(&query("bi-01", Classification::Kept, "RETURN 1"))
                .unwrap(),
            Routed::Plan(_)
        ));
    }

    /// The committed params file parses, every entry names a committed
    /// query file, and every expression evaluates — the workload's
    /// self-check, over the same doors the lane builds through.
    #[test]
    fn the_committed_params_file_evaluates_for_every_query() {
        let queries = super::super::workload_queries(Workload::SnbBi).unwrap();
        let table = ParamsTable::load(&Workload::SnbBi.dir(), ScaleFactor::Sf0_1).unwrap();
        assert!(validate_params(&table, &queries).is_ok());
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        evaluate_table(&mut conn, &table).unwrap_or_else(|e| panic!("{e}"));
    }
}
