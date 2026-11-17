//! Anonymous usage telemetry for the `relate` CLI (ADR-0079).
//!
//! The event taxonomy (`event.rs`) is compiled into every build, so call
//! sites stay unconditional and the privacy contract below lives in one
//! place. The `telemetry` feature (ADR-0104) adds the live handle
//! (`handle.rs`: the spool and drain-thread lifecycle); the Mixpanel
//! `/track` client, the layered opt-out, and the state files live in
//! `relate-feedback` (ADR-0588). Without the feature, `Telemetry` is a
//! no-op handle (`noop.rs`).
//!
//! Privacy (ADR-0079 §4): event builder types have no fields for query text,
//! database paths, or parameter values, so capturing those is a compile
//! error. The full event/property listing ships in `docs/api/cli/spec.md`.

// Without the feature events are discarded unread, so their fields and
// serialization helpers (and, with no commands built, `Event::Command`)
// go unused.
#[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
mod event;

#[cfg(feature = "telemetry")]
mod drain;
#[cfg(feature = "telemetry")]
mod envelope;
#[cfg(feature = "telemetry")]
mod handle;
#[cfg(not(feature = "telemetry"))]
mod noop;
#[cfg(feature = "telemetry")]
pub(crate) mod spool;

pub use event::{Event, OutputMode, QuerySource, StartupKind, StorageKind};
#[cfg(feature = "telemetry")]
pub use handle::Telemetry;
#[cfg(not(feature = "telemetry"))]
pub use noop::Telemetry;

/// Build an injectable env lookup from key/value pairs, for tests — every
/// test module shares this one helper.
#[cfg(test)]
pub(crate) fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |k| {
        pairs
            .iter()
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.to_string())
    }
}

/// The event names inside captured envelopes — the one extraction every
/// test module's claim-side and sink-side assertions read, so the view
/// of "what the spool held" or "what the sink took" cannot drift between
/// them.
#[cfg(all(test, feature = "telemetry"))]
pub(crate) fn event_names<'a>(
    values: impl IntoIterator<Item = &'a serde_json::Value>,
) -> Vec<String> {
    values
        .into_iter()
        .map(|event| event["event"].as_str().expect("event").to_string())
        .collect()
}

/// Seed one pending spool file carrying `marker`: the backlog a
/// delivery test finds at its arming, written the way the run before it
/// would have. The e2e suites have their own fixture (the support
/// module's `seed_spool`) — this is the src-side spelling.
#[cfg(all(test, feature = "telemetry"))]
pub(crate) fn seed_spool(dir: &std::path::Path, marker: &str) {
    spool::write(
        &spool::spool_file(dir),
        &[serde_json::json!({"event": marker})],
    );
}

/// The one enabled-handle constructor the telemetry test modules share
/// (ADR-0615): a live pipeline against a loopback endpoint the test
/// controls — the refused-port shape the handle-lifecycle tests use and
/// the mock-sink shape the delivery tests use are the two endpoints of
/// this one constructor. Behind the `telemetry` feature, which owns the
/// live `enable`.
#[cfg(all(test, feature = "telemetry"))]
pub(crate) fn handle_in(dir: &std::path::Path, endpoint: &str) -> Telemetry {
    let pairs = [("RELATE_MIXPANEL_ENDPOINT", endpoint)];
    let env = env_of(&pairs);
    Telemetry::enable(
        relate_feedback::opt_out::EffectiveState::Default,
        &env,
        Some(dir.to_path_buf()),
    )
}
