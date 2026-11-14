//! The local feedback receipt log (ADR-0588 §1): one JSON object per line
//! under the caller's config directory, so a person can see what was sent.
//!
//! A receipt exists for a locally rejected report and for every send
//! attempt; an authorization or opt-out skip returns before this module is
//! reached. A rejected report's receipt records `payload: null` — rejected
//! text is never written — while a send attempt records the full payload it
//! transmitted. The caller maps a write failure to `receipt_path: null` and
//! carries on: a receipt never fails the command.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde_json::json;

use crate::outcome::Reason;
use crate::report::Payload;

/// The receipt log's filename inside the config directory.
pub const RECEIPTS_FILE: &str = "feedback-receipts.jsonl";

/// One appended receipt line. The outcome is derived from `reason` in
/// [`append`], so a line cannot carry a pair the mapping does not produce.
pub struct Receipt<'a> {
    pub reason: Reason,
    pub request_id: &'a str,
    pub report_id: Option<&'a str>,
    pub destination: Option<&'a str>,
    pub elapsed_ms: u64,
    /// The transmitted payload for a send attempt; `None` for a local
    /// rejection.
    pub payload: Option<&'a Payload>,
}

/// Path of the receipt log inside a config directory.
pub fn receipts_path(dir: &Path) -> PathBuf {
    dir.join(RECEIPTS_FILE)
}

/// Append one receipt and return the log path. A permission or write failure
/// is returned as an error: the log holds report text, so a directory or file
/// that cannot be made owner-only fails the append instead of weakening the
/// guarantee.
pub fn append(dir: &Path, receipt: &Receipt<'_>) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The log holds report text: owner-only directory, owner-only file.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let path = receipts_path(dir);
    let line = json!({
        "time": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "outcome": receipt.reason.outcome(),
        "reason": receipt.reason,
        "request_id": receipt.request_id,
        "report_id": receipt.report_id,
        "destination": receipt.destination,
        "elapsed_ms": receipt.elapsed_ms,
        "payload": receipt.payload,
    });
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `mode` applies only at creation: a pre-existing log with looser bits
        // (another tool, a restored backup) is tightened before it is used.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let mut bytes = serde_json::to_vec(&line).expect("a receipt line serializes");
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::payload;
    use serde_json::Value;

    fn lines(dir: &Path) -> Vec<Value> {
        fs::read_to_string(receipts_path(dir))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn a_rejection_receipt_never_stores_report_text() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let path = append(
            dir,
            &Receipt {
                reason: Reason::PolicyViolation,
                request_id: "id-1",
                report_id: None,
                destination: Some("https://api.mixpanel.com/track"),
                elapsed_ms: 3,
                payload: None,
            },
        )
        .expect("receipt written");
        assert_eq!(path, receipts_path(dir));
        let line = &lines(dir)[0];
        assert_eq!(line["outcome"], "rejected");
        assert_eq!(line["reason"], "policy_violation");
        assert!(line["payload"].is_null());
        assert!(line["time"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn a_send_receipt_records_the_payload_and_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let payload = payload();
        for _ in 0..2 {
            append(
                dir,
                &Receipt {
                    reason: Reason::Sent,
                    request_id: &payload.request_id,
                    report_id: Some(&payload.request_id),
                    destination: Some("http://127.0.0.1:9/track"),
                    elapsed_ms: 7,
                    payload: Some(&payload),
                },
            )
            .unwrap();
        }
        let lines = lines(dir);
        assert_eq!(lines.len(), 2, "one JSON object per line");
        assert_eq!(lines[0]["payload"]["expected"], "a");
        assert_eq!(
            lines[1]["report_id"],
            "123e4567-e89b-42d3-a456-426614174000"
        );
    }

    #[cfg(unix)]
    #[test]
    fn receipts_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("relate");
        append(
            &dir,
            &Receipt {
                reason: Reason::Sent,
                request_id: "id",
                report_id: Some("id"),
                destination: None,
                elapsed_ms: 0,
                payload: Some(&payload()),
            },
        )
        .unwrap();
        let file_mode = fs::metadata(receipts_path(&dir))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    /// The creation mode only covers a new file: a pre-existing loose log and
    /// directory are tightened before the append uses them.
    #[cfg(unix)]
    #[test]
    fn a_pre_existing_loose_log_and_directory_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("relate");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(receipts_path(&dir), "").unwrap();
        fs::set_permissions(receipts_path(&dir), fs::Permissions::from_mode(0o644)).unwrap();
        append(
            &dir,
            &Receipt {
                reason: Reason::Sent,
                request_id: "id",
                report_id: Some("id"),
                destination: None,
                elapsed_ms: 0,
                payload: None,
            },
        )
        .unwrap();
        let file_mode = fs::metadata(receipts_path(&dir))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }
}
