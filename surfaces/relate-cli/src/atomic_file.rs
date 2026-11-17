//! The CLI's view of the staged-write kit, which lives in
//! `relate_feedback::atomic_file` (the feedback state files are its other
//! caller, and the CLI depends on that crate, not the reverse): the
//! non-predictable staging-name recipe, the exclusive private create step,
//! and the atomic writer.
//!
//! The module compiles with the `staging` marker feature (the three
//! staging features list it); each re-export is gated to the features whose
//! callers use it — the upgrade's binary replacement ([`write_atomic`]),
//! the remote `--database` bundle ([`staging_name`], [`create_staging_file`])
//! and the telemetry spool's per-run files ([`staging_name`],
//! [`create_exclusive`], ADR-0545 §1).

#[cfg(feature = "telemetry")]
pub(crate) use relate_feedback::atomic_file::create_exclusive;
#[cfg(feature = "remote-database")]
pub(crate) use relate_feedback::atomic_file::create_staging_file;
// The remote bundle's own callers take the name through
// `create_staging_file`; only its test pins the recipe directly.
#[cfg(any(feature = "telemetry", all(test, feature = "remote-database")))]
pub(crate) use relate_feedback::atomic_file::staging_name;
#[cfg(feature = "upgrade")]
pub(crate) use relate_feedback::atomic_file::write_atomic;
