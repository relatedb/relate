//! One durable commit. The JSON field names match the historical log record,
//! so a file written before this type moved still decodes.

use serde::{Deserialize, Serialize};

use crate::redo::RedoOp;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitRecord {
    pub lsn: u64,
    pub commit_ts: u64,
    pub ops: Vec<RedoOp>,
}

/// The record's serialized shape, spelled once beside the type whose field
/// names it mirrors. The WAL's encode-once path (ADR-0558) writes the header
/// by hand around an already-serialized `ops` array; these are the
/// spellings it consults, and the test below pins them to what serde emits.
impl CommitRecord {
    /// The opening brace and the `lsn` key.
    pub const LSN_KEY: &'static str = "{\"lsn\":";
    /// The separator and the `commit_ts` key.
    pub const COMMIT_TS_KEY: &'static str = ",\"commit_ts\":";
    /// The separator and the `ops` key; the array follows.
    pub const OPS_KEY: &'static str = ",\"ops\":";
    /// Decimal digits of `u64::MAX`: the widest `lsn` or `commit_ts`.
    pub const U64_DECIMAL_MAX: usize = 20;
    /// The widest header the record can carry: the three key spellings and
    /// two maximum-width unsigned decimals.
    pub const HEADER_MAX: usize = Self::LSN_KEY.len()
        + Self::COMMIT_TS_KEY.len()
        + Self::OPS_KEY.len()
        + 2 * Self::U64_DECIMAL_MAX;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_spellings_match_the_serialized_field_names() {
        let record = CommitRecord {
            lsn: u64::MAX,
            commit_ts: u64::MAX,
            ops: Vec::new(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let header = format!(
            "{}{}{}{}{}",
            CommitRecord::LSN_KEY,
            u64::MAX,
            CommitRecord::COMMIT_TS_KEY,
            u64::MAX,
            CommitRecord::OPS_KEY
        );
        assert_eq!(json, format!("{header}[]}}"));
        assert_eq!(header.len(), CommitRecord::HEADER_MAX);
        assert_eq!(u64::MAX.to_string().len(), CommitRecord::U64_DECIMAL_MAX);
    }
}
