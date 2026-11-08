//! No-op telemetry handle for builds without the `telemetry` feature
//! (ADR-0104): events are discarded, nothing is read or written, and no
//! thread or network connection is ever created.

use super::Event;

pub struct Telemetry;

impl Telemetry {
    pub fn new() -> Self {
        Telemetry
    }

    /// Never claims: with telemetry compiled out there is nothing to notify.
    pub fn claim_first_run_notice(&mut self) -> bool {
        false
    }

    /// The no-op twin of the live handle's `emit`: `&mut self`, matching
    /// the live receiver so call sites are identical in every bundle.
    pub fn emit(&mut self, _event: Event) {}

    pub fn flush(&mut self) {}
}
