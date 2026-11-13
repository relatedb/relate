//! Shared scaffolding for the dataset binaries (`benchmark-harness`,
//! `codec-prototype`): `--db` / `--graph-examples-dir` store resolution and
//! synthetic shape building.

use std::path::PathBuf;

use crate::shapes::{self, GraphShape, IndexCounts, ShapeScale};

/// One dataset to measure: an existing store opened read-only.
#[derive(Debug, Clone)]
pub struct StoreDataset {
    pub name: String,
    pub path: PathBuf,
}

/// Parse one `--db NAME=PATH` value; both parts must be non-empty.
pub fn parse_store(raw: &str) -> Result<StoreDataset, String> {
    let Some((name, path)) = raw.split_once('=') else {
        return Err("expected NAME=PATH".into());
    };
    if name.is_empty() || path.is_empty() {
        return Err("expected NAME=PATH with both parts non-empty".into());
    }
    Ok(StoreDataset {
        name: name.to_string(),
        path: PathBuf::from(path),
    })
}

/// Every subdirectory of `dir` that holds a `manifest.json` is a pulled
/// relate store.
pub fn scan_graph_examples(dir: &std::path::Path) -> Result<Vec<StoreDataset>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut stores = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() && path.join("manifest.json").is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            stores.push(StoreDataset { name, path });
        }
    }
    stores.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(stores)
}

/// `--db` values plus every store found under the `--graph-examples-dir`
/// directories, in flag order.
pub fn resolve_stores(db: &[StoreDataset], dirs: &[PathBuf]) -> Result<Vec<StoreDataset>, String> {
    let mut stores = db.to_vec();
    for dir in dirs {
        let found = scan_graph_examples(dir).map_err(|e| format!("--graph-examples-dir {e}"))?;
        stores.extend(found);
    }
    Ok(stores)
}

/// `--shape-logical-mb` in bytes, overflow-checked.
pub fn shape_logical_bytes(mb: u64) -> Result<u64, String> {
    mb.checked_mul(1024 * 1024)
        .ok_or_else(|| "shape-logical-mb overflows".to_string())
}

/// Build one synthetic memory shape in an in-memory database.
pub fn build_shape(
    shape: GraphShape,
    logical_bytes: u64,
    attributes: u32,
) -> Result<relate::Database, String> {
    let db = relate::Database::open(":memory:").map_err(|e| e.to_string())?;
    let mut conn = db.connect().map_err(|e| e.to_string())?;
    let statements = shapes::statements(
        shape,
        ShapeScale::Large { logical_bytes },
        IndexCounts::NONE,
        attributes,
    )?;
    for statement in &statements {
        conn.query(statement)
            .map_err(|e| format!("{statement}: {e}"))?;
    }
    Ok(db)
}

/// `SELECT` a single integer (`AS n`) — the one scalar-count helper the
/// dataset harnesses (mixed workload, bulk ladder, checkpoint) share.
pub fn scalar(conn: &mut relate::Connection, cypher: &str) -> relate::Result<i64> {
    let r = conn.query(cypher)?;
    match r.rows.as_slice() {
        [row] => row.first().and_then(relate::Value::as_int).ok_or_else(|| {
            relate::Error::semantic(format!("{cypher}: the single row is not an integer"))
        }),
        rows => Err(relate::Error::semantic(format!(
            "{cypher}: expected one row with one integer, got {} rows",
            rows.len()
        ))),
    }
}

/// CPU/OS line recorded in the report: `machdep.cpu.brand_string` on macOS,
/// the first `model name` of `/proc/cpuinfo` on Linux.
pub fn hardware_note() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let cpu = if cfg!(target_os = "macos") {
        std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|l| l.strip_prefix("model name"))
                    .and_then(|rest| rest.split(':').nth(1))
                    .map(|name| name.trim().to_string())
            })
    };
    match cpu {
        Some(cpu) if !cpu.is_empty() => format!("{cpu} ({os}/{arch})"),
        _ => format!("{os}/{arch}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_note_mentions_the_os() {
        assert!(hardware_note().contains(std::env::consts::OS));
    }
}
