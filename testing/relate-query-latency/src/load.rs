//! Build the graph from a data set directory with batched `UNWIND $rows`
//! statements (ADR-0243), the pattern `relate-load` uses:
//!
//! 1. the index profile's pre-load step (the `ID` indexes);
//! 2. per node file, `UNWIND $rows AS r CREATE (n:<Label>) SET n = r
//!    RETURN elementId(n)`, remembering each LDBC id's element id;
//! 3. per relationship file, endpoints seeked by `elementId()` and the
//!    relationship created with its property;
//! 4. the profile's post-load index steps;
//! 5. every label and type counted against its file's rows, and the totals
//!    against the published ones.

use crate::rows_param;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use relate::{Connection, Value};
use serde::Serialize;

use crate::schema::{csv_path, label_index, NodeFile, PropType, RelFile, NODE_FILES, REL_FILES};
use crate::workload::{index_script, IndexProfile, Step};
use crate::Result;

/// Rows per `UNWIND $rows` batch.
pub const BATCH_ROWS: usize = 5_000;

/// What a load did and how long each step took.
#[derive(Debug, Clone, Serialize)]
pub struct LoadReport {
    pub seconds: f64,
    /// Seconds per step: index steps by name, `nodes`, `relationships`,
    /// `count`.
    pub steps: BTreeMap<String, f64>,
    pub nodes: u64,
    pub relationships: u64,
    pub labels: BTreeMap<String, u64>,
    pub types: BTreeMap<String, u64>,
}

/// Load `dataset` through `conn` with `profile`'s indexes. `published`, when
/// given, is the (nodes, relationships) total the load must reproduce.
pub fn load(
    conn: &mut Connection,
    dataset: &Path,
    profile: IndexProfile,
    published: Option<(u64, u64)>,
) -> Result<LoadReport> {
    let t_all = Instant::now();
    let mut steps = BTreeMap::new();
    let script = index_script(profile)?;
    run_step(conn, &script.pre_load, &mut steps)?;

    let t = Instant::now();
    let mut ids: Vec<HashMap<i64, Arc<str>>> = vec![HashMap::new(); NODE_FILES.len()];
    let mut file_rows: BTreeMap<String, u64> = BTreeMap::new();
    for (i, nf) in NODE_FILES.iter().enumerate() {
        let n = load_nodes(conn, dataset, nf, &mut ids[i])?;
        *file_rows.entry(nf.label.to_string()).or_default() += n;
    }
    steps.insert("nodes".into(), t.elapsed().as_secs_f64());

    let t = Instant::now();
    let mut rel_rows: BTreeMap<String, u64> = BTreeMap::new();
    for rf in REL_FILES {
        let n = load_rels(conn, dataset, rf, &ids)?;
        *rel_rows.entry(rf.rel_type.to_string()).or_default() += n;
    }
    steps.insert("relationships".into(), t.elapsed().as_secs_f64());
    drop(ids);

    for step in &script.post_load {
        run_step(conn, step, &mut steps)?;
    }

    let t = Instant::now();
    let labels = count_each(conn, NODE_FILES.iter().map(|n| n.label), |l| {
        format!("MATCH (n:{l}) RETURN count(n) AS c")
    })?;
    let types = count_each(conn, REL_FILES.iter().map(|r| r.rel_type), |t| {
        format!("MATCH ()-[r:{t}]->() RETURN count(r) AS c")
    })?;
    steps.insert("count".into(), t.elapsed().as_secs_f64());
    if labels != file_rows {
        return Err(format!(
            "label counts {labels:?} differ from the node files' rows {file_rows:?}"
        ));
    }
    if types != rel_rows {
        return Err(format!(
            "relationship type counts {types:?} differ from the relationship files' rows {rel_rows:?}"
        ));
    }
    let nodes = labels.values().sum();
    let relationships = types.values().sum();
    if let Some((want_n, want_r)) = published {
        if (nodes, relationships) != (want_n, want_r) {
            return Err(format!(
                "loaded {nodes} nodes and {relationships} relationships; the published totals are \
                 {want_n} and {want_r}"
            ));
        }
    }
    Ok(LoadReport {
        seconds: t_all.elapsed().as_secs_f64(),
        steps,
        nodes,
        relationships,
        labels,
        types,
    })
}

fn run_step(conn: &mut Connection, step: &Step, steps: &mut BTreeMap<String, f64>) -> Result<()> {
    let t = Instant::now();
    for stmt in &step.statements {
        conn.execute(stmt)
            .map_err(|e| format!("{}: {stmt}: {e}", step.name))?;
    }
    steps.insert(step.name.clone(), t.elapsed().as_secs_f64());
    Ok(())
}

/// A reader over one data set file: `|`-separated, header row first.
fn csv_reader(path: &Path) -> Result<csv::Reader<std::fs::File>> {
    // The files hold no quotes (the examples harness checks), so quoting is
    // off: every byte between delimiters is the field.
    csv::ReaderBuilder::new()
        .delimiter(b'|')
        .quoting(false)
        .from_path(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))
}

fn parse_int(cell: &str, what: &str) -> Result<i64> {
    cell.parse()
        .map_err(|_| format!("{what}: {cell:?} is not an integer"))
}

/// A property cell as a value; `None` for an empty cell (left out, as
/// upstream's `SET n += row` skips nulls).
fn prop_value(cell: &str, ty: PropType, what: &str) -> Result<Option<Value>> {
    if cell.is_empty() {
        return Ok(None);
    }
    Ok(Some(match ty {
        PropType::Str => Value::String(cell.into()),
        PropType::Int => Value::Int(parse_int(cell, what)?),
    }))
}

fn load_nodes(
    conn: &mut Connection,
    dataset: &Path,
    nf: &NodeFile,
    ids: &mut HashMap<i64, Arc<str>>,
) -> Result<u64> {
    let path = csv_path(dataset, nf.file);
    let mut rdr = csv_reader(&path)?;
    let headers = rdr
        .headers()
        .map_err(|e| format!("{}: {e}", path.display()))?
        .clone();
    let col = |name: &str| {
        headers
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| format!("{}: no column {name:?}", path.display()))
    };
    let id_col = col("id")?;
    let prop_cols = nf
        .props
        .iter()
        .map(|(name, ty)| Ok((col(name)?, *name, *ty)))
        .collect::<Result<Vec<_>>>()?;
    let stmt = format!(
        "UNWIND $rows AS r CREATE (n:{}) SET n = r RETURN r.ID AS id, elementId(n) AS eid",
        nf.label
    );
    let mut batch = Vec::with_capacity(BATCH_ROWS);
    let mut total = 0u64;
    let mut flush = |batch: &mut Vec<Value>, ids: &mut HashMap<i64, Arc<str>>| -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let n = batch.len();
        let r = conn
            .query_with_params(&stmt, rows_param(std::mem::take(batch)))
            .map_err(|e| format!("{}: {e}", nf.label))?;
        if r.rows.len() != n {
            return Err(format!(
                "{}: a batch of {n} created {} nodes",
                nf.label,
                r.rows.len()
            ));
        }
        for row in r.rows {
            let [Value::Int(id), Value::String(eid)] = &row[..] else {
                return Err(format!("{}: unexpected row shape {row:?}", nf.label));
            };
            if ids.insert(*id, eid.clone()).is_some() {
                return Err(format!("{}: id {id} appears twice", nf.label));
            }
        }
        Ok(())
    };
    for rec in rdr.records() {
        let rec = rec.map_err(|e| format!("{}: {e}", path.display()))?;
        let what = |c: &str| {
            format!(
                "{} line {} {c}",
                path.display(),
                rec.position().map_or(0, |p| p.line())
            )
        };
        let mut props = BTreeMap::new();
        props.insert(
            "ID".to_string(),
            Value::Int(parse_int(&rec[id_col], &what("id"))?),
        );
        for (i, name, ty) in &prop_cols {
            if let Some(v) = prop_value(&rec[*i], *ty, &what(name))? {
                props.insert(name.to_string(), v);
            }
        }
        batch.push(Value::Map(props.into()));
        total += 1;
        if batch.len() == BATCH_ROWS {
            flush(&mut batch, ids)?;
        }
    }
    flush(&mut batch, ids)?;
    Ok(total)
}

fn load_rels(
    conn: &mut Connection,
    dataset: &Path,
    rf: &RelFile,
    ids: &[HashMap<i64, Arc<str>>],
) -> Result<u64> {
    let path = csv_path(dataset, rf.file);
    let mut rdr = csv_reader(&path)?;
    let from = &ids[label_index(rf.from).ok_or("unknown from label")?];
    let to = &ids[label_index(rf.to).ok_or("unknown to label")?];
    let set = match rf.prop {
        Some(_) => " SET x = r.p",
        None => "",
    };
    let stmt = format!(
        "UNWIND $rows AS r MATCH (a) WHERE elementId(a) = r.s MATCH (b) WHERE elementId(b) = r.e \
         CREATE (a)-[x:{}]->(b){set} RETURN count(*) AS c",
        rf.rel_type
    );
    let mut batch = Vec::with_capacity(BATCH_ROWS);
    let mut total = 0u64;
    let mut flush = |batch: &mut Vec<Value>| -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let n = batch.len() as i64;
        let r = conn
            .query_with_params(&stmt, rows_param(std::mem::take(batch)))
            .map_err(|e| format!("{}: {e}", rf.rel_type))?;
        let created = r
            .rows
            .first()
            .and_then(|row| row.first())
            .and_then(Value::as_int);
        if created != Some(n) {
            return Err(format!(
                "{}: a batch of {n} created {created:?}",
                rf.rel_type
            ));
        }
        Ok(())
    };
    for rec in rdr.records() {
        let rec = rec.map_err(|e| format!("{}: {e}", path.display()))?;
        let line = rec.position().map_or(0, |p| p.line());
        let what = |c: &str| format!("{} line {line} {c}", path.display());
        let endpoint = |map: &HashMap<i64, Arc<str>>, cell: &str, label: &str| -> Result<Value> {
            let id = parse_int(cell, &what(label))?;
            map.get(&id)
                .map(|eid| Value::String(eid.clone()))
                .ok_or_else(|| format!("{}: no {label} with id {id}", what("endpoint")))
        };
        let mut row = BTreeMap::new();
        row.insert("s".to_string(), endpoint(from, &rec[0], rf.from)?);
        row.insert("e".to_string(), endpoint(to, &rec[1], rf.to)?);
        if let Some((name, ty)) = rf.prop {
            let cell = rec.get(2).ok_or_else(|| what("has no third column"))?;
            let mut p = BTreeMap::new();
            if let Some(v) = prop_value(cell, ty, &what(name))? {
                p.insert(name.to_string(), v);
            }
            row.insert("p".to_string(), Value::Map(p.into()));
        }
        batch.push(Value::Map(row.into()));
        total += 1;
        if batch.len() == BATCH_ROWS {
            flush(&mut batch)?;
        }
    }
    flush(&mut batch)?;
    Ok(total)
}

fn count_each<'a>(
    conn: &mut Connection,
    names: impl Iterator<Item = &'a str>,
    stmt: impl Fn(&str) -> String,
) -> Result<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    for name in names {
        let r = conn
            .query(&stmt(name))
            .map_err(|e| format!("counting {name}: {e}"))?;
        let c = r
            .rows
            .first()
            .and_then(|row| row.first())
            .and_then(Value::as_int)
            .ok_or_else(|| format!("counting {name}: no count"))?;
        *out.entry(name.to_string()).or_default() += c as u64;
    }
    Ok(out)
}
