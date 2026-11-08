//! Anonymous usage telemetry for the `relate` CLI (ADR-0079).
//!
//! The event taxonomy (`event.rs`) is compiled into every build, so call
//! sites stay unconditional and the privacy contract below lives in one
//! place. The `telemetry` feature (ADR-0104) adds the live handle
//! (`handle.rs`: opt-out resolution, config/state files, Mixpanel worker);
//! without it, `Telemetry` is a no-op handle (`noop.rs`).
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
pub(crate) mod config;
#[cfg(feature = "telemetry")]
mod envelope;
#[cfg(feature = "telemetry")]
mod handle;
#[cfg(not(feature = "telemetry"))]
mod noop;
#[cfg(feature = "telemetry")]
mod optout;
#[cfg(feature = "telemetry")]
pub(crate) mod spool;

pub use event::{Event, OutputMode, QuerySource, StartupKind, StorageKind};
#[cfg(feature = "telemetry")]
pub use handle::Telemetry;
#[cfg(not(feature = "telemetry"))]
pub use noop::Telemetry;
#[cfg(feature = "telemetry")]
pub use optout::{EffectiveState, Layer};

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
