//! The Mixpanel `/track` client and the consent model shared by the
//! `relate` CLI's anonymous usage metrics (ADR-0079) and person-consented
//! agent feedback (ADR-0588).
//!
//! - [`client`] is the one `/track` POST: the endpoint constant and its
//!   allow-list, a short deadline, no retries, redirects never followed,
//!   `ip=0`.
//! - [`consent`] is the layered opt-out resolution (`DO_NOT_TRACK`, the
//!   environment, the config key) and the deciding layer,
//!   `relate telemetry status` reports.
//! - [`state`] is the config-directory state files: the install ID, the
//!   one-time first-run-notice flag, and the persistent config keys.
//! - [`report`], [`policy`], [`authorization`], [`outcome`] and [`submit`]
//!   are the feedback domain: the report model, its validation and
//!   redaction, the consent record, the submit vocabulary, and the submit
//!   pipeline; the local receipt log is crate-private.
//! - [`sender`] sends a validated report as the feedback `Mixpanel` event,
//!   under its own endpoint policy.
//!
//! The crate takes the config directory and an environment lookup as
//! arguments; the CLI keeps its one config-directory rule.

pub mod authorization;
pub mod client;
pub mod consent;
pub mod ids;
pub mod outcome;
pub mod policy;
pub(crate) mod receipt;
pub mod report;
pub mod sender;
pub mod state;
pub mod submit;

#[cfg(test)]
mod test_support;
