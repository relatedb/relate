//! The workload-JSON side of the corpus file format: the file-name
//! conventions and entry parsers every lane's side-table loader reads
//! through — a lane's params table, the pathdb seed tables, an oracle
//! file — the rows-oracle machinery the rows-oracle lanes (ukrailway,
//! agg-stress and equi-join) share whole (the [`Oracle`] table type,
//! its one loader
//! ([`load_rows_oracle`]) and its one build-time contract
//! ([`validate_rows_oracle`])) and the parameter-table machinery the
//! snb-bi lane uses whole ([`ParamsTable`], its one
//! loader, [`evaluate_table`]) — so the lanes' file formats cannot drift
//! apart. The query-file side (the `// status:` marker and its
//! classification) stays in the corpus root beside
//! [`load_queries`](super::load_queries), and the report-side shared
//! vocabulary (the graph-benchmark-ldbc store origin's provenance line) in the `report`
//! submodule.
//!
//! # Visibility ladder
//!
//! Items here are `pub(super)` — from this module, whose parent is
//! `corpus`, that means visible exactly inside the corpus tree: the
//! narrowest visibility the sibling lanes (fraud, pathdb with its
//! `seeds` submodule, snbbi, ukrailway, aggstress) compile with. Nothing
//! here is part of the crate's public surface; that stays the corpus
//! root's own vocabulary and re-exports (the root module's ladder note).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{Classification, CorpusQuery};
use crate::workload::ScaleFactor;
use crate::{read, Result};

/// The query entries of a workload's JSON document: every top-level key
/// but the `_`-prefixed provenance blocks — the one home of the
/// metadata-key rule, consulted by every loader that walks such a
/// document through [`read_query_entries`] (a lane's params table, an
/// oracle file).
fn query_entries_of(
    doc: BTreeMap<String, serde_json::Value>,
) -> impl Iterator<Item = (String, serde_json::Value)> {
    doc.into_iter().filter(|(id, _)| !id.starts_with('_'))
}

/// A workload's per-scale-factor side table, `<stem>-sf<SF>.json` — the
/// one home of the per-scale file-name convention every lane's loader
/// reads through (the pathdb seed tables' and snb-bi parameter table's
/// `params` files, the rows-oracle workloads' `oracle` files): a side
/// table whose rows are store-specific is recorded per scale factor, so
/// a store at another scale factor resolves its own file — and a scale
/// factor with no file fails loudly at load, never silently reusing
/// another scale's rows.
pub(super) fn scale_file(dir: &Path, stem: &str, sf: ScaleFactor) -> PathBuf {
    dir.join(format!("{stem}-sf{}.json", sf.as_str()))
}

/// A workload's per-scale-factor params file, `params-sf<SF>.json` —
/// [`scale_file`] at the `params` stem.
pub(super) fn params_file(dir: &Path, sf: ScaleFactor) -> PathBuf {
    scale_file(dir, "params", sf)
}

/// Read a workload JSON file and parse it, the parse error naming the
/// path — the one spelling of "read a file of JSON" every workload-file
/// JSON loader in the corpus tree shares (a lane's params table, the
/// seed tables, the fraud oracle). A loader that needs the raw text for
/// more than the parse reads it itself; only the read-and-parse pairs
/// go through here.
pub(super) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = read(path)?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// A workload JSON document's query entries, read and parsed in one
/// step — `read_json` + `query_entries_of`, the pair every loader that
/// walks a workload JSON document's entries starts with (a lane's
/// params table, the fraud oracle).
pub(super) fn read_query_entries(
    path: &Path,
) -> Result<impl Iterator<Item = (String, serde_json::Value)>> {
    let doc: BTreeMap<String, serde_json::Value> = read_json(path)?;
    Ok(query_entries_of(doc))
}

/// One `rows` check of a workload's oracle document: the expected rows
/// and order sensitivity, in the `expected-sf<SF>.json` shape — the one
/// parser the CORPUS lanes' rows checks share (the fraud workload's
/// `oracle.json` `rows` entries, the ukrailway workload's `oracle.json`),
/// so the two lanes' file formats cannot drift apart. The graph-benchmark-ldbc lane's
/// expected-answers parser (in `workload.rs`) is a separate, sealed
/// implementation; consolidating the two is tracked in LABS-747.
pub(super) fn parse_rows_check(
    path: &Path,
    id: &str,
    v: &serde_json::Value,
) -> Result<crate::answers::Expected> {
    let rows = v["rows"]
        .as_array()
        .ok_or_else(|| format!("{}: {id} rows is not a list", path.display()))?
        .iter()
        .map(|r| {
            r.as_object()
                .cloned()
                .ok_or_else(|| format!("{}: {id} a row is not an object", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let order_sensitive = v["order_sensitive"]
        .as_bool()
        .ok_or_else(|| format!("{}: {id} order_sensitive is not a bool", path.display()))?;
    Ok(crate::answers::Expected {
        rows,
        order_sensitive,
    })
}

/// One rows-oracle entry: the exact answer a run checks the executed
/// result with, plus the sha256 of the query text the answer was
/// generated for.
#[derive(Debug, Clone)]
pub(super) struct OracleEntry {
    pub(super) expected: crate::answers::Expected,
    /// The sha256 of the query text the answer was generated for —
    /// mandatory in the rows-oracle format, so the freshness guard can
    /// never be silently absent.
    pub(super) query_sha256: String,
}

/// A rows-oracle lane's oracle table: per query id, the entry a run
/// checks the executed result with — the ONE table type the rows-oracle
/// lanes (ukrailway's, agg-stress's and equi-join's verified rows)
/// hold, so the loader, the validation and the
/// route all speak one type, never a lane-local re-spelling of it.
pub(super) type Oracle = BTreeMap<String, OracleEntry>;

/// A rows-oracle lane's oracle file at `path`: every top-level key but
/// the `_`-prefixed provenance blocks is a query id whose entry is one
/// rows check — the ONE loader every rows-oracle lane reads through, so
/// the lanes' loads cannot drift from each other or from the format
/// [`parse_rows_check`] pins. A file that does not exist fails loudly
/// here, naming the path.
pub(super) fn load_rows_oracle(path: &Path) -> Result<Oracle> {
    let mut oracle = Oracle::new();
    for (id, v) in read_query_entries(path)? {
        let expected = parse_rows_check(path, &id, &v)?;
        // The text-freshness fact is part of the rows-oracle format,
        // mandatory here: an oracle that cannot say which query text it
        // answers is refused at load (the comparison against the
        // committed file's hash is `validate_rows_oracle`'s, where the
        // entry and its query are already paired).
        let query_sha256 = v["query_sha256"]
            .as_str()
            .ok_or_else(|| {
                format!(
                    "{}: {id} has no query_sha256 — regenerate the oracle (every rows-oracle \
                     entry records the sha256 of the text it was generated for)",
                    path.display()
                )
            })?
            .to_string();
        oracle.insert(
            id,
            OracleEntry {
                expected,
                query_sha256,
            },
        );
    }
    Ok(oracle)
}

/// One row of a lane's oracle table, looked up by query id — the ONE
/// lookup and miss text every oracle-owning lane resolves through (the
/// fraud lane's check rows, the ukrailway lane's verified rows, the
/// agg-stress lane's verified rows), shared by the build-time
/// validation and every route, so the lanes can never drift apart.
/// Unified here from the two lanes' bespoke spellings, whose miss texts
/// had drifted (`no oracle entry` vs `no oracle row`): the one wording
/// names the id and the oracle file it is missing from.
pub(super) fn row_of<'a, V>(
    table: &'a BTreeMap<String, V>,
    id: &str,
    oracle: &Path,
) -> Result<&'a V> {
    table
        .get(id)
        .ok_or_else(|| format!("{id}: no oracle row in {}", oracle.display()))
}

/// A side-table id naming no query file is stale entry data riding
/// along unvalidated — the one refusal every lane's build check runs,
/// whichever side table it walks (a params entry, an oracle entry), the
/// lane naming its table's noun. The reverse direction — a query file
/// its table does not cover — is each lane's own policy (plan it, or
/// refuse it), never this one.
pub(super) fn refuse_stale_ids<'a>(
    ids: impl Iterator<Item = &'a str>,
    queries: &[CorpusQuery],
    what: &str,
) -> Result<()> {
    for id in ids {
        if !queries.iter().any(|q| q.id == id) {
            return Err(format!("{id}: {what} with no query file"));
        }
    }
    Ok(())
}

/// A rows oracle's contract with its workload's query files, both
/// directions, stated ONCE for every rows-oracle lane (ukrailway,
/// agg-stress): every oracle row names a query file (the shared
/// stale-id refusal), every executable (`ok`) entry carries a row (its
/// check is machine-readable, never prose — the row resolves through the
/// one lookup, [`row_of`]), and no kept or pin entry carries one (neither
/// ever executes, so a row for it is stale data riding along unvalidated
/// — the presence probe is the table's own `contains_key`, never the
/// lookup's miss error constructed and discarded).
pub(super) fn validate_rows_oracle(
    oracle: &Oracle,
    queries: &[CorpusQuery],
    path: &Path,
) -> Result<()> {
    refuse_stale_ids(
        oracle.keys().map(String::as_str),
        queries,
        "an oracle entry",
    )?;
    for q in queries {
        match &q.classification {
            Classification::Ok => {
                let entry = row_of(oracle, &q.id, path)?;
                // The text-freshness comparison, where the entry and its
                // query are already paired: an edited query must not
                // ride a stale answer (the graph-benchmark-ldbc lane's guard).
                if entry.query_sha256 != q.sha256 {
                    return Err(format!(
                        "{}: {}'s oracle was generated for a different text (recorded {}, file is {}); \
                         regenerate the oracle for the edited query",
                        path.display(),
                        q.id,
                        entry.query_sha256,
                        q.sha256
                    ));
                }
            }
            Classification::Kept | Classification::ErrorPin { .. } => {
                if oracle.contains_key(&q.id) {
                    return Err(format!(
                        "{}: a kept or pin entry never executes; its oracle row is stale data",
                        q.id
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A workload's substitution parameters at one scale factor, per query
/// id: each parameter as the ONE Cypher expression the lane evaluates
/// and binds — the file format's one rule, no per-value kind
/// discrimination (a bare word is not an expression, so a string
/// parameter carries its quotes). The snb-bi lane uses this table
/// whole, loading its own workload's file.
#[derive(Debug, Clone)]
pub(super) struct ParamsTable {
    pub(super) queries: BTreeMap<String, BTreeMap<String, String>>,
}

impl ParamsTable {
    /// Parse the workload's parameter table for `sf` under `dir` — a
    /// store at another scale factor needs its own substitutions
    /// (seed-policy.md §4), so the file is exactly this scale factor's:
    /// every top-level key but the `_source` provenance block is a query
    /// id whose entry is an object of parameter → expression strings;
    /// any other shape is a malformed entry, refused naming the file.
    pub(super) fn load(dir: &Path, sf: ScaleFactor) -> Result<Self> {
        let path = params_file(dir, sf);
        let mut queries = BTreeMap::new();
        for (id, v) in read_query_entries(&path)? {
            let entries = v.as_object().ok_or_else(|| {
                format!(
                    "{}: {id}'s entry is not an object of parameters",
                    path.display()
                )
            })?;
            let mut params = BTreeMap::new();
            for (name, value) in entries {
                let expr = value.as_str().ok_or_else(|| {
                    format!(
                        "{}: {id}'s parameter {name} is not a Cypher expression string",
                        path.display()
                    )
                })?;
                params.insert(name.clone(), expr.to_string());
            }
            queries.insert(id, params);
        }
        Ok(Self { queries })
    }
}

/// The params table's contract with the workload's query files, checked
/// in the direction the lanes own: every params id names a query file —
/// stale entry data cannot ride along unvalidated (the shared stale-id
/// refusal). A query file with no entry at this scale factor plans,
/// never runs (the pathdb lane's rule for its SF1 subset, inherited
/// through the shared route table).
pub(super) fn validate_params(table: &ParamsTable, queries: &[CorpusQuery]) -> Result<()> {
    refuse_stale_ids(
        table.queries.keys().map(String::as_str),
        queries,
        "a params entry",
    )
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{ok_query, pin_query, query};
    use super::*;

    #[test]
    fn read_json_names_the_path_in_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.json");
        std::fs::write(&good, r#"{"a": 1}"#).unwrap();
        let doc: BTreeMap<String, serde_json::Value> = read_json(&good).unwrap();
        assert_eq!(doc["a"], 1);
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{").unwrap();
        let err = read_json::<serde_json::Value>(&bad).unwrap_err();
        assert!(
            err.contains("bad.json: "),
            "the parse error names the path: {err}"
        );
    }

    /// The rows-oracle loader over a whole oracle document: every
    /// top-level key but the `_`-prefixed provenance block is a query id
    /// with one rows check, and a malformed check fails naming the file
    /// and the id.
    #[test]
    fn rows_oracles_load_their_query_entries() {
        let dir = tempfile::tempdir().unwrap();
        let oracle_file = dir.path().join("oracle.json");
        std::fs::write(
            &oracle_file,
            r#"{"_source": {"note": "provenance"},
                "q1": {"rows": [{"c": 2}], "order_sensitive": false, "query_sha256": "aa"},
                "q2": {"rows": [], "order_sensitive": true, "query_sha256": "bb"}}"#,
        )
        .unwrap();
        let oracle = load_rows_oracle(&oracle_file).unwrap();
        assert_eq!(oracle.len(), 2, "the _source block is not an entry");
        assert_eq!(oracle["q1"].expected.rows.len(), 1);
        assert!(!oracle["q1"].expected.order_sensitive);
        assert!(oracle["q2"].expected.order_sensitive);
        let malformed = dir.path().join("malformed.json");
        std::fs::write(
            &malformed,
            r#"{"q1": {"order_sensitive": false, "query_sha256": "x"}}"#,
        )
        .unwrap();
        let err = load_rows_oracle(&malformed).unwrap_err();
        assert!(
            err.contains("malformed.json: q1"),
            "the refusal names the file and the id: {err}"
        );
    }

    /// The rows-oracle contract, both directions, over fixture tables:
    /// an executable entry without a row, a kept or pin entry carrying
    /// one, and an oracle row without a query file are malformed
    /// workloads the validation refuses — the refusal every rows-oracle
    /// lane runs at build, before any fetch or load.
    #[test]
    fn malformed_workloads_are_refused_at_build() {
        // The entries carry the sha of the very query texts the checks
        // pair them with (`ok_query` fixtures have empty shas, so the
        // recorded shas are empty too — the comparison is what is under
        // test, not the hash values).
        let entry = || OracleEntry {
            expected: crate::answers::Expected {
                rows: vec![],
                order_sensitive: false,
            },
            query_sha256: String::new(),
        };
        let oracle = Oracle::from([("q1".to_string(), entry()), ("q2".to_string(), entry())]);
        let path = Path::new("oracle.json");
        // A workload covering every oracle row, one entry per row, with
        // each case's mutation applied — the base the refusals below
        // mutate, so the stale-id check (which runs first) never masks
        // the case under test.
        let covering = |first_class: Classification| {
            vec![
                query("q1", first_class, "RETURN 1"),
                ok_query("q2", "RETURN 1"),
            ]
        };
        // An `ok` entry its oracle does not cover.
        let mut uncovered = covering(Classification::Ok);
        uncovered.push(ok_query("q9-not-in-the-oracle", "RETURN 1"));
        let err = validate_rows_oracle(&oracle, &uncovered, path).unwrap_err();
        assert!(err.contains("no oracle row"), "{err}");
        // A kept entry carrying a row never executes it; a pin entry
        // carrying one is the same stale data.
        let err = validate_rows_oracle(&oracle, &covering(Classification::Kept), path).unwrap_err();
        assert!(err.contains("stale data"), "{err}");
        // An oracle row with no query file — the shared stale-id refusal
        // over the rows-oracle table's own noun.
        let err = validate_rows_oracle(&oracle, &[ok_query("q1", "RETURN 1")], path).unwrap_err();
        assert!(
            err.contains("q2: an oracle entry with no query file"),
            "{err}"
        );
        // A pin entry without a row is a clean workload, and the
        // committed shape — every row covered, every `ok` entry carrying
        // one, no kept or pin entry carrying one — passes.
        assert!(validate_rows_oracle(
            &oracle,
            &[
                ok_query("q1", "RETURN 1"),
                ok_query("q2", "RETURN 1"),
                query("q3-kept", Classification::Kept, "RETURN 1"),
                pin_query("q4-pin", "RETURN 1"),
            ],
            path,
        )
        .is_ok());
    }
}
