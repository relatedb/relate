//! The shared rows-oracle store lane — the one implementation every
//! in-house workload whose entries are answer-checked against a
//! `oracle-sf<SF>.json` rows table runs (agg-stress, LABS-733; equi-join,
//! LABS-1531): open the graph-benchmark-ldbc store — the same store machinery as
//! the pathdb and snb-bi lanes — and answer-check every executed entry
//! against the workload's verified rows oracle. What every one of these
//! workloads shares is the whole policy — the graph-benchmark-ldbc
//! store, the rows oracle, constants inline so an executed entry binds
//! no parameters — so they share the implementation too: `new` takes
//! the workload, derives its one oracle path ([`Workload::oracle_path`],
//! shared by the loader, the build-time validation and the route's miss
//! text) and stores it — the lane carries no workload beyond that
//! derivation, and the file conventions themselves are owned by
//! `corpus.rs`'s `Workload::oracle_path`. A workload of this
//! class is a directory of queries plus an oracle file plus its own
//! tests — never a second copy of this module.
//!
//! This module imports the corpus tree's shared vocabulary — the `graph`
//! module's execution types ([`GraphHandle`], [`Routed`], and the
//! rows-oracle route [`Routed::rows_checked`]) and the `files`
//! submodule's rows-oracle machinery (the [`Oracle`] table type, the one
//! loader and validation, the per-scale file-name convention) — and
//! never imports the runner or the other lanes.

use std::path::PathBuf;

use super::files::Oracle;
use super::graph::{CorpusLane, GraphHandle, Routed};
use super::load_oracle;
use super::report::graph_benchmark_ldbc_store_line;
use super::{CorpusQuery, Workload};
use crate::suite::GraphOptions;
#[cfg(test)]
use crate::workload::ScaleFactor;
use crate::Result;

/// A rows-oracle store lane: the graph-benchmark-ldbc store (boxed — a
/// store's database and connection dwarf every other lane's state), the
/// oracle each executed entry is answer-checked with, and the oracle
/// file's path. The path is derived ONCE at build —
/// [`Workload::oracle_path`], the one derivation the loader, the
/// build-time validation and the route's miss text share — and the lane
/// stores it; it carries no workload beyond that derivation.
/// (`workloads/<name>/oracle-sf<SF>.json` holds the verified values over
/// the pinned graph-benchmark-ldbc data, recorded once per scale factor — a
/// store at another scale factor answers different rows, so the lane
/// resolves its own workload's file, and a scale factor with no file
/// fails loudly at load, never reuses another scale's rows.)
pub(super) struct RowsOracleStoreLane {
    graph: Box<GraphHandle>,
    oracle: Oracle,
    oracle_path: PathBuf,
    line: String,
}

impl RowsOracleStoreLane {
    /// Build the lane: run the shared oracle build sequence, then open the
    /// graph-benchmark-ldbc store.
    pub(super) fn new(
        workload: Workload,
        opts: &GraphOptions,
        queries: &[CorpusQuery],
    ) -> Result<Self> {
        let (oracle, path) = load_oracle(workload, opts.sf, queries)?;
        let (graph, origin) = GraphHandle::open_graph_benchmark_ldbc(opts)?;
        Ok(Self {
            graph: Box::new(graph),
            oracle,
            oracle_path: path,
            line: graph_benchmark_ldbc_store_line(origin),
        })
    }

    /// The tests' lane over a memory graph: the SAME oracle build
    /// sequence [`Self::new`] runs ([`load_oracle`]), so a test lane can
    /// never be wired to a different file — or validated differently —
    /// than the lane it stands in for. The report-only `line` field is
    /// empty here (the lane-under-test routes, never reports), stated
    /// once at this constructor rather than fabricated per literal.
    #[cfg(test)]
    pub(super) fn for_test(
        workload: Workload,
        graph: GraphHandle,
        queries: &[CorpusQuery],
    ) -> Result<Self> {
        let (oracle, path) = load_oracle(workload, ScaleFactor::Sf0_1, queries)?;
        Ok(Self {
            graph: Box::new(graph),
            oracle,
            oracle_path: path,
            line: String::new(),
        })
    }
}

impl CorpusLane for RowsOracleStoreLane {
    /// The report's one-line graph provenance.
    fn graph_line(&self) -> String {
        self.line.clone()
    }

    /// The entry's full disposition: the shared rows-oracle route
    /// ([`Routed::rows_checked`]) over the workload's own oracle file —
    /// an `ok` entry runs answer-checked against its row with no
    /// parameters (the queries carry their constants inline), a kept or
    /// pin entry resolves through the shared non-executing table, and an
    /// `ok` id with no row is refused.
    fn route<'a>(&'a mut self, q: &'a CorpusQuery) -> Result<Routed<'a>> {
        let Self {
            graph,
            oracle,
            oracle_path,
            ..
        } = self;
        Routed::rows_checked(&mut graph.conn, None, oracle, oracle_path, q)
    }
}

/// The store lane's committed-entry route test body — the concrete
/// construction (this lane's `for_test` over a memory graph) plus the
/// shared lane-agnostic assertion. It lives HERE, the lane's own module,
/// because it builds this concrete lane; the other rows-oracle lanes
/// (ukrailway, fraud) build their own, and the workload modules' tests
/// call this with their `Workload`.
#[cfg(test)]
pub(super) fn assert_test_lane_routes(workload: Workload) {
    let queries = super::workload_queries(workload).unwrap();
    let mut lane = RowsOracleStoreLane::for_test(
        workload,
        super::graph::GraphHandle::memory("the route test's graph").unwrap(),
        &queries,
    )
    .unwrap();
    super::fixtures::assert_routes_every_committed_entry(&mut lane, &queries);
}

#[cfg(test)]
mod tests {
    use super::super::files::load_rows_oracle;

    /// A scale factor with no oracle file fails loudly — never silently
    /// reuses another scale factor's rows. The property of the shared
    /// loader, stated once here: every rows-oracle workload inherits it,
    /// none re-tests it.
    #[test]
    fn a_scale_factor_without_an_oracle_file_fails_loudly() {
        // Asserted against a path that provably does not exist (a temp
        // dir), not against SF1 — a future SF1 oracle must not turn this
        // test red; the property under test is the refusal.
        let dir = tempfile::tempdir().unwrap();
        let err = load_rows_oracle(&dir.path().join("oracle-sf0.9.json")).unwrap_err();
        assert!(err.contains("oracle-sf0.9.json"), "{err}");
    }
}
