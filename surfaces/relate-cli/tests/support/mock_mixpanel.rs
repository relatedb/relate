//! Mock Mixpanel `/track` sink for the telemetry E2E suites: the canonical
//! loopback capture server (`relate-test-support`'s
//! [`spawn_capture_server`]) answers every POST on 127.0.0.1 (ephemeral
//! port) with `200 OK` and body `1`, exactly like the real API, and records
//! each request body. This module adds only the Mixpanel-specific residue:
//! envelope flattening, event polling, and the quiet-grace assertion.
//!
//! Every test binary that includes `support` compiles this module but uses a
//! subset of it, so dead-code warnings here would only be noise.

#![allow(dead_code)]

use std::path::Path;
use std::time::{Duration, Instant};

use relate_test_support::loopback::{spawn_capture_server, CaptureSink};

use super::{run_cli, CliRunOptions};

/// How long one delivery attempt inside
/// [`MockMixpanel::deliver_spooled`] waits for the mock to see the
/// spooled events before the next follow-up run starts. The drain POST
/// against a loopback mock is milliseconds on a quiet machine, but the
/// suites run their tests in parallel and a saturated runner can starve
/// the drain thread and the mock's accept loop for seconds at a stretch
/// — anything slower than this is a failed attempt, not a slow one.
pub const DELIVERY_WAIT: Duration = Duration::from_secs(5);

/// Grace period for the opt-out tests: a disabled run makes no connections
/// at all, so anything arriving within this window is a failure.
pub const QUIET_GRACE: Duration = Duration::from_millis(500);

/// A minimal mock of the Mixpanel `/track` endpoint (see the module docs).
/// Dropping the mock stops the server and joins its thread.
pub struct MockMixpanel {
    sink: CaptureSink,
    port: u16,
}

/// One delivery follow-up run's fixed shape (ADR-0545 §2): the shared
/// work query (the suite imports [`super::DELIVERY_WORK_QUERY`]) plus
/// the suite's env axes. The delivery loop below owns its execution —
/// spawn, success assert, retry — so no suite re-implements the
/// follow-up; only the env wiring, which genuinely differs per suite,
/// stays at the call site.
pub struct FollowupRun<'a> {
    /// The statement to run: a work query, whose engine work keeps the
    /// run alive long enough for its drain thread to POST.
    pub args: &'a [&'a str],
    /// Extra env axes (an `XDG_CONFIG_HOME` root, say); empty for the
    /// `$HOME`-shaped suites.
    pub envs: &'a [(&'a str, &'a str)],
    /// The `$HOME` override, when the suite drives the config dir that
    /// way.
    pub home: Option<&'a Path>,
    /// The working-directory override, when the suite needs one.
    pub cwd: Option<&'a Path>,
}

impl FollowupRun<'_> {
    /// One follow-up execution against `mock` — the loop's one
    /// load-bearing step, owned here.
    fn spawn_against(&self, mock: &MockMixpanel) {
        let url = mock.url();
        let mut options = CliRunOptions::new(self.args).with_endpoint(&url);
        if !self.envs.is_empty() {
            options = options.with_envs(self.envs);
        }
        if let Some(home) = self.home {
            options = options.in_home(home);
        }
        if let Some(cwd) = self.cwd {
            options = options.in_dir(cwd);
        }
        let (status, _, stderr) = run_cli(options);
        assert!(status.success(), "delivery follow-up failed: {stderr}");
    }
}

impl MockMixpanel {
    pub fn start() -> Self {
        // The real `/track` API replies `200 OK` with body `1`.
        let (sink, port) = spawn_capture_server("200 OK", "1");
        Self { sink, port }
    }

    /// The URL to hand to `RELATE_MIXPANEL_ENDPOINT`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn request_count(&self) -> usize {
        self.sink.request_count()
    }

    /// All captured events: every request body is a JSON array of Mixpanel
    /// envelopes; flatten them into one event list.
    pub fn events(&self) -> Vec<serde_json::Value> {
        self.sink
            .bodies()
            .iter()
            .flat_map(|body| {
                let value: serde_json::Value = serde_json::from_str(body)
                    .unwrap_or_else(|e| panic!("mock received a non-JSON body: {e}: {body}"));
                value
                    .as_array()
                    .expect("/track body must be a JSON array")
                    .clone()
            })
            .collect()
    }

    /// Poll the captured events until `pred` holds or `timeout` passes.
    pub fn wait_for_events(
        &self,
        timeout: Duration,
        pred: impl Fn(&[serde_json::Value]) -> bool,
    ) -> bool {
        let start = Instant::now();
        loop {
            if pred(&self.events()) {
                return true;
            }
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Assert that no request arrives within [`QUIET_GRACE`]. `what` names
    /// the opted-out context being asserted (e.g. `DO_NOT_TRACK=1`) so a
    /// failure says which layer leaked.
    pub fn assert_quiet(&self, what: &str) {
        std::thread::sleep(QUIET_GRACE);
        assert_eq!(
            self.request_count(),
            0,
            "{what} must send no requests; mock captured: {:?}",
            self.sink.bodies()
        );
    }

    /// The spool model's delivery loop (ADR-0545 §2), the e2e suites' one
    /// shared shape: a run's events reach the sink through a LATER
    /// enabled run's drain, and a short run can itself exit before its
    /// drain POST completes — the claim then waits for the run after it.
    /// At-least-once across runs is the contract; this loop is its test
    /// shape. `run_under_test` performs the run the suite is asserting
    /// (its own asserts live inside the closure); `followup` is the
    /// declarative shape of the delivery runs the loop spawns until
    /// `pred` holds on the captured events, bounded. Returns the
    /// captured events.
    pub fn deliver_spooled(
        &self,
        run_under_test: impl FnOnce(),
        followup: FollowupRun<'_>,
        pred: impl Fn(&[serde_json::Value]) -> bool,
    ) -> Vec<serde_json::Value> {
        run_under_test();
        // The run under test only spooled (ADR-0545 §1): waiting before
        // the first follow-up would be a guaranteed stall, so deliver
        // immediately and wait only between attempts.
        followup.spawn_against(self);
        for _ in 0..3 {
            if self.wait_for_events(DELIVERY_WAIT, &pred) {
                return self.events();
            }
            followup.spawn_against(self);
        }
        assert!(
            self.wait_for_events(DELIVERY_WAIT, &pred),
            "spooled events were never delivered by a follow-up run; captured: {:?}",
            self.events()
        );
        self.events()
    }
}
