//! The corpus tree's shared test scaffolding (LABS-733, LABS-1531): the
//! fixtures and assertion bodies the lanes' and the root module's tests
//! build from — one home, kept out of the production root so `corpus.rs`
//! stays the query-file loader, the workload vocabulary and the lane
//! configuration its own doc describes. Compiled only under
//! `#[cfg(test)]` (declared there).

use super::*;

/// A fixture [`CorpusQuery`] for the corpus tree's tests — the one
/// constructor home: no file behind it, so no sha256.
pub(super) fn query(id: &str, classification: Classification, text: &str) -> CorpusQuery {
    CorpusQuery {
        id: id.into(),
        classification,
        text: text.into(),
        sha256: String::new(),
    }
}

/// A fixture `ok` entry.
pub(super) fn ok_query(id: &str, text: &str) -> CorpusQuery {
    query(id, Classification::Ok, text)
}

/// The lane-routing contract every rows-oracle lane's committed-entry
/// test asserts — the one spelling, beside the other shared test
/// fixtures, and lane-agnostic: each `ok` entry of the workload RUNS
/// answer-checked (the constant-inline lanes bind no parameters, so the
/// target's map is empty) and a kept entry PLANS, never runs. Each
/// workload's test builds its own lane — the lane type is the
/// workload's, never a parameter here — and calls this.
pub(super) fn assert_routes_every_committed_entry(
    lane: &mut dyn graph::CorpusLane,
    queries: &[CorpusQuery],
) {
    for q in ok_entries(queries) {
        let graph::Routed::Run(target) = lane.route(q).unwrap() else {
            panic!("{}: an ok entry runs", q.id);
        };
        assert!(target.params.is_empty(), "{}: no parameters", q.id);
        assert!(target.check.is_some(), "{}: answer-checked", q.id);
    }
    // A kept entry plans, never runs — a synthetic probe: a Kept
    // classification resolves through the shared non-executing table,
    // which never reads the id or text.
    assert!(matches!(
        lane.route(&query("a-kept-probe", Classification::Kept, "RETURN 1"))
            .unwrap(),
        graph::Routed::Plan(_)
    ));
}

/// The entries that execute — the one predicate the routing assertion
/// below uses, so "which entries run" is spelled once (an `ok` entry
/// runs and is answer-checked; a kept or pinned entry never does). The
/// coverage check walks the oracle table instead, because the loader's
/// validation has already proven that bijection.
pub(super) fn ok_entries(queries: &[CorpusQuery]) -> impl Iterator<Item = &CorpusQuery> {
    queries
        .iter()
        .filter(|q| q.classification == Classification::Ok)
}

/// The committed-oracle test's shared body — three rows-oracle workloads
/// carry the same assertions (the oracle's entries match the workload's
/// executable entries; every entry answers its recorded rows), so the
/// body lives once and each workload's test is one call with its own
/// oracle file and counts — the file from
/// [`Workload::oracle_path`], the one derivation of the convention.
pub(super) fn the_oracle_covers(w: Workload, entries: usize, rows_per_entry: usize) {
    let queries = workload_queries(w).unwrap();
    let (oracle, _) = super::load_oracle(w, ScaleFactor::Sf0_1, &queries).unwrap();
    assert_eq!(oracle.len(), entries, "{w}");
    // The loader's validation already proved the bijection — every `ok`
    // entry carries a row and no kept or pinned entry does — so the
    // table itself is the complete inventory: walk it, never re-look-up
    // what the validation just guaranteed.
    for (id, entry) in &oracle {
        assert_eq!(
            entry.expected.rows.len(),
            rows_per_entry,
            "{id}: its recorded rows"
        );
    }
}

/// A fixture `error-pin` entry pinning the one authored error the
/// corpus's pins carry.
pub(super) fn pin_query(id: &str, text: &str) -> CorpusQuery {
    query(
        id,
        Classification::ErrorPin {
            error: "UnsafeRepeatableElements".into(),
        },
        text,
    )
}
