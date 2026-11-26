//! ADR-0687 decision 6: one execution's grade is the store reads it performed,
//! taken at statement end. The streamed read entry (`execute_part_read_streamed`,
//! ADR-0682) is reached on a route that bypasses the seeded entries entirely —
//! this test pins that its store reads are counted before the probe's reading
//! is taken (the consumer is fed inline, so no row arrives after it).

use std::collections::BTreeMap;

use relate_core::{Result, Value};

use super::dispatch::streamable_part;
use super::test_support::build;
use super::{execute_read_streaming_at, ExecRequest, GradeProbe, ReadAdmission};
use crate::consumer::{BatchFlow, RowConsumer, StreamSink};

/// The consumer a streamed read's rows reach: counts them, never stops.
struct CountingConsumer {
    rows: usize,
}

impl RowConsumer for CountingConsumer {
    fn columns(&mut self, _columns: &[String]) -> Result<()> {
        Ok(())
    }

    fn batch(&mut self, rows: Vec<Vec<Value>>) -> Result<BatchFlow> {
        self.rows += rows.len();
        Ok(BatchFlow::Continue)
    }
}

/// A streamed scan bypasses the seeded entries, yet every store read it
/// performs lands before the probe's reading is taken — never a zero reading
/// standing in for a run whose rows arrived later. The route is pinned, not
/// assumed: the fixture's shape must admit the streamed part, so a shape
/// change that stops streaming reds this test instead of quietly grading the
/// materialized fallback.
#[test]
fn a_streamed_scan_reads_the_store_before_the_reading_is_taken() {
    let built = build(&[&["User"], &["User"], &["User"]], &[]);
    let cypher = "MATCH (n:User) RETURN n.id AS id";
    let plan = relate_planner::plan(relate_front::parse_and_lower(cypher).unwrap(), &mut |_| {
        false
    })
    .unwrap();
    assert!(
        streamable_part(&plan).unwrap().is_some(),
        "the fixture must route through the streamed part this test pins"
    );
    let pin = built.graph.pin_now().unwrap();
    let schema = built.graph.schema_snapshot();
    let mut consumer = CountingConsumer { rows: 0 };
    let mut sink = StreamSink::new(&mut consumer);
    let probe = GradeProbe::begin();
    execute_read_streaming_at(
        &ReadAdmission {
            graph: &built.graph,
            snapshot_ts: pin.ts(),
            schema: &schema,
            request: ExecRequest::new(&plan, &BTreeMap::new(), cypher),
            cancel: None,
            ontology_document: None,
        },
        &mut sink,
    )
    .unwrap();
    let reads = probe.finish();
    assert_eq!(consumer.rows, 3, "the streamed terminal fed every row");
    assert!(
        reads >= 3,
        "the scan's reads landed before the reading was taken: {reads}"
    );
}
