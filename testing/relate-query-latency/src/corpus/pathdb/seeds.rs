//! The pathdb lane's seed table and the degree-median resolution policy
//! (seed-policy.md, ADR-0479) — the lane's whole seed plumbing: the
//! tables, their load-time invariants, the re-resolution under the
//! median policy, and the parameters an entry runs with.
//!
//! PathDB's own seed ids are LDBC datagen outputs and exist in none of
//! our stores (0 of 26 in graph-benchmark-ldbc SF1 — different datagen run), so every
//! seed is substituted with a node of OUR store, chosen deterministically
//! and recorded per scale factor in `params-sf<SF>.json`. The seed-key
//! table (`seed-keys.json`, every key, both scale factors) is the one
//! canonical fact table — its key grammar is canonical ([`canonical_key`]:
//! the label plus the type set sorted and de-duplicated, so one fact has
//! exactly one spelling); each query's seed is derived from it and
//! checked against it on load — never re-split.

use std::collections::BTreeMap;
use std::path::Path;

use relate::{Connection, Value};
use serde::Deserialize;

use crate::corpus::files::{params_file, read_json};
use crate::corpus::report;
use crate::workload::ScaleFactor;
use crate::Result;

/// The seed table's per-scale-factor key spelling (`sf0.1`) — the one
/// home for the fact the load check and the re-resolve both consult.
fn sf_key(sf: ScaleFactor) -> String {
    format!("sf{}", sf.as_str())
}

/// A seed key's two halves: the seed label and the first-hop
/// relationship types (`<label>|<types>`), split once for every reader.
fn split_key(key: &str) -> Result<(&str, &str)> {
    key.split_once('|')
        .ok_or_else(|| format!("seed key {key:?} is not <label>|<types>"))
}

/// A seed key's canonical spelling: the label plus the type set sorted
/// and de-duplicated — the ONE spelling a (label, type set) fact may
/// carry, so the same fact can never sit under two keys (an alternation
/// order carried over from a query's text, a doubled type: both are
/// re-spellings of the canonical fact and the loader refuses them).
fn canonical_key(key: &str) -> Result<String> {
    let (label, types) = split_key(key)?;
    let mut set: Vec<&str> = types.split('|').collect();
    if set.iter().any(|t| t.is_empty()) {
        return Err(format!("seed key {key:?} has an empty relationship type"));
    }
    set.sort_unstable();
    set.dedup();
    Ok(format!("{label}|{}", set.join("|")))
}

/// The key grammar's invariant at ONE door: `key` IS the canonical
/// spelling of its fact — a second spelling of an existing set is
/// refused naming the canonical spelling, with the door's own subject
/// wording (`ctx`) the two doors differ in and nothing else.
fn require_canonical(path: &Path, key: &str, ctx: &str) -> Result<()> {
    let canonical = canonical_key(key)?;
    if canonical != key {
        return Err(format!(
            "{}: {ctx} {key:?} is not the canonical spelling of its fact \
             ({canonical:?}: the label plus the type set sorted and de-duplicated)",
            path.display()
        ));
    }
    Ok(())
}

/// One recorded seed: the chosen node's id, its incident degree over
/// the key's first-hop relationship types, and the label of the node
/// the policy chose — which the load checks against the key's label, so
/// a key can never silently name a node of another label.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SeedRecord {
    pub(super) id: i64,
    pub(super) degree: u64,
    pub(super) label: String,
}

/// One query's seed entry: the seed and the key it was resolved through.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct QuerySeed {
    pub(super) seed: i64,
    pub(super) seed_key: String,
}

/// The parsed `seed-keys.json`: the one canonical seed-key fact table,
/// one record per key per scale factor (`null` where the key has no
/// seed there).
#[derive(Debug, Deserialize)]
struct SeedKeysFile {
    seed_keys: BTreeMap<String, BTreeMap<String, Option<SeedRecord>>>,
}

/// The parsed `params-sf<SF>.json`: the derived per-query seed entries
/// at one scale factor (its `_source` block is provenance prose the
/// parser skips).
#[derive(Debug, Deserialize)]
struct ParamsFile {
    queries: BTreeMap<String, QuerySeed>,
}

/// The pathdb workload's seed tables at one scale factor: the per-query
/// entries plus the canonical key table they derive from.
#[derive(Debug, Clone)]
pub(super) struct SeedTable {
    /// The scale factor the per-query entries were parsed for — the
    /// table IS this scale factor's table, so every lookup inside the
    /// lane reads it from here, never a second carrier.
    sf: ScaleFactor,
    pub(super) queries: BTreeMap<String, QuerySeed>,
    pub(super) seed_keys: BTreeMap<String, BTreeMap<String, Option<SeedRecord>>>,
}

impl SeedTable {
    /// Parse the pathdb workload's seed tables under `dir`: the
    /// per-query entries of `params-sf<SF>.json` and the one canonical
    /// `seed-keys.json` every scale factor consults (the fraud workload
    /// anchors on planted ids and has no seeds).
    pub(super) fn load(dir: &Path, sf: ScaleFactor) -> Result<Self> {
        let params_path = params_file(dir, sf);
        let queries: ParamsFile = read_json(&params_path)?;
        let keys_path = dir.join("seed-keys.json");
        let keys: SeedKeysFile = read_json(&keys_path)?;
        // The key grammar's one invariant, enforced at BOTH doors a
        // spelling enters: every key IS its canonical spelling, so a
        // (label, type set) fact exists under exactly one key — the
        // fact table's own keys and the params entries' seed keys
        // alike, a second spelling of an existing set refused naming
        // the spelling the fact already has.
        for (id, entry) in &queries.queries {
            require_canonical(&params_path, &entry.seed_key, &format!("{id}'s seed key"))?;
        }
        for key in keys.seed_keys.keys() {
            require_canonical(&keys_path, key, "seed key")?;
        }
        Ok(Self {
            sf,
            queries: queries.queries,
            seed_keys: keys.seed_keys,
        })
    }

    /// The scale factor this table holds entries for.
    pub(super) fn sf(&self) -> ScaleFactor {
        self.sf
    }

    /// The seed a query runs with at this scale factor: `None` when the
    /// params file holds no entry for it (the SF1 file holds the spike's
    /// 26-query subset — a query without an entry there plans, never
    /// runs), the entry's recorded seed when it does — checked against
    /// the format's two invariants (seed-policy.md §5): a query's seed
    /// is its seed key's recorded id, and the key's label names the
    /// label of the node the record chose. The per-query entries are
    /// derived facts, never re-split ones, and a key never names a node
    /// of another label.
    pub(super) fn seed_of(&self, id: &str) -> Result<Option<i64>> {
        let Some(entry) = self.queries.get(id) else {
            return Ok(None);
        };
        let record = self
            .seed_keys
            .get(&entry.seed_key)
            .and_then(|per_sf| per_sf.get(&sf_key(self.sf)))
            .and_then(|record| record.as_ref())
            .ok_or_else(|| {
                format!(
                    "{id}: seed key {:?} has no record at SF{}",
                    entry.seed_key,
                    self.sf.as_str()
                )
            })?;
        if record.id != entry.seed {
            return Err(format!(
                "{id}: seed {} disagrees with its key {:?} (recorded {})",
                entry.seed, entry.seed_key, record.id
            ));
        }
        let (key_label, _) = split_key(&entry.seed_key)?;
        if record.label != key_label {
            return Err(format!(
                "{id}: seed key {:?} names label {key_label:?} but its record chose a \
                 {:?} node (id {}); re-key the entry to the label the policy resolved",
                entry.seed_key, record.label, record.id
            ));
        }
        Ok(Some(entry.seed))
    }
}

/// Re-resolve every recorded seed key with the degree-median policy and
/// record the keys whose resolved seed differs from the recorded one.
pub(super) fn reseed(
    conn: &mut Connection,
    table: &SeedTable,
    diffs: &mut Vec<report::SeedDiff>,
) -> Result<()> {
    for (key, per_sf) in &table.seed_keys {
        let Some(Some(recorded)) = per_sf.get(&sf_key(table.sf)) else {
            continue;
        };
        let (label, types) = split_key(key)?;
        let Some(resolved) = resolve_seed(conn, label, types)? else {
            return Err(format!(
                "seed key {key:?} resolved to nothing on this store (its recorded seed is \
                 {}); the data set no longer carries those types",
                recorded.id
            ));
        };
        if resolved.id != recorded.id {
            diffs.push(report::SeedDiff {
                seed_key: key.clone(),
                recorded: recorded.id,
                recorded_degree: recorded.degree,
                resolved: resolved.id,
                resolved_degree: resolved.degree,
            });
        }
    }
    Ok(())
}

/// A seed the degree-median policy chose: node id, incident degree over
/// the key's types, and the label of the node actually chosen (which
/// may differ from the key's label under the fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedSeed {
    id: i64,
    degree: u64,
    label: String,
}

/// The degree-median policy (seed-policy.md §2): among nodes of `label`
/// with at least one incident edge of `types`, order by `(degree, ID)`
/// and take the element at the lower-median position. When no node of
/// the label carries those types, the same policy over all labels. The
/// `sort_by` below is the one home of the ordering — the queries return
/// the rows unordered (ids are unique per node, so the sort fully
/// determines the median element).
pub(super) fn resolve_seed(
    conn: &mut Connection,
    label: &str,
    types: &str,
) -> Result<Option<ResolvedSeed>> {
    let labelled = format!("MATCH (n:{label})-[r:{types}]-() RETURN n.ID AS id, count(r) AS d");
    let r = conn
        .query(&labelled)
        .map_err(|e| format!("the labelled seed query for {label}|{types}: {e}"))?;
    let mut pairs = labelled_pairs(&r, label)?;
    if pairs.is_empty() {
        let unlabelled =
            format!("MATCH (n)-[r:{types}]-() RETURN n.ID AS id, count(r) AS d, labels(n) AS ls");
        let r = conn
            .query(&unlabelled)
            .map_err(|e| format!("the fallback seed query for {types}: {e}"))?;
        pairs = fallback_pairs(&r)?;
    }
    if pairs.is_empty() {
        return Ok(None);
    }
    pairs.sort_by_key(|a| (a.1, a.0));
    let (id, degree, label) = pairs[pairs.len() / 2].clone();
    Ok(Some(ResolvedSeed { id, degree, label }))
}

/// `(id, degree, label)` rows of the labelled seed query — the label is
/// the query's own.
fn labelled_pairs(r: &relate::QueryResult, label: &str) -> Result<Vec<(i64, u64, String)>> {
    r.rows
        .iter()
        .map(|row| match &row[..] {
            [Value::Int(id), Value::Int(d)] => Ok((*id, degree(*d, row)?, label.to_string())),
            _ => Err(format!("a labelled seed row is not (id, degree): {row:?}")),
        })
        .collect()
}

/// `(id, degree, label)` rows of the fallback seed query over all
/// labels — the label is the chosen node's own.
fn fallback_pairs(r: &relate::QueryResult) -> Result<Vec<(i64, u64, String)>> {
    r.rows
        .iter()
        .map(|row| match &row[..] {
            [Value::Int(id), Value::Int(d), Value::List(ls)] => {
                let label = match ls.first() {
                    Some(Value::String(s)) if !s.is_empty() => s.to_string(),
                    _ => {
                        return Err(format!(
                            "a fallback seed row's labels are not a non-empty string list: {row:?}"
                        ))
                    }
                };
                Ok((*id, degree(*d, row)?, label))
            }
            _ => Err(format!(
                "a fallback seed row is not (id, degree, labels): {row:?}"
            )),
        })
        .collect()
}

/// A seed row's degree. `count(r)` is never negative; a negative `Int`
/// is a malformed row, reported — never silently clamped.
fn degree(d: i64, row: &[Value]) -> Result<u64> {
    u64::try_from(d).map_err(|_| format!("a seed row's degree {d} is negative: {row:?}"))
}

/// A temp workload dir holding the two seed files — this scale factor's
/// params and the canonical seed keys — so every test that loads a seed
/// table, the loader's own invariants and the lane's alike, runs
/// against real files.
#[cfg(test)]
pub(super) fn write_seed_table(params: &str, keys: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("params-sf0.1.json"), params).unwrap();
    std::fs::write(dir.path().join("seed-keys.json"), keys).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::Workload;

    const KEYS: &str = r#"{"_source": {}, "seed_keys": {
        "Person|knows": {"sf0.1": {"id": 9, "degree": 3, "label": "Person"}, "sf1": null},
        "Tag|hasInterest": {"sf0.1": {"id": 7, "degree": 2, "label": "Tag"},
                            "sf1": null}}}"#;

    #[test]
    fn seed_tables_check_both_derived_seed_invariants() {
        let params = r#"{"_source": {}, "queries": {
            "p009": {"seed": 9, "seed_key": "Person|knows"},
            "p010": {"seed": 8, "seed_key": "Person|knows"},
            "p011": {"seed": 9, "seed_key": "Person|likes"},
            "p095": {"seed": 7, "seed_key": "Tag|hasInterest"}}}"#;
        let dir = write_seed_table(params, KEYS);
        let table = SeedTable::load(dir.path(), ScaleFactor::Sf0_1).unwrap();
        assert_eq!(table.seed_of("p009").unwrap(), Some(9));
        assert_eq!(table.seed_of("p095").unwrap(), Some(7));
        // No entry at this scale factor: no seed, not an error — the
        // entry plans, never runs.
        assert_eq!(table.seed_of("p012").unwrap(), None);
        assert!(table
            .seed_of("p010")
            .unwrap_err()
            .contains("disagrees with its key"));
        assert!(table
            .seed_of("p011")
            .unwrap_err()
            .contains("no record at SF0.1"));
        // A record whose label is not its key's label is a crossing the
        // table refuses to hide: the key must name the node it chose.
        let mismatched_keys = r#"{"_source": {}, "seed_keys": {
            "Comment|hasInterest": {"sf0.1": {"id": 7, "degree": 2,
                                              "label": "Tag"}}}}"#;
        let mismatched_params = r#"{"_source": {}, "queries": {
            "p095": {"seed": 7, "seed_key": "Comment|hasInterest"}}}"#;
        let mismatched_dir = write_seed_table(mismatched_params, mismatched_keys);
        let mismatched = SeedTable::load(mismatched_dir.path(), ScaleFactor::Sf0_1).unwrap();
        let err = mismatched.seed_of("p095").unwrap_err();
        assert!(
            err.contains("names label \"Comment\" but its record chose a \"Tag\""),
            "{err}"
        );
    }

    #[test]
    fn the_committed_seed_tables_parse_and_check() {
        let table = SeedTable::load(&Workload::Pathdb.dir(), ScaleFactor::Sf0_1).unwrap();
        assert_eq!(table.queries.len(), 207);
        assert_eq!(table.seed_keys.len(), 36);
        for id in table.queries.keys() {
            table.seed_of(id).unwrap();
        }
        let sf1 = SeedTable::load(&Workload::Pathdb.dir(), ScaleFactor::Sf1).unwrap();
        assert_eq!(sf1.queries.len(), 26, "the spike's SF1 subset");
        for id in sf1.queries.keys() {
            sf1.seed_of(id).unwrap();
        }
        // An unreadable workloads directory fails loudly, never
        // satisfies the ratchet by being unable to check it.
        assert!(
            !Workload::FraudQpp
                .dir()
                .join("params-sf0.1.json")
                .try_exists()
                .unwrap(),
            "the fraud workload has no params file"
        );
    }

    /// The key grammar's one invariant, at both doors: a (label, type
    /// set) fact has exactly one spelling — the canonical one. A doubled
    /// type, a re-ordered alternation, or a second spelling of an
    /// existing set is refused at load — in the fact table's own keys
    /// and in a params entry's seed key alike — naming the canonical
    /// spelling, so the fact table cannot drift apart again.
    #[test]
    fn a_second_spelling_of_a_seed_fact_is_refused_at_load() {
        // A doubled type is a re-spelling of the single-type fact.
        let doubled = r#"{"_source": {}, "seed_keys": {
            "Person|knows|knows": {"sf0.1": {"id": 9, "degree": 3,
                                             "label": "Person"}, "sf1": null}}}"#;
        let dir = write_seed_table(r#"{"_source": {}, "queries": {}}"#, doubled);
        let err = SeedTable::load(dir.path(), ScaleFactor::Sf0_1).unwrap_err();
        assert!(
            err.contains("is not the canonical spelling") && err.contains("\"Person|knows\""),
            "{err}"
        );
        // The same door on the params side: an entry's seed key is held
        // to the canonical spelling even when the fact table is clean.
        let misspelled_params = r#"{"_source": {}, "queries": {
            "p009": {"seed": 9, "seed_key": "Person|knows|knows"}}}"#;
        let dir = write_seed_table(misspelled_params, KEYS);
        let err = SeedTable::load(dir.path(), ScaleFactor::Sf0_1).unwrap_err();
        assert!(
            err.contains("p009's seed key")
                && err.contains("is not the canonical spelling")
                && err.contains("\"Person|knows\""),
            "the params door refuses the second spelling: {err}"
        );
        // A re-ordered alternation is a second spelling of the same set.
        let reordered = r#"{"_source": {}, "seed_keys": {
            "Person|knows|hasInterest": {"sf0.1": {"id": 9, "degree": 3,
                                                   "label": "Person"}, "sf1": null}}}"#;
        let dir = write_seed_table(r#"{"_source": {}, "queries": {}}"#, reordered);
        let err = SeedTable::load(dir.path(), ScaleFactor::Sf0_1).unwrap_err();
        assert!(
            err.contains("\"Person|hasInterest|knows\""),
            "the error names the canonical spelling: {err}"
        );
        // An empty type half is malformed, not canonicalized away.
        let empty = r#"{"_source": {}, "seed_keys": {
            "Person|": {"sf0.1": {"id": 9, "degree": 3, "label": "Person"}, "sf1": null}}}"#;
        let dir = write_seed_table(r#"{"_source": {}, "queries": {}}"#, empty);
        assert!(SeedTable::load(dir.path(), ScaleFactor::Sf0_1)
            .unwrap_err()
            .contains("empty relationship type"));
    }

    #[test]
    fn seed_resolution_follows_the_degree_median_policy() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute(
            "CREATE (a:P {ID: 1})-[:K]->(b:P {ID: 2}), (a)-[:K]->(:P {ID: 3}),
                     (c:P {ID: 4})-[:K]->(:P {ID: 5}), (:Q {ID: 6})-[:L]->(:Q {ID: 7})",
        )
        .unwrap();
        // Degrees over K: ID 1 -> 2, ID 2 -> 1, ID 3 -> 1, ID 4 -> 1,
        // ID 5 -> 1. Sorted by (degree, id): (1,2),(1,3),(1,4),(1,5),(2,1)
        // — the lower-median position (5/2 = 2) is ID 4.
        let seed = resolve_seed(&mut conn, "P", "K").unwrap().unwrap();
        assert_eq!(
            (seed.id, seed.degree, seed.label.as_str()),
            (4, 1, "P"),
            "the element at the lower-median position of (degree, ID)"
        );
        // The fallback: no P is incident to an L edge, so the same
        // policy runs over all labels and reports the chosen node's own
        // label.
        let seed = resolve_seed(&mut conn, "P", "L").unwrap().unwrap();
        assert_eq!(seed.label, "Q");
        // Nothing carries an M edge at all: no seed, not an error.
        assert!(resolve_seed(&mut conn, "P", "M").unwrap().is_none());
    }
}
