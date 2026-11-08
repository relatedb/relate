//! Cold-start-to-first-query guard (ADR-0545 §8): a short oneshot run
//! with telemetry at its product default must stay imperceptibly slow —
//! the exit path is one local spool append, never a network wait. The
//! guard exists for the regression class ADR-0545 records: the
//! pre-spool exit waited on the network, ~2 s of SYN retransmission
//! against a refused port on Windows and up to the HTTP timeout against
//! a slow real endpoint elsewhere.

#![cfg(feature = "telemetry")]

mod support;

use std::time::{Duration, Instant};

use support::{run_cli, tempdir, CliRunOptions as Opts};

/// The bound: generous for a debug build on a loaded CI runner (process
/// spawn + engine open + statement + exit), but well under the failure
/// mode it guards — the Windows refused-port connect alone was ~2 s.
const COLD_START_BOUND: Duration = Duration::from_millis(1500);

#[test]
fn test_short_run_cold_start_stays_imperceptible() {
    let home = tempdir();
    // The product default telemetry (`with_default_telemetry`) against
    // the harness's refused endpoint: the run is enabled, its drain
    // fails offline, and none of that may sit on the exit path.
    let start = Instant::now();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["RETURN 1 AS n"])
            .in_home(home.path())
            .with_default_telemetry(),
    );
    let elapsed = start.elapsed();
    assert!(status.success(), "one-shot query failed: {stderr}");
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    // The measured number rides the test log.
    println!("cold_start_to_first_query: {elapsed:?}");
    assert!(
        elapsed < COLD_START_BOUND,
        "cold start to first query took {elapsed:?} — the exit path is \
         waiting on something (the network?)"
    );
    // The run really did telemetry, so the timing above is meaningful:
    // its exit write left a spool file under the config dir.
    let cfg = home.path().join(".config").join("relate");
    let spooled = support::pending_telemetry_files_in(&cfg).len();
    assert_eq!(spooled, 1, "expected one spool file, found {spooled}");
}
