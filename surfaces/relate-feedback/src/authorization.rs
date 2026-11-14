//! The feedback authorization record (ADR-0588 §1): the person's consent to
//! free-text reports — the destination it was granted for and its absolute
//! deadline — persisted under the `feedback` object in `config.json`.

use std::path::Path;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::state;

/// The authorization an accepted report grants, in days.
pub const DEFAULT_AUTHORIZATION_DAYS: i64 = 30;

/// The longest authorization a person may grant: a farther-ahead expiry is
/// treated as no authorization rather than trusted.
pub const MAX_AUTHORIZATION_DAYS: i64 = 90;

/// The persisted feedback consent record. Every field is optional: absence
/// means "not granted".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackConsent {
    /// The destination the person consented to; the submit pipeline sends
    /// there, never to an endpoint the environment names later.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// RFC3339 UTC instant; the person's authorization runs until then.
    #[serde(rename = "authorized-until", skip_serializing_if = "Option::is_none")]
    pub authorized_until: Option<String>,
    /// The person answered the first-run ask with no: recorded so the ask
    /// never repeats. A later `enable` clears it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub declined: bool,
}

/// The serde skip predicate for [`FeedbackConsent::declined`].
fn is_false(value: &bool) -> bool {
    !*value
}

/// What the record says about sending a report right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authorization {
    /// A live authorization within the allowed horizon, carrying the
    /// destination and the deadline it was proved against — the caller prints
    /// that deadline, never the raw string.
    Authorized {
        until: DateTime<Utc>,
        endpoint: String,
    },
    /// No authorization was ever granted (or the stored value is unusable,
    /// including one farther ahead than [`MAX_AUTHORIZATION_DAYS`] or one
    /// recorded without a destination).
    Missing,
    /// An authorization existed but has passed.
    Expired,
}

impl FeedbackConsent {
    /// Load the record from `config.json`; a missing, unreadable, corrupt or
    /// partial file yields the absent record (and `save` refuses to
    /// overwrite one it could not parse).
    pub fn load(dir: &Path) -> Self {
        let Ok(config) = state::load_config(dir) else {
            return Self::default();
        };
        let feedback = config.get("feedback").cloned().unwrap_or_default();
        serde_json::from_value(feedback).unwrap_or_default()
    }

    /// Persist the record under the `feedback` key, keeping every other
    /// `config.json` key.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        // A record of two optional strings always serializes: a failure is an
        // invariant break, not a value error.
        let value = serde_json::to_value(self).expect("a consent record serializes");
        state::update_config(dir, move |config| {
            config.insert("feedback".to_string(), value);
        })
    }

    /// Store an accepted authorization: the endpoint the person consented
    /// to, and an expiry [`DEFAULT_AUTHORIZATION_DAYS`] ahead — the one
    /// window the grant offers, so the record's writer and its reader state
    /// the same horizon. Returns the deadline it recorded, so the caller
    /// prints the value rather than re-reading the raw field.
    pub fn authorize(&mut self, now: DateTime<Utc>, endpoint: String) -> DateTime<Utc> {
        self.endpoint = Some(endpoint);
        self.declined = false;
        let until = now + Duration::days(DEFAULT_AUTHORIZATION_DAYS);
        self.authorized_until = Some(until.to_rfc3339_opts(SecondsFormat::Secs, true));
        until
    }

    /// The record's verdict at `now`.
    pub fn authorization(&self, now: DateTime<Utc>) -> Authorization {
        let Some(raw) = self.authorized_until.as_deref() else {
            return Authorization::Missing;
        };
        let Ok(until) = DateTime::parse_from_rfc3339(raw) else {
            return Authorization::Missing;
        };
        let until = until.with_timezone(&Utc);
        if until > now + Duration::days(MAX_AUTHORIZATION_DAYS) {
            return Authorization::Missing;
        }
        if until < now {
            return Authorization::Expired;
        }
        // A record with no destination authorizes nothing: every submit would
        // only be `not_configured`.
        let Some(endpoint) = self.endpoint.clone() else {
            return Authorization::Missing;
        };
        Authorization::Authorized { until, endpoint }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENDPOINT: &str = "https://api.mixpanel.com/track";

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn feedback_consent_roundtrips_through_config_json() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut record = FeedbackConsent::default();
        record.authorize(now(), ENDPOINT.to_string());
        record.save(dir).unwrap();
        assert_eq!(FeedbackConsent::load(dir), record);
        // A telemetry write in the same file leaves the record alone.
        state::save_config_telemetry(dir, false).unwrap();
        assert_eq!(FeedbackConsent::load(dir), record);
    }

    /// The first-run ask's no is a recorded answer: it round-trips, it is
    /// not an authorization, and a later grant clears it.
    #[test]
    fn a_decline_roundtrips_and_a_grant_clears_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let declined = FeedbackConsent {
            declined: true,
            ..FeedbackConsent::default()
        };
        declined.save(dir).unwrap();
        assert_eq!(FeedbackConsent::load(dir), declined);
        assert_eq!(declined.authorization(now()), Authorization::Missing);

        let mut record = FeedbackConsent::load(dir);
        record.authorize(now(), ENDPOINT.to_string());
        assert!(!record.declined, "a grant clears the decline");
        record.save(dir).unwrap();
        assert!(!FeedbackConsent::load(dir).declined);
    }

    #[test]
    fn a_missing_or_corrupt_record_is_not_authorized() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(
            FeedbackConsent::load(dir).authorization(now()),
            Authorization::Missing
        );
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("config.json"), "{ not json").unwrap();
        assert_eq!(
            FeedbackConsent::load(dir).authorization(now()),
            Authorization::Missing
        );
        // A file that cannot be parsed is refused, not overwritten from an
        // empty map: the save fails and the record stays unauthorized.
        let record = FeedbackConsent {
            endpoint: Some(ENDPOINT.to_string()),
            authorized_until: Some("not a date".to_string()),
            ..FeedbackConsent::default()
        };
        assert!(record.save(dir).is_err());
        assert_eq!(
            FeedbackConsent::load(dir).authorization(now()),
            Authorization::Missing
        );
    }

    #[test]
    fn an_unusable_stored_deadline_is_no_authorization() {
        let tmp = tempfile::tempdir().unwrap();
        let record = FeedbackConsent {
            endpoint: Some(ENDPOINT.to_string()),
            authorized_until: Some("not a date".to_string()),
            ..FeedbackConsent::default()
        };
        record.save(tmp.path()).unwrap();
        assert_eq!(record.authorization(now()), Authorization::Missing);
    }

    #[test]
    fn an_authorization_without_a_destination_is_no_authorization() {
        let record = FeedbackConsent {
            authorized_until: Some(
                (now() + Duration::days(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
            ),
            ..FeedbackConsent::default()
        };
        assert_eq!(record.authorization(now()), Authorization::Missing);
    }

    #[test]
    fn authorization_expires() {
        let mut record = FeedbackConsent::default();
        let until = record.authorize(now(), ENDPOINT.to_string());
        assert_eq!(
            record.authorization(now()),
            Authorization::Authorized {
                until,
                endpoint: ENDPOINT.to_string()
            },
            "the verdict carries the destination and the deadline it proved"
        );
        let later = now() + Duration::days(DEFAULT_AUTHORIZATION_DAYS + 1);
        assert_eq!(record.authorization(later), Authorization::Expired);
    }

    /// A hand-edited record is held to the named horizon exactly: the limit
    /// itself authorizes, one day past it does not.
    #[test]
    fn an_expiry_beyond_the_allowed_horizon_is_no_authorization() {
        let deadline = |days: i64| FeedbackConsent {
            endpoint: Some(ENDPOINT.to_string()),
            authorized_until: Some(
                (now() + Duration::days(days)).to_rfc3339_opts(SecondsFormat::Secs, true),
            ),
            ..FeedbackConsent::default()
        };
        assert_eq!(
            deadline(MAX_AUTHORIZATION_DAYS).authorization(now()),
            Authorization::Authorized {
                until: now() + Duration::days(MAX_AUTHORIZATION_DAYS),
                endpoint: ENDPOINT.to_string(),
            },
            "the named limit is the last accepted deadline"
        );
        assert_eq!(
            deadline(MAX_AUTHORIZATION_DAYS + 1).authorization(now()),
            Authorization::Missing
        );
    }

    #[test]
    fn accepting_again_renews_the_authorization() {
        let mut record = FeedbackConsent::default();
        record.authorize(now(), ENDPOINT.to_string());
        let later = now() + Duration::days(DEFAULT_AUTHORIZATION_DAYS + 1);
        assert_eq!(record.authorization(later), Authorization::Expired);
        let until = record.authorize(later, ENDPOINT.to_string());
        assert_eq!(
            record.authorization(later),
            Authorization::Authorized {
                until,
                endpoint: ENDPOINT.to_string()
            }
        );
    }
}
