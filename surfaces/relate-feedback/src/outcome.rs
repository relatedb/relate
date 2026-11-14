//! The submit pipeline's vocabulary (ADR-0588 §1): the coarse outcome, the
//! reason a submit ended as it did, and the one mapping between them.
//!
//! A leaf module: [`crate::submit`] produces the pair, `crate::receipt`
//! records it, and neither owns the vocabulary the other reads.

use serde::Serialize;

/// The coarse result of a submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The sink confirmed the report.
    Sent,
    /// Nothing was attempted: opted out or not authorized.
    Skipped,
    /// The report was refused locally or by the sink.
    Rejected,
    /// The send failed and is safe to retry with a fresh id.
    Failed,
    /// No answer: the send may or may not have landed.
    Unknown,
}

impl Outcome {
    /// A normal local decision — `sent` or `skipped`. The CLI's exit code
    /// derives from this one predicate.
    pub fn is_success(self) -> bool {
        matches!(self, Outcome::Sent | Outcome::Skipped)
    }
}

/// Why the outcome is what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Sent,
    DoNotTrack,
    TelemetryOff,
    NotEnabled,
    AuthorizationExpired,
    InvalidInput,
    PolicyViolation,
    ServerRejected,
    ServerError,
    NotConfigured,
    Unreachable,
    Timeout,
}

impl Reason {
    /// Every reason, in [`Reason::outcome`]'s order: a coverage test that
    /// iterates this list cannot silently skip a new variant. Test-only: the
    /// mapping has no caller that needs the whole vocabulary.
    #[cfg(test)]
    pub(crate) const ALL: [Reason; 12] = [
        Reason::Sent,
        Reason::DoNotTrack,
        Reason::TelemetryOff,
        Reason::NotEnabled,
        Reason::AuthorizationExpired,
        Reason::InvalidInput,
        Reason::PolicyViolation,
        Reason::ServerRejected,
        Reason::ServerError,
        Reason::NotConfigured,
        Reason::Unreachable,
        Reason::Timeout,
    ];

    /// The coarse outcome this reason reports — the one mapping, so the
    /// pair never drifts apart.
    pub(crate) fn outcome(self) -> Outcome {
        match self {
            Reason::Sent => Outcome::Sent,
            Reason::DoNotTrack
            | Reason::TelemetryOff
            | Reason::NotEnabled
            | Reason::AuthorizationExpired => Outcome::Skipped,
            Reason::InvalidInput | Reason::PolicyViolation | Reason::ServerRejected => {
                Outcome::Rejected
            }
            Reason::ServerError | Reason::NotConfigured | Reason::Unreachable => Outcome::Failed,
            Reason::Timeout => Outcome::Unknown,
        }
    }
}
