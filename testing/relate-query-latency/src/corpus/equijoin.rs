//! The equi-join workload's own facts (LABS-1531, the measuring net for
//! the planned hash-join operator): three IN-HOUSE queries over the
//! pinned graph-benchmark-ldbc SF0.1 store — two MATCH patterns sharing
//! no anchor, joined by a WHERE equality on a shared property, the
//! shape the engine today multiplies as a cross product inside the
//! Expand (the `CartesianProduct` notification, 03N90) — their oracle
//! the value this build answers, verified by execution against the
//! pinned graph-benchmark-ldbc SF0.1 data (2026-11-13) and recorded in
//! `oracle-sf<SF>.json`, never an upstream answer: no upstream query of
//! this shape exists.
//!
//! The lane itself is the shared rows-oracle store lane
//! ([`super::oracle_store::RowsOracleStoreLane`]) — this workload
//! shares its whole policy with agg-stress (the graph-benchmark-ldbc
//! store, the rows oracle, constants inline so an executed entry binds
//! no parameters), so it shares the implementation too: this module
//! owns the workload's facts and its tests, never a second copy of the
//! lane.

#[cfg(test)]
mod tests {
    use super::super::Workload;

    #[test]
    fn the_committed_oracle_covers_every_executable_entry() {
        super::super::fixtures::the_oracle_covers(Workload::EquiJoin, 3, 1);
    }

    /// The lane routes every committed entry — the store lane's own test
    /// wiring (`oracle_store::assert_test_lane_routes`), which builds
    /// this workload's lane and asserts through the shared
    /// lane-agnostic body.
    #[test]
    fn the_lane_routes_every_committed_entry() {
        super::super::oracle_store::assert_test_lane_routes(Workload::EquiJoin);
    }
}
