//! Feedback submit orchestration (ADR-0588 §1): opt-out, authorization,
//! validation, one send, one local receipt — in that order, so nothing
//! touches the network before the report has passed every local check.
//!
//! The send itself is the [`Sender`] seam. The CLI implements it over
//! [`crate::client::Client`]; tests implement it as a mock that records
//! calls, which is what proves a rejected or unauthorized report is never
//! sent.

use std::path::Path;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};

use crate::authorization::{Authorization, FeedbackConsent};
use crate::consent::{self, EffectiveState};
use crate::ids;
use crate::outcome::{Outcome, Reason};
use crate::policy;
use crate::receipt::{self, Receipt};
use crate::report::{Context, Input, Payload};
use crate::state::ConfigTelemetry;

/// The one send seam: the CLI's Mixpanel client, or a test mock.
pub trait Sender {
    /// POST one validated payload to `endpoint`, the destination the
    /// consent record names; returns why the attempt ended as it did.
    fn send(&self, endpoint: &str, payload: &Payload) -> Reason;
}

/// Everything one submit needs that the caller owns.
pub struct Request<'a> {
    /// The config directory holding the consent record and the receipts.
    pub dir: &'a Path,
    pub input: &'a Input,
    pub context: &'a Context,
    /// The injected environment lookup (`DO_NOT_TRACK`,
    /// `RELATE_TELEMETRY`).
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// The typed `telemetry` read from `config.json`.
    pub config_telemetry: ConfigTelemetry,
    pub now: DateTime<Utc>,
}

/// The submit result document, serialized to stdout by the command.
/// `outcome` and `report_id` are
/// never stored: the one construction path starts every per-arm field at its
/// absent value, and both the wire document and the receipt emit them derived
/// from `reason` and `request_id`.
#[derive(Debug, Clone)]
pub struct OutcomeDoc {
    pub reason: Reason,
    pub request_id: String,
    pub elapsed_ms: u64,
    pub violations: Vec<policy::Violation>,
    pub dropped_fields: Vec<&'static str>,
    /// The transmitted payload, only when the outcome is `sent`.
    pub payload: Option<Payload>,
    pub receipt_path: Option<String>,
}

impl OutcomeDoc {
    /// The one constructor: everything a decision does not set starts absent.
    fn new(reason: Reason, request_id: String, elapsed_ms: u64) -> Self {
        Self {
            reason,
            request_id,
            elapsed_ms,
            violations: Vec::new(),
            dropped_fields: Vec::new(),
            payload: None,
            receipt_path: None,
        }
    }

    /// The coarse outcome, derived from `reason` — never stored beside it.
    pub fn outcome(&self) -> Outcome {
        self.reason.outcome()
    }

    /// The report's id on the wire: a report exists only when it was sent, so
    /// `report_id` is the request id exactly for [`Reason::Sent`] — derived
    /// from the two neighbours, never stored.
    fn report_id(&self) -> Option<&str> {
        (self.reason == Reason::Sent).then_some(self.request_id.as_str())
    }

    /// Append the receipt this outcome implies and record its path; a failed
    /// write leaves `receipt_path` absent and the command carries on.
    fn write_receipt(&mut self, dir: &Path, destination: &str, transmitted: Option<&Payload>) {
        self.receipt_path = receipt::append(
            dir,
            &Receipt {
                reason: self.reason,
                request_id: &self.request_id,
                report_id: self.report_id(),
                destination: Some(destination),
                elapsed_ms: self.elapsed_ms,
                payload: transmitted,
            },
        )
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    }
}

/// The wire document: `outcome` and `report_id` are emitted from `reason`, so
/// the serialized pair cannot disagree.
impl Serialize for OutcomeDoc {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            outcome: Outcome,
            reason: Reason,
            request_id: &'a str,
            report_id: Option<&'a str>,
            elapsed_ms: u64,
            violations: &'a [policy::Violation],
            dropped_fields: &'a [&'static str],
            payload: Option<&'a Payload>,
            receipt_path: Option<&'a str>,
        }
        Wire {
            outcome: self.reason.outcome(),
            reason: self.reason,
            request_id: &self.request_id,
            report_id: self.report_id(),
            elapsed_ms: self.elapsed_ms,
            violations: &self.violations,
            dropped_fields: &self.dropped_fields,
            payload: self.payload.as_ref(),
            receipt_path: self.receipt_path.as_deref(),
        }
        .serialize(serializer)
    }
}

/// Run the submit pipeline. Consent and validation run before
/// [`Sender::send`], and only a validated, authorized report reaches it.
pub fn submit(request: &Request<'_>, sender: &dyn Sender) -> OutcomeDoc {
    let started = Instant::now();
    // Each disabled layer is its own decided state, so no arm folds two
    // layers into one reason.
    match consent::resolve(request.env, request.config_telemetry) {
        EffectiveState::DoNotTrack => return skipped(Reason::DoNotTrack, started),
        EffectiveState::Env(false) | EffectiveState::ConfigFile(false) => {
            return skipped(Reason::TelemetryOff, started)
        }
        EffectiveState::Default | EffectiveState::Env(true) | EffectiveState::ConfigFile(true) => {}
    }

    // The destination is the endpoint the person consented to, not whatever
    // the environment says at submit time (ADR-0588 §3).
    let endpoint = match FeedbackConsent::load(request.dir).authorization(request.now) {
        Authorization::Authorized { endpoint, .. } => endpoint,
        Authorization::Expired => return skipped(Reason::AuthorizationExpired, started),
        Authorization::Missing => return skipped(Reason::NotEnabled, started),
    };

    let checked = policy::check(request.input, request.context);
    let policy_hit = checked.is_policy_violation();
    match checked.result {
        Ok(payload) => {
            let reason = sender.send(&endpoint, &payload);
            let sent = reason.outcome() == Outcome::Sent;
            // Measured after the send, so the document and the receipt carry
            // the submission's elapsed time including the network attempt.
            let mut doc = OutcomeDoc::new(reason, payload.request_id.clone(), elapsed_ms(started));
            doc.dropped_fields = checked.dropped;
            // A send attempt's receipt records what it transmitted, whether or
            // not the sink confirmed it; a run with no configured destination
            // attempted nothing, so its receipt carries no payload. The
            // document keeps it only when sent.
            let transmitted = (reason != Reason::NotConfigured).then_some(&payload);
            doc.write_receipt(request.dir, &endpoint, transmitted);
            doc.payload = sent.then_some(payload);
            doc
        }
        Err(violations) => {
            let reason = if policy_hit {
                Reason::PolicyViolation
            } else {
                Reason::InvalidInput
            };
            let mut doc = OutcomeDoc::new(reason, checked.request_id, elapsed_ms(started));
            doc.violations = violations;
            doc.dropped_fields = checked.dropped;
            doc.write_receipt(request.dir, &endpoint, None);
            doc
        }
    }
}

fn skipped(reason: Reason, started: Instant) -> OutcomeDoc {
    OutcomeDoc::new(reason, ids::uuid_v4(), elapsed_ms(started))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::authorization::MAX_AUTHORIZATION_DAYS;
    use crate::report::MAX_TEXT_BYTES;
    use crate::test_support::{context, env_of, MockSender};

    const ENDPOINT: &str = "https://api.mixpanel.com/track";

    fn input() -> Input {
        Input {
            category: "unclear_error".to_string(),
            expected: "the info command to print the version".to_string(),
            observed: "it printed help instead".to_string(),
            ..Input::default()
        }
    }

    /// Grant the one authorization a test needs, for `endpoint`.
    fn authorize(dir: &Path, now: DateTime<Utc>, endpoint: &str) {
        let mut record = FeedbackConsent::default();
        record.authorize(now, endpoint.to_string());
        record.save(dir).unwrap();
    }

    fn run<'a>(
        dir: &'a Path,
        input: &'a Input,
        env: &'a dyn Fn(&str) -> Option<String>,
        now: DateTime<Utc>,
        sender: &dyn Sender,
    ) -> OutcomeDoc {
        let context = context();
        submit(
            &Request {
                dir,
                input,
                context: &context,
                env,
                config_telemetry: ConfigTelemetry::Missing,
                now,
            },
            sender,
        )
    }

    /// The full opt-out contract for any layer: a skip, its reason, no send,
    /// and no receipt written.
    fn assert_skipped(doc: &OutcomeDoc, reason: Reason, dir: &Path, sender: &MockSender) {
        assert_eq!(doc.outcome(), Outcome::Skipped, "{reason:?}");
        assert_eq!(doc.reason, reason);
        assert_eq!(sender.calls(), 0, "zero client calls");
        assert!(
            !receipt::receipts_path(dir).exists(),
            "a skip writes no receipt"
        );
    }

    #[test]
    fn a_report_without_recorded_consent_is_never_sent() {
        let tmp = tempfile::tempdir().unwrap();
        let sender = MockSender::new(Reason::Sent);
        let doc = run(tmp.path(), &input(), &env_of(&[]), Utc::now(), &sender);
        assert_skipped(&doc, Reason::NotEnabled, tmp.path(), &sender);
    }

    #[test]
    fn an_expired_authorization_is_never_sent() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        authorize(
            tmp.path(),
            now - chrono::Duration::days(MAX_AUTHORIZATION_DAYS + 1),
            ENDPOINT,
        );
        let sender = MockSender::new(Reason::Sent);
        let doc = run(tmp.path(), &input(), &env_of(&[]), now, &sender);
        assert_eq!(doc.reason, Reason::AuthorizationExpired);
        assert_eq!(sender.calls(), 0);
    }

    #[test]
    fn every_opt_out_layer_suppresses_sending() {
        for (env, reason) in [
            (vec![("DO_NOT_TRACK", "1")], Reason::DoNotTrack),
            (vec![("RELATE_TELEMETRY", "0")], Reason::TelemetryOff),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let sender = MockSender::new(Reason::Sent);
            let doc = run(tmp.path(), &input(), &env_of(&env), Utc::now(), &sender);
            assert_skipped(&doc, reason, tmp.path(), &sender);
        }
    }

    /// One row per rejection rule: the report is refused, the named detector
    /// fired, and no send is attempted.
    #[test]
    fn every_validation_rule_rejects_before_a_send() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        authorize(tmp.path(), now, ENDPOINT);
        let env = env_of(&[]);
        let cases: [(&str, &str, policy::Detector); 9] = [
            (
                "bogus_category",
                "plain expected text",
                policy::Detector::InvalidInput,
            ),
            ("friction", "mail dev@example.com", policy::Detector::Email),
            (
                "friction",
                "see https://evil.example/x",
                policy::Detector::Url,
            ),
            (
                "friction",
                "connect db.example:5432",
                policy::Detector::HostPort,
            ),
            ("friction", "server 10.0.0.1", policy::Detector::IpAddress),
            ("friction", "read /etc/passwd", policy::Detector::FilePath),
            (
                "friction",
                "imported users.csv",
                policy::Detector::DataFilename,
            ),
            ("friction", "token=abc123", policy::Detector::Secret),
            (
                "friction",
                "MATCH (n:Person) RETURN n",
                policy::Detector::Cypher,
            ),
        ];
        for (category, observed, detector) in cases {
            let mut input = input();
            input.category = category.to_string();
            input.observed = observed.to_string();
            let sender = MockSender::new(Reason::Sent);
            let doc = run(tmp.path(), &input, &env, now, &sender);
            assert_eq!(
                doc.outcome(),
                Outcome::Rejected,
                "{category} / {observed:?} must be rejected"
            );
            assert_eq!(
                doc.violations[0].detector, detector,
                "{observed:?} names its detector"
            );
            assert_eq!(doc.violations.len(), 1, "{observed:?} has one violation");
            assert_eq!(
                sender.calls(),
                0,
                "{category} / {observed:?} must not reach the client"
            );
        }
        // Over-length text on either required field.
        let over_long = "x".repeat(MAX_TEXT_BYTES + 1);
        for input in [
            Input {
                expected: over_long.clone(),
                ..input()
            },
            Input {
                observed: over_long,
                ..input()
            },
        ] {
            let sender = MockSender::new(Reason::Sent);
            let doc = run(tmp.path(), &input, &env, now, &sender);
            assert_eq!(doc.reason, Reason::InvalidInput);
            assert_eq!(doc.violations[0].detector, policy::Detector::TooLong);
            assert_eq!(doc.violations.len(), 1);
            assert_eq!(sender.calls(), 0);
        }
        assert!(
            receipt::receipts_path(tmp.path()).exists(),
            "every rejection is receipted"
        );
    }

    #[test]
    fn a_redaction_hit_is_rejected_before_any_send() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        authorize(tmp.path(), now, ENDPOINT);
        let mut input = input();
        input.observed = "the error mentioned dev@example.com".to_string();
        let sender = MockSender::new(Reason::Sent);
        let doc = run(tmp.path(), &input, &env_of(&[]), now, &sender);
        assert_eq!(doc.outcome(), Outcome::Rejected);
        assert_eq!(doc.reason, Reason::PolicyViolation);
        assert_eq!(doc.violations[0].detector, policy::Detector::Email);
        assert_eq!(sender.calls(), 0);
        // The rejection receipt carries no payload text.
        let line = fs::read_to_string(receipt::receipts_path(tmp.path())).unwrap();
        assert!(line.contains("\"payload\":null"), "{line}");
    }

    #[test]
    fn an_authorized_clean_report_is_sent_once_and_receipted() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        authorize(tmp.path(), now, ENDPOINT);
        let mut input = input();
        input.error_code = Some("not_a_code".to_string());
        let sender = MockSender::new(Reason::Sent);
        let doc = run(tmp.path(), &input, &env_of(&[]), now, &sender);
        assert_eq!(doc.outcome(), Outcome::Sent);
        assert_eq!(doc.reason, Reason::Sent);
        assert_eq!(sender.calls(), 1);
        assert_eq!(doc.report_id(), Some(doc.request_id.as_str()));
        assert_eq!(doc.payload.as_ref().unwrap().expected, input.expected);
        assert_eq!(doc.dropped_fields, ["error_code"]);
        assert!(doc.receipt_path.is_some());
        let line = fs::read_to_string(receipt::receipts_path(tmp.path())).unwrap();
        assert!(line.contains("the info command"), "{line}");
    }

    #[test]
    fn a_send_failure_is_failed_and_keeps_the_payload_off_the_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        authorize(tmp.path(), now, ENDPOINT);
        let sender = MockSender::new(Reason::Unreachable);
        let doc = run(tmp.path(), &input(), &env_of(&[]), now, &sender);
        assert_eq!(doc.outcome(), Outcome::Failed);
        assert_eq!(doc.reason, Reason::Unreachable);
        assert_eq!(doc.report_id(), None);
        assert!(doc.payload.is_none());
        let line = fs::read_to_string(receipt::receipts_path(tmp.path())).unwrap();
        assert!(
            line.contains("\"payload\":{"),
            "send attempts record it: {line}"
        );
    }

    /// A sender that takes measurable time, so the elapsed window can be
    /// shown to cover the send itself.
    struct SlowSender;

    impl Sender for SlowSender {
        fn send(&self, _endpoint: &str, _payload: &Payload) -> Reason {
            std::thread::sleep(std::time::Duration::from_millis(20));
            Reason::Sent
        }
    }

    /// `elapsed_ms` is the submission's elapsed time, so the send attempt is
    /// inside the window — both on the outcome document and in the receipt.
    #[test]
    fn elapsed_ms_covers_the_send_in_the_outcome_and_the_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        authorize(tmp.path(), now, ENDPOINT);
        let report = input();
        let doc = run(tmp.path(), &report, &env_of(&[]), now, &SlowSender);
        assert_eq!(doc.outcome(), Outcome::Sent);
        assert!(
            doc.elapsed_ms >= 20,
            "the send itself must be measured: {} ms",
            doc.elapsed_ms
        );
        let line = fs::read_to_string(receipt::receipts_path(tmp.path())).unwrap();
        let receipt: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert!(
            receipt["elapsed_ms"].as_u64().unwrap() >= 20,
            "the receipt carries the send time: {line}"
        );
    }

    /// The mapping is pinned structurally: iterating [`Reason::ALL`] through a
    /// wildcard-free `match` makes a new variant a compile error, not a test
    /// that silently keeps passing.
    #[test]
    fn every_reason_maps_to_its_one_outcome() {
        for reason in Reason::ALL {
            let expected = match reason {
                Reason::Sent => Outcome::Sent,
                Reason::DoNotTrack
                | Reason::TelemetryOff
                | Reason::NotEnabled
                | Reason::AuthorizationExpired => Outcome::Skipped,
                Reason::InvalidInput | Reason::PolicyViolation | Reason::ServerRejected => {
                    Outcome::Rejected
                }
                Reason::ServerError | Reason::NotConfigured | Reason::Unreachable => {
                    Outcome::Failed
                }
                Reason::Timeout => Outcome::Unknown,
            };
            assert_eq!(reason.outcome(), expected, "{reason:?}");
        }
    }

    /// The destination is the endpoint recorded when the person enabled
    /// feedback: changing `RELATE_FEEDBACK_ENDPOINT` afterwards cannot
    /// divert the report elsewhere.
    #[test]
    fn submit_sends_to_the_endpoint_recorded_at_enable_time() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        authorize(tmp.path(), now, "http://127.0.0.1:9/recorded");
        let sender = MockSender::new(Reason::Sent);
        let doc = run(
            tmp.path(),
            &input(),
            &env_of(&[("RELATE_FEEDBACK_ENDPOINT", "http://127.0.0.1:9/diverted")]),
            now,
            &sender,
        );
        assert_eq!(doc.outcome(), Outcome::Sent);
        assert_eq!(
            sender.endpoint().as_deref(),
            Some("http://127.0.0.1:9/recorded")
        );
        let line = fs::read_to_string(receipt::receipts_path(tmp.path())).unwrap();
        assert!(line.contains("http://127.0.0.1:9/recorded"), "{line}");
        assert!(!line.contains("diverted"), "{line}");
    }
}
