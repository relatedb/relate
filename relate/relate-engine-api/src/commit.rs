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
