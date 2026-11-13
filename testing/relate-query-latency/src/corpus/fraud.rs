//! The fraud workload's oracle, answer checks and graphs (ADR-0479):
//! the `oracle.json` format, the planted-ring presence check over the
//! generated fraud graph, the rows check over the sample fixture, the
//! fixture loader — and [`FraudGraphs`], the lane that opens each graph
//! its oracle asks for and routes every entry onto it.
//!
//! The workload is IN-HOUSE (see its README): f01/f02's oracle is the
//! generator's planted directed cycle — an engine-independent oracle,
//! since the generator knows the ring it planted before any query runs —
//! and f06's is the designed fixture's exact answer, compared with the
//! graph-benchmark-ldbc lane's no-coercion rules.
//!
//! This module imports the corpus tree's shared execution vocabulary
//! (`graph`'s [`Target`], [`Routed`], the graph handles); it never
//! imports the runner — `runner → fraud` is the one edge between the
//! submodules.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::path::PathBuf;

use relate::Value;
use relate_datasets::fraud::{generate, FraudConfig};
use relate_datasets::load::load_fraud;

use super::files::{parse_rows_check, read_query_entries, refuse_stale_ids, row_of};
use super::graph::{CorpusLane, GraphHandle, Routed, Target};
use super::report::FraudReport;
use super::{Classification, CorpusQuery, Workload};
use crate::answers::{self, Expected};
use crate::{read, split_header, Result};

/// The fraud workload's oracle (`workloads/fraud-qpp/oracle.json`): per
/// query id, the check a run applies to the answer — and, with it, the
/// graph the entry runs on (the planted-ring check runs on the generated
/// fraud graph, the rows check on the sample fixture).
#[derive(Debug, Clone)]
pub enum FraudCheck {
    /// The planted directed cycle of the generator (`cycle-0` …) must
    /// appear among the returned rings — an engine-independent oracle
    /// (the generator knows the ring it planted). Runs on the generated
    /// graph.
    PlantedRing,
    /// Exact rows, compared with the graph-benchmark-ldbc lane's no-coercion rules. Runs
    /// on the sample fixture.
    Rows(Expected),
    /// No answer check: the `kept` entries, whose oracle is prose notes.
    None,
}

/// The fraud workload's oracle file, `workloads/fraud-qpp/oracle.json` —
/// [`Workload::oracle_path`]'s derivation, the one home of the
/// convention, unwrapped once here: this workload carries one. Shared by
/// the parser and the row lookup's miss text.
fn oracle_path() -> PathBuf {
    Workload::FraudQpp
        .oracle_path(crate::workload::ScaleFactor::Sf0_1)
        .expect("the fraud workload carries a rows oracle")
}

/// Parse the fraud workload's oracle (`workloads/fraud-qpp/oracle.json`).
/// Every executable (`ok`) entry must carry a machine-readable check;
/// `kept` entries carry none.
pub fn fraud_oracle() -> Result<BTreeMap<String, FraudCheck>> {
    let path = oracle_path();
    let mut out = BTreeMap::new();
    for (id, v) in read_query_entries(&path)? {
        let check = v
            .get("check")
            .and_then(|c| c.as_str())
            .ok_or_else(|| format!("{}: {id} has no check", path.display()))?;
        let check = match check {
            "planted-ring" => FraudCheck::PlantedRing,
            "rows" => FraudCheck::Rows(parse_rows_check(&path, &id, &v)?),
            "none" => FraudCheck::None,
            other => {
                return Err(format!(
                    "{}: {id} check {other:?} is not planted-ring, rows or none",
                    path.display()
                ))
            }
        };
        out.insert(id, check);
    }
    Ok(out)
}

/// The answer of an executed entry against its check, as the difference
/// when it does not match — the one place a [`FraudCheck`] meets an
/// executed result (`kept` entries never execute, so `none` never
/// diffs; `planted` is the planted cycle of the check's own graph).
pub(super) fn answer_diff(
    check: &FraudCheck,
    planted: &[String],
    result: &relate::QueryResult,
) -> Option<String> {
    match check {
        FraudCheck::Rows(expected) => answers::diff(result, expected),
        FraudCheck::PlantedRing => planted_ring_diff(planted, result),
        FraudCheck::None => None,
    }
}

/// The planted-ring presence check: some row must be `(account, ring)`
/// with the account the cycle's first id and the ring the cycle's ids in
/// order, closed — the generator planted that ring, so the engine must
/// find it (accidental noise rings may appear beside it; the planted one
/// may not be missing). A row of the wrong shape, or a ring member that
/// is not a string id, is a shape difference reported immediately, never
/// coerced or skipped.
fn planted_ring_diff(planted: &[String], result: &relate::QueryResult) -> Option<String> {
    if planted.is_empty() {
        return Some("the planted cycle is empty: the generator planted no ring".into());
    }
    let wanted: Vec<&str> = planted
        .iter()
        .chain(std::iter::once(&planted[0]))
        .map(|s| &**s)
        .collect();
    for row in &result.rows {
        let (account, ring) = match &row[..] {
            [Value::String(account), Value::List(ring)] => (account, ring),
            _ => {
                return Some(format!(
                    "a planted-ring row is not (account, ring of ids): {row:?}"
                ))
            }
        };
        let got: Option<Vec<&str>> = ring
            .iter()
            .map(|v| match v {
                Value::String(s) => Some(&**s),
                _ => None,
            })
            .collect();
        let got = match got {
            Some(got) => got,
            None => {
                return Some(format!(
                    "a planted-ring row's ring is not a list of ids: {row:?}"
                ))
            }
        };
        if &**account == planted[0].as_str() && got == wanted {
            return None;
        }
    }
    Some(format!(
        "the planted cycle {wanted:?} does not appear among the returned rings"
    ))
}

/// Which fraud graph an entry's check runs on: one graph kind per check
/// that needs one, so the lane's graphs and its routing are the same
/// table. `none` checks (the kept entries, planned never executed) plan
/// on the generated graph — the lane's principal graph.
fn graph_kind(check: &FraudCheck) -> FraudGraphKind {
    match check {
        FraudCheck::PlantedRing | FraudCheck::None => FraudGraphKind::Generated,
        FraudCheck::Rows(_) => FraudGraphKind::Fixture,
    }
}

/// The fraud lane's open graph kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FraudGraphKind {
    /// The generated fraud graph, with the planted cycle.
    Generated,
    /// The f06 sample fixture.
    Fixture,
}

impl FraudGraphKind {
    /// The graph line's name for the report.
    fn line(self) -> &'static str {
        match self {
            Self::Generated => "the generated graph",
            Self::Fixture => "the f06 fixture",
        }
    }
}

/// One opened fraud graph: the shared, lane-neutral handle plus the
/// planted-cycle ids the planted-ring check compares with (the
/// generator's graph carries them; the fixture plants none).
struct FraudGraph {
    handle: GraphHandle,
    planted: Vec<String>,
}

/// The fraud lane: the oracle that routes entries, the generator knobs
/// (a graph opens on first use), and the open graphs. Routing can never
/// miss its graph — the graph a check asks for opens at the moment the
/// check first routes to it.
pub(super) struct FraudGraphs {
    oracle: BTreeMap<String, FraudCheck>,
    config: FraudConfig,
    graphs: BTreeMap<FraudGraphKind, FraudGraph>,
    /// The oracle file's path, derived once at build and stored — the
    /// row lookup names the file in its miss text, never re-derives the
    /// path per entry (the same discipline `RowsOracleStoreLane` and
    /// `UkrailwayLane` follow).
    oracle_path: PathBuf,
}

impl FraudGraphs {
    /// Parse the oracle, validate it against the workload's entries —
    /// the lane owns the oracle's format contract, totally: every
    /// workload entry has an oracle row (resolved through the one
    /// lookup, [`row_of`]) and no oracle row is without a query file,
    /// and every (classification, check) pair is either valid or
    /// refused — an executable (`ok`) entry carries a real check, a
    /// `kept` entry carries `none`, a pin entry carries `none` (its
    /// authored failure is its check) — and hold the generator knobs;
    /// the graphs open on first use.
    pub(super) fn new(config: FraudConfig, queries: &[CorpusQuery]) -> Result<Self> {
        let oracle = fraud_oracle()?;
        let path = oracle_path();
        for q in queries {
            let check = row_of(&oracle, &q.id, &path)?;
            match (&q.classification, check) {
                (Classification::Ok, FraudCheck::None) => {
                    return Err(format!(
                        "{}: an executable entry with no machine-readable check",
                        q.id
                    ));
                }
                (Classification::Kept, FraudCheck::PlantedRing | FraudCheck::Rows(_)) => {
                    return Err(format!(
                        "{}: a kept entry carries no check (it never executes)",
                        q.id
                    ));
                }
                (
                    Classification::ErrorPin { .. },
                    FraudCheck::PlantedRing | FraudCheck::Rows(_),
                ) => {
                    return Err(format!(
                        "{}: a pin entry's authored failure is its check; its oracle row \
                         carries none",
                        q.id
                    ));
                }
                // The valid pairs: an executable entry's real check, and
                // a non-executing entry's `none`.
                (Classification::Ok, FraudCheck::PlantedRing | FraudCheck::Rows(_))
                | (Classification::Kept, FraudCheck::None)
                | (Classification::ErrorPin { .. }, FraudCheck::None) => {}
            }
        }
        // No oracle row is without a query file — the shared stale-id
        // refusal, over this lane's own table.
        refuse_stale_ids(
            oracle.keys().map(String::as_str),
            queries,
            "an oracle entry",
        )?;
        Ok(Self {
            oracle,
            config,
            graphs: BTreeMap::new(),
            oracle_path: path,
        })
    }
}

impl CorpusLane for FraudGraphs {
    /// The entry's full disposition: an executable entry's target — its
    /// check as the runner's one answer-check value, the connection the
    /// check runs on (opened here on first use), and the planted cycle
    /// ids the planted-ring check compares with — or the non-executing
    /// disposition, on the lane's principal graph, resolved through the
    /// shared [`Routed::non_executing`] table. An id absent from the
    /// oracle cannot reach here (`new` validated the workload against
    /// the oracle), but the lookup still fails loudly rather than
    /// fabricating a target.
    fn route<'a>(&'a mut self, q: &'a CorpusQuery) -> Result<Routed<'a>> {
        let FraudGraphs {
            oracle,
            config,
            graphs,
            oracle_path,
        } = self;
        let check = row_of(oracle, &q.id, oracle_path)?;
        // `new`'s validation locked every kept or pin entry to a `none`
        // check, so the check alone decides the graph — `graph_kind`'s
        // table is the one home of check → graph.
        let kind = graph_kind(check);
        let graph = match graphs.entry(kind) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(match kind {
                FraudGraphKind::Generated => generated_graph(config)?,
                FraudGraphKind::Fixture => fixture_graph()?,
            }),
        };
        let FraudGraph { handle, planted } = graph;
        let GraphHandle { conn, .. } = handle;
        let planted = planted.as_slice();
        Ok(match &q.classification {
            Classification::Ok => Routed::Run(Target {
                conn,
                // Fraud entries anchor on literals — no parameters.
                params: BTreeMap::new(),
                check: Some(Box::new(move |result| answer_diff(check, planted, result))),
            }),
            other => Routed::non_executing(other, conn),
        })
    }

    /// The report's one-line graph provenance: the graphs the run
    /// opened, joined.
    fn graph_line(&self) -> String {
        let names: Vec<&str> = self.graphs.keys().map(|k| k.line()).collect();
        format!("fraud graphs: {}", names.join(" + "))
    }

    /// The report's fraud block: the generator knobs the lane holds.
    fn fraud_report(&self) -> Option<FraudReport> {
        Some(FraudReport {
            scale: self.config.scale,
            seed: self.config.seed,
            fanout_k: self.config.fanout_k,
            cycle_len: self.config.cycle_len,
        })
    }
}

/// The generated fraud graph: the generator's world loaded into an
/// in-memory store, with the planted cycle's account ids carried beside
/// it (the planted-ring oracle).
fn generated_graph(config: &FraudConfig) -> Result<FraudGraph> {
    let graph = generate(config);
    let mut handle = GraphHandle::memory("the generated fraud graph's store")?;
    load_fraud(&mut handle.conn, &graph)
        .map_err(|e| format!("loading the generated fraud graph: {e}"))?;
    Ok(FraudGraph {
        handle,
        planted: graph.planted.cycle_accounts,
    })
}

/// The f06 sample fixture: the designed ring-and-decoys world of
/// `fixture-f06.cypher` loaded into an in-memory store (nothing
/// planted).
fn fixture_graph() -> Result<FraudGraph> {
    let mut handle = GraphHandle::memory("the f06 fixture's store")?;
    let fixture = read_fixture()?;
    handle
        .conn
        .execute(&fixture)
        .map_err(|e| format!("loading fixture-f06.cypher: {e}"))?;
    Ok(FraudGraph {
        handle,
        planted: Vec::new(),
    })
}

/// `workloads/fraud-qpp/fixture-f06.cypher`, the sample-ring loader: the
/// file's one statement (the header is the fixture's design notes).
fn read_fixture() -> Result<String> {
    let path = Workload::FraudQpp.dir().join("fixture-f06.cypher");
    let (_, body) = split_header(&read(&path)?).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::query;
    use super::super::{workload_queries, Workload};
    use super::*;

    #[test]
    fn the_fraud_oracle_parses_and_covers_every_executable_entry() {
        let oracle = fraud_oracle().unwrap();
        for q in workload_queries(Workload::FraudQpp).unwrap() {
            let check = oracle
                .get(&q.id)
                .unwrap_or_else(|| panic!("{}: no oracle entry", q.id));
            if q.classification == Classification::Ok {
                assert!(
                    !matches!(check, FraudCheck::None),
                    "{}: an executable entry needs a machine-readable check",
                    q.id
                );
            } else {
                assert!(
                    matches!(check, FraudCheck::None),
                    "{}: a kept entry carries no check",
                    q.id
                );
            }
        }
    }

    /// The lane owns the oracle's format contract, totally: every
    /// mismatched (classification, check) pair, an id without an oracle
    /// row, and an oracle row without a query file are malformed
    /// workloads the lane refuses at build — not just failures the
    /// oracle test happens to catch.
    #[test]
    fn malformed_oracles_are_refused_at_build() {
        // f04's oracle entry is the `none` check of a kept entry — an
        // executable entry with it is the first mismatched pair.
        let err = FraudGraphs::new(
            FraudConfig::ci(),
            &[query(
                "f04-bipartite-unbounded",
                Classification::Ok,
                "RETURN 1",
            )],
        )
        .err()
        .unwrap();
        assert!(err.contains("no machine-readable check"), "{err}");
        // f01's is a real check of an executable entry — a kept entry
        // carrying it is the second mismatched pair.
        let err = FraudGraphs::new(
            FraudConfig::ci(),
            &[query(
                "f01-monopartite-cycle",
                Classification::Kept,
                "RETURN 1",
            )],
        )
        .err()
        .unwrap();
        assert!(err.contains("a kept entry carries no check"), "{err}");
        // A pin entry handed a real check: its authored failure is its
        // check — an entry that can never answer-check may not carry
        // one.
        let err = FraudGraphs::new(
            FraudConfig::ci(),
            &[query(
                "f01-monopartite-cycle",
                Classification::ErrorPin {
                    error: "UnsafeRepeatableElements".into(),
                },
                "RETURN 1",
            )],
        )
        .err()
        .unwrap();
        assert!(
            err.contains("a pin entry's authored failure is its check"),
            "{err}"
        );
        // An id with no oracle row.
        let err = FraudGraphs::new(
            FraudConfig::ci(),
            &[query(
                "f99-not-in-the-oracle",
                Classification::Kept,
                "RETURN 1",
            )],
        )
        .err()
        .unwrap();
        assert!(err.contains("no oracle row"), "{err}");
        // An oracle row with no query file — the check runs in both
        // directions.
        let mut stale = workload_queries(Workload::FraudQpp).unwrap();
        let dropped = stale.pop().unwrap();
        let err = FraudGraphs::new(FraudConfig::ci(), &stale).err().unwrap();
        assert!(
            err.contains(&format!(
                "{}: an oracle entry with no query file",
                dropped.id
            )),
            "{err}"
        );
    }

    #[test]
    fn the_planted_ring_check_requires_the_planted_cycle() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute(
            "CREATE (c0:Account {id: 'cycle-0'})-[:T]->(c1:Account {id: 'cycle-1'}),
                    (c1)-[:T]->(c2:Account {id: 'cycle-2'}), (c2)-[:T]->(c0)",
        )
        .unwrap();
        let planted = ["cycle-0", "cycle-1", "cycle-2"].map(String::from).to_vec();
        let found = conn
            .query(
                "MATCH p = (a:Account)-[:T*1..3]->(a) RETURN a.id AS account, [n IN nodes(p) | n.id] AS ring",
            )
            .unwrap();
        assert_eq!(planted_ring_diff(&planted, &found), None);
        // A wrong ring set (a different starting account only) misses the
        // planted cycle.
        let partial = conn
            .query(
                "MATCH p = (a:Account {id: 'cycle-1'})-[:T*1..3]->(a) RETURN a.id AS account, [n IN nodes(p) | n.id] AS ring",
            )
            .unwrap();
        assert!(planted_ring_diff(&planted, &partial).is_some());
        // A row of the wrong shape is reported, not coerced.
        let shaped = conn
            .query("MATCH (a:Account {id: 'cycle-0'}) RETURN a.id AS account, 1 AS ring")
            .unwrap();
        assert!(planted_ring_diff(&planted, &shaped)
            .unwrap()
            .contains("is not (account, ring of ids)"));
        // A ring whose members are not ids is a shape difference too —
        // reported immediately, not scanned past.
        let members = conn
            .query("MATCH (a:Account {id: 'cycle-0'}) RETURN a.id AS account, [1, 2] AS ring")
            .unwrap();
        assert!(planted_ring_diff(&planted, &members)
            .unwrap()
            .contains("is not a list of ids"));
    }

    #[test]
    fn the_fraud_fixture_loads_and_f06_answers_its_oracle() {
        // The normal-lane answer check (fast, no generator, no data set):
        // the fixture plus the f06 statement against oracle.json.
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        let fixture = read_fixture().unwrap();
        conn.execute(&fixture).unwrap();
        let oracle = fraud_oracle().unwrap();
        let q = workload_queries(Workload::FraudQpp)
            .unwrap()
            .into_iter()
            .find(|q| q.id == "f06-sample-ring")
            .unwrap();
        let FraudCheck::Rows(expected) = &oracle["f06-sample-ring"] else {
            panic!("f06 carries a rows check");
        };
        let result = conn.query(&q.text).unwrap();
        assert_eq!(
            answer_diff(&oracle["f06-sample-ring"], &[], &result),
            None,
            "the designed sample ring is the one answer"
        );
        assert_eq!(expected.rows.len(), 1, "one designed ring");
    }

    /// The lane routes every committed entry and names the graphs it
    /// opened — the planted-ring entries onto the generated graph, the
    /// rows entry onto the fixture, the kept entries onto the principal
    /// graph.
    #[test]
    fn the_lane_routes_every_committed_entry_and_reports_its_graphs() {
        let queries = workload_queries(Workload::FraudQpp).unwrap();
        let mut lane = FraudGraphs::new(FraudConfig::ci(), &queries).unwrap();
        for q in &queries {
            let _routed = lane.route(q).unwrap();
        }
        assert_eq!(
            lane.graph_line(),
            "fraud graphs: the generated graph + the f06 fixture"
        );
        let Some(report) = lane.fraud_report() else {
            panic!("the fraud lane reports its generator knobs");
        };
        assert_eq!((report.scale, report.seed), (0.05, 42));
    }
}
