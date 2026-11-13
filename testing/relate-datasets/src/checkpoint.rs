//! Load the fraud graph, checkpoint, and report logical / RSS / snapshot sizes.

use std::path::PathBuf;
use std::time::Duration;

use relate::{Database, OpenOptions, Result};
use relate_test_support::sampling::process_rss_bytes;
use serde::{Deserialize, Serialize};

use crate::fraud::{generate, FraudConfig};
use crate::harness::scalar;
use crate::load::load_fraud;

#[derive(Debug, Clone)]
pub struct CheckpointConfig {
    pub uri: String,
    pub scale: f64,
    pub graph_seed: u64,
    pub fanout_k: usize,
    pub cycle_len: usize,
    pub wal_dir: Option<PathBuf>,
    pub reset: bool,
    pub logical_graph_size_limit: Option<u64>,
    pub out: Option<PathBuf>,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            uri: "./target/fraud-checkpoint".into(),
            scale: 0.05,
            graph_seed: 42,
            fanout_k: 12,
            cycle_len: 4,
            wal_dir: None,
            reset: false,
            logical_graph_size_limit: None,
            out: None,
        }
    }
}

impl CheckpointConfig {
    pub fn ci() -> Self {
        Self {
            scale: 0.05,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointReport {
    pub name: String,
    pub uri: String,
    pub scale: f64,
    pub nodes: i64,
    pub relationships: i64,
    pub accounts: i64,
    pub transfers: i64,
    pub logical_used_bytes: u64,
    pub rss_bytes: u64,
    pub data_file_bytes: u64,
}

pub fn run(cfg: &CheckpointConfig) -> Result<CheckpointReport> {
    crate::memory::require_sampling_available()?;
    let graph = generate(&FraudConfig {
        seed: cfg.graph_seed,
        scale: cfg.scale,
        fanout_k: cfg.fanout_k,
        cycle_len: cfg.cycle_len,
    });
    if cfg.reset {
        relate_storage::reset_uri(&cfg.uri, cfg.wal_dir.as_deref())?;
    }
    let db = Database::open_with(
        &cfg.uri,
        OpenOptions {
            wal_dir: cfg.wal_dir.clone(),
            checkpoint_interval: Some(Duration::ZERO),
            logical_graph_size_limit: cfg.logical_graph_size_limit,
            ..OpenOptions::default()
        },
    )?;
    let mut conn = db.connect()?;
    load_fraud(&mut conn, &graph)?;
    db.checkpoint()?;

    let (logical_used_bytes, _) = db.memory_usage()?;
    let rss_bytes = process_rss_bytes().expect("RSS sampling availability is checked at run start");
    let data_file_bytes = db.snapshot_data_bytes()?;

    let nodes = scalar(&mut conn, "MATCH (n) RETURN count(n) AS n")?;
    let relationships = scalar(&mut conn, "MATCH ()-[r]->() RETURN count(r) AS n")?;
    let accounts = scalar(&mut conn, "MATCH (a:Account) RETURN count(a) AS n")?;
    let transfers = scalar(&mut conn, "MATCH ()-[t:TRANSFER]->() RETURN count(t) AS n")?;

    let report = CheckpointReport {
        name: "fraud-checkpoint".into(),
        uri: cfg.uri.clone(),
        scale: cfg.scale,
        nodes,
        relationships,
        accounts,
        transfers,
        logical_used_bytes,
        rss_bytes,
        data_file_bytes,
    };
    if let Some(path) = &cfg.out {
        crate::report::write_report(path, &report).map_err(relate::Error::storage)?;
    }
    Ok(report)
}
