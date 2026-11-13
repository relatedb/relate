//! The agg-stress workload's own facts (LABS-733, the measuring net for
//! LABS-685's planned chunk-native streaming fold): five IN-HOUSE
//! queries over the pinned graph-benchmark-ldbc SF0.1 store, synthetic
//! aggregation-stress shapes — bounded `knows` walks whose result
//! streams are dominated by rows flowing into an aggregation — their
//! oracle the value this build answers, verified by execution against
//! the pinned graph-benchmark-ldbc SF0.1 data (2026-11-02) and recorded
//! per scale factor in `oracle-sf<SF>.json`, never an upstream answer:
//! no upstream query of this shape exists.
//!
//! The lane itself is the shared rows-oracle store lane
//! ([`super::oracle_store::RowsOracleStoreLane`]) — this workload
//! shares its whole policy with the other in-house rows-oracle
//! workloads (the graph-benchmark-ldbc store, the rows oracle,
//! constants inline so an executed entry binds no parameters), so it
//! shares the implementation too: this module owns the workload's
//! facts and its tests, never a second copy of the lane.

#[cfg(test)]
mod tests {
    use super::super::Workload;

    #[test]
    fn the_committed_oracle_covers_every_executable_entry() {
        super::super::fixtures::the_oracle_covers(Workload::AggStress, 5, 10);
    }

    /// The lane routes every committed entry — the store lane's own test
    /// wiring (`oracle_store::assert_test_lane_routes`), which builds
    /// this workload's lane and asserts through the shared
    /// lane-agnostic body.
    #[test]
    fn the_lane_routes_every_committed_entry() {
        super::super::oracle_store::assert_test_lane_routes(Workload::AggStress);
    }
}
