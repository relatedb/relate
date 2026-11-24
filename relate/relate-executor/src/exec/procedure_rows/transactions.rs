//! Rows for `db.transactions` (ADR-0610): the retained transaction
//! records — one row per committed transaction the `value-provenance`
//! feature recorded, of every record type, ascending by `lsn`, with the
//! record's `type`, its metadata map and its derived changed property
//! keys, each changed key rendered with the element-id wire spelling.
//! The rows are a function of the retained table alone: deleted elements
//! stay represented (their records name them; the live graph is never
//! consulted), and `since` is the one production-bounding filter — the
//! inclusive `commitTs` lower bound, defaulting to every retained
//! record. The read is the table's streaming fold with the bound, the
//! row cap and the byte budget applied inside it (ADR-0555): no
//! whole-table Vec, and the answer never grows past
//! [`TRANSACTIONS_ROW_CAP`] rows or [`TRANSACTIONS_ANSWER_BYTES`] bytes.

use std::collections::BTreeMap;

use relate_core::{node_element_id, rel_element_id, Error, Namespace, Result, Value};
use relate_engine_api::{ChangedKey, ChangedKeyKind, TransactionRecord};

use crate::graph::audit_integer;
use crate::overlay::ExecGraph;

/// ADR-0610's engine-side answer bound for `db.transactions`: one call
/// answers at most this many rows. `since` is the caller's bounding
/// filter (the contract's scale control — a higher bound answers a
/// smaller suffix); this cap is the engine-side backstop that keeps the
/// answer a bounded page however large the retained table grew — the
/// remotely reachable surface's own memory contract (ADR-0555). A call
/// matching more is the typed refusal naming the bound and the remedy,
/// never a silent truncation. The value is the ADR-0555 batch scale's
/// own 1024 rows, deliberately spelled as its own named constant: the
/// stream batch bound and this answer bound are two policies and must
/// not share a name.
const TRANSACTIONS_ROW_CAP: usize = 1024;

/// ADR-0610 + ADR-0555's byte-side answer bound for `db.transactions`:
/// one call's built rows may together serialize to at most this many
/// bytes. [`TRANSACTIONS_ROW_CAP`] counts rows alone, and a row's
/// `metadata` cell runs to the ADR-0086 per-object envelope (16 MiB),
/// so a count-only cap would let the fold build a ~16 GiB answer before
/// it refused; this budget closes that hole by accumulating each built
/// row's serialized length ([`row_bytes`], the same measure the metadata
/// channel's own envelope bound takes) and refusing — naming both
/// bounds — the moment the page crosses it. The figure is the
/// retained-history families' own per-part 64 MiB, the scale one
/// commit record's serialized form is already bounded by on the write
/// side, so the common page (thousands of small rows) is bounded by the
/// row cap long before it, and the pathological one (few rows, each
/// near the envelope) stays at a handful of envelopes. Its own named
/// constant, never shared with the row cap: two policies, two names.
const TRANSACTIONS_ANSWER_BYTES: usize = 64 * 1024 * 1024;

/// Rows for `db.transactions` (ADR-0610): every retained record committed
/// at or after the `since` bound. The producer only ever runs under a
/// call's bound arguments; `since` is `None` when the call omitted the
/// defaulted argument (or bound NULL, which the CALL type check admits
/// for every declared type) — both answer the contract's default, every
/// retained record.
pub(super) fn transactions_rows(
    graph: &ExecGraph<'_>,
    since: Option<&Value>,
) -> Result<Vec<BTreeMap<String, Value>>> {
    let since = since_bound(since)?;
    // The retained table's streaming fold (ADR-0555) — never the
    // whole-table Vec: the `since` bound, the row cap and the byte
    // budget all apply inside it, so memory holds at most one bounded
    // page of built rows. The table iterates ascending by `commit_ts`,
    // and the WAL admits each record with both its `lsn` and its
    // `commit_ts` advancing (the preflight's `admits_lsn`/`admits_commit_ts`
    // pair), so the commit-ts order is the lsn order the contract
    // records — no second sort.
    let mut rows = Vec::new();
    let mut built_bytes = 0usize;
    graph.base().retained_transactions_into(&mut |record| {
        if record.commit_ts < since {
            return Ok(());
        }
        if rows.len() == TRANSACTIONS_ROW_CAP {
            return Err(Error::execution(format!(
                "db.transactions answers at most {TRANSACTIONS_ROW_CAP} rows; this call \
                 matched more — raise the `since` bound to narrow it"
            )));
        }
        let row = transaction_row(record, graph.namespace())?;
        built_bytes = built_bytes.saturating_add(row_bytes(&row)?);
        if built_bytes > TRANSACTIONS_ANSWER_BYTES {
            return Err(Error::execution(format!(
                "db.transactions answers at most {TRANSACTIONS_ROW_CAP} rows and \
                 {TRANSACTIONS_ANSWER_BYTES} bytes of rows; this call crossed the byte \
                 bound — raise the `since` bound to narrow it"
            )));
        }
        rows.push(row);
        Ok(())
    })?;
    Ok(rows)
}

/// One built row's serialized length (ADR-0610): the byte budget's
/// measure — the row map's `serde_json` length in the tagged form the
/// metadata channel's own envelope bound and the WAL's per-record
/// payload cap already measure by, so the budget prices the page in the
/// store's own serialized scale without holding a second copy of the
/// row's bytes (the Vec is transient, dropped at the return).
fn row_bytes(row: &BTreeMap<String, Value>) -> Result<usize> {
    serde_json::to_vec(row)
        .map(|bytes| bytes.len())
        .map_err(|e| Error::execution(format!("db.transactions row serialization failed: {e}")))
}

/// The inclusive `commitTs` lower bound of one call (ADR-0610): the
/// contract's `0` — every retained record — when the argument is absent
/// or NULL, and a negative bound admits every record too (every
/// `commit_ts` is unsigned). A non-integer binding is the typed
/// rejection, never a panic.
fn since_bound(since: Option<&Value>) -> Result<u64> {
    match since {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Int(bound)) => Ok((*bound).max(0) as u64),
        Some(_) => Err(Error::invalid_argument_type(
            "db.transactions argument `since` expects INTEGER",
        )),
    }
}

/// One retained record's row, in the yield order the signature declares.
/// The fold hands the table's own record by reference (the table
/// retains it — it is the audit surface's truth), so the row is built
/// without re-copying the audit payload: the changed keys iterate the
/// record's own list, and the metadata map is the one copy the row's
/// own cell requires — cloning the whole record to move the map would
/// re-copy the changed keys for nothing. A provenance record the
/// changed keys alone retained (no metadata map) answers the honest
/// empty map, like the live `tx.getMetaData` row; a record of another
/// type answers `changedKeys` null — its keys were never derived
/// (changed keys derive only for the provenance family's commits,
/// ADR-0610), so an empty list would claim a change-free commit. The
/// INTEGER cells come from the one shared checked conversion, so an
/// out-of-range `lsn` or `commitTs` is its typed error naming the field.
fn transaction_row(
    record: &TransactionRecord,
    namespace: Namespace,
) -> Result<BTreeMap<String, Value>> {
    let changed_keys = if record.is_provenance() {
        Value::List(
            record
                .changed_keys
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|key| changed_key_row(key, namespace))
                .collect::<Result<Vec<_>>>()?
                .into(),
        )
    } else {
        Value::Null
    };
    Ok(BTreeMap::from([
        ("lsn".to_string(), audit_integer(record.lsn, "lsn")?),
        (
            "commitTs".to_string(),
            audit_integer(record.commit_ts, "commitTs")?,
        ),
        (
            "type".to_string(),
            Value::String(record.record_type().as_str().into()),
        ),
        (
            "metadata".to_string(),
            Value::Map(record.metadata.clone().unwrap_or_default().into()),
        ),
        ("changedKeys".to_string(), changed_keys),
    ]))
}

/// One changed key's cell: the element's id in the wire spelling and the
/// property key. The record carries both for deleted elements, so the
/// rendering never consults the live graph. A namespace that renders no
/// element id (`system`) is the same typed failure `elementId()` reports —
/// the procedure runs on the user database, so the arm is a checked
/// invariant rather than a silent fallback.
fn changed_key_row(key: &ChangedKey, namespace: Namespace) -> Result<Value> {
    let element_id = match key.kind {
        ChangedKeyKind::Node => node_element_id(key.id, namespace),
        ChangedKeyKind::Rel => rel_element_id(key.id, namespace),
    }
    .ok_or_else(|| Error::execution("elements of this namespace have no element ids"))?;
    Ok(Value::Map(
        BTreeMap::from([
            ("elementId".to_string(), Value::String(element_id.into())),
            ("key".to_string(), Value::String(key.key.clone().into())),
        ])
        .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::builtin_procedure_rows;
    use super::super::executable_signature;
    use super::super::test_helpers::{rows, rows_with_args};
    use super::*;
    use relate_engine_api::RecordType;

    /// One seeded record, spelled once for the fixtures below: the
    /// commit at `commit_ts` (`lsn` equal to it) that set `key` on the
    /// node `id` under `{agent: <agent>}` — a provenance-family record
    /// (the absent type's default, `RecordType::default`).
    fn record(commit_ts: u64, agent: &str, id: i64, key: &str) -> TransactionRecord {
        TransactionRecord {
            lsn: commit_ts,
            commit_ts,
            metadata: Some(BTreeMap::from([(
                "agent".to_string(),
                Value::String(agent.into()),
            )])),
            changed_keys: Some(vec![ChangedKey {
                kind: ChangedKeyKind::Node,
                id,
                key: key.to_string(),
            }]),
            record_type: RecordType::default(),
        }
    }

    #[test]
    fn rows_render_the_records_fields_with_wire_element_ids() {
        // Three retained records on a default (neo4j-namespace) graph;
        // the third names a node the second transaction's record also
        // names — and a later delete could have removed, which changes
        // nothing here: the record, not the live graph, is the truth.
        let graph = crate::graph::Graph::default();
        graph.record_transaction(record(2, "a", 4, "title"));
        graph.record_transaction(record(4, "b", 5, "title"));
        graph.record_transaction(record(6, "a", 4, "title"));
        let rows = rows(&graph, "db.transactions");
        assert_eq!(rows.len(), 3);
        // Ascending lsn (the table's commit-ts order), the yield columns
        // in signature order, element ids in the `4:rl-id:n<id>` wire
        // spelling.
        for (row, (commit_ts, agent, id)) in
            rows.iter().zip([(2, "a", 4), (4, "b", 5), (6, "a", 4)])
        {
            assert_eq!(row["lsn"], Value::Int(commit_ts));
            assert_eq!(row["commitTs"], Value::Int(commit_ts));
            assert_eq!(row["type"], Value::String("provenance".into()));
            assert_eq!(
                row["metadata"],
                Value::Map(
                    BTreeMap::from([("agent".to_string(), Value::String(agent.into()))]).into()
                )
            );
            let Value::List(keys) = &row["changedKeys"] else {
                panic!("changedKeys is a list, got {:?}", row["changedKeys"])
            };
            assert_eq!(
                keys.as_slice(),
                [Value::Map(
                    BTreeMap::from([
                        (
                            "elementId".to_string(),
                            Value::String(format!("4:rl-id:n{id}").into())
                        ),
                        ("key".to_string(), Value::String("title".into())),
                    ])
                    .into()
                )]
            );
        }
    }

    #[test]
    fn a_changed_keys_only_record_answers_the_honest_empty_map() {
        // A relationship's changed key renders the `5:rl-id:r<id>` form,
        // and a record the keys alone retained carries no metadata map.
        let graph = crate::graph::Graph::default();
        graph.record_transaction(TransactionRecord {
            lsn: 1,
            commit_ts: 1,
            metadata: None,
            changed_keys: Some(vec![ChangedKey {
                kind: ChangedKeyKind::Rel,
                id: 9,
                key: "since".to_string(),
            }]),
            record_type: RecordType::default(),
        });
        let rows = rows(&graph, "db.transactions");
        assert_eq!(rows[0]["metadata"], Value::Map(BTreeMap::new().into()));
        let Value::List(keys) = &rows[0]["changedKeys"] else {
            panic!("changedKeys is a list")
        };
        let Value::Map(cell) = &keys[0] else {
            panic!("a changed-key cell is a map")
        };
        assert_eq!(
            cell.get("elementId"),
            Some(&Value::String("5:rl-id:r9".into()))
        );
        assert_eq!(cell.get("key"), Some(&Value::String("since".into())));
    }

    /// ADR-0610's type column over a mixed-type table: another family's
    /// record answers its own `type` with `changedKeys` null — its keys
    /// were never derived — while the provenance family's rows keep the
    /// honest list, and the absent field of a pre-typed record reads as
    /// the provenance family's own.
    #[test]
    fn another_types_row_answers_its_type_and_null_changed_keys() {
        let graph = crate::graph::Graph::default();
        graph.record_transaction(record(2, "a", 4, "title"));
        graph.record_transaction(TransactionRecord {
            lsn: 5,
            commit_ts: 5,
            metadata: Some(BTreeMap::from([("run".to_string(), Value::Int(7))])),
            changed_keys: None,
            record_type: RecordType::Typed("audit-trail".into()),
        });
        let rows = rows(&graph, "db.transactions");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["type"], Value::String("provenance".into()));
        assert_eq!(rows[1]["type"], Value::String("audit-trail".into()));
        assert_eq!(
            rows[1]["metadata"],
            Value::Map(BTreeMap::from([("run".to_string(), Value::Int(7))]).into())
        );
        assert_eq!(rows[1]["changedKeys"], Value::Null);
    }

    #[test]
    fn since_is_the_inclusive_committs_lower_bound() {
        let graph = crate::graph::Graph::default();
        graph.record_transaction(record(2, "a", 4, "title"));
        graph.record_transaction(record(4, "b", 5, "title"));
        graph.record_transaction(record(6, "a", 4, "title"));
        let bound = |since: i64| {
            rows_with_args(
                &graph,
                "db.transactions",
                None,
                &BTreeMap::from([("since".to_string(), Value::Int(since))]),
            )
        };
        // The default (no bound) answers every retained record, and so
        // does a negative one (every commit_ts is unsigned).
        assert_eq!(rows(&graph, "db.transactions").len(), 3);
        assert_eq!(bound(-1).len(), 3);
        // The inclusive bound keeps the record at the bound itself.
        assert_eq!(
            bound(4)
                .iter()
                .map(|row| row["commitTs"].clone())
                .collect::<Vec<_>>(),
            [Value::Int(4), Value::Int(6)]
        );
        // A bound above every record answers zero rows.
        assert!(bound(7).is_empty());
    }

    #[test]
    fn a_non_integer_since_is_a_typed_rejection() {
        let graph = crate::graph::Graph::default();
        let view = ExecGraph::read(&graph);
        let signature =
            executable_signature("db.transactions").expect("db.transactions is registered");
        let args = BTreeMap::from([("since".to_string(), Value::String("nope".into()))]);
        let error = builtin_procedure_rows(&view, signature, None, &args)
            .expect_err("a STRING since is rejected");
        assert!(
            error
                .to_string()
                .contains("InvalidArgumentType: db.transactions argument `since` expects INTEGER"),
            "{error}"
        );
    }

    /// The engine-side answer bounds (ADR-0610 + ADR-0555): a call whose
    /// matching records pass the row cap is the typed refusal naming the
    /// bound and the `since` remedy — never a silent truncation — and
    /// `since`, the contract's scale control, narrows the same table
    /// back under the bound: a call matching exactly the cap answers.
    /// The byte budget answers the row cap's blind side: a call well
    /// under the row count whose rows each carry a wide metadata map is
    /// refused the same way the moment their serialized page crosses
    /// the byte bound, and the byte-side refusal names both bounds.
    #[test]
    fn the_row_cap_refuses_past_its_bound_and_since_narrows_under_it() {
        let graph = crate::graph::Graph::default();
        for commit_ts in 1..=(TRANSACTIONS_ROW_CAP as u64 + 1) {
            graph.record_transaction(record(commit_ts, "a", 4, "title"));
        }
        let view = ExecGraph::read(&graph);
        let signature =
            executable_signature("db.transactions").expect("db.transactions is registered");
        // One record over the cap: the refusal names the bound.
        let error = builtin_procedure_rows(&view, signature, None, &BTreeMap::new())
            .expect_err("a call matching past the cap is refused");
        assert!(
            error.to_string().contains(&format!(
                "db.transactions answers at most {TRANSACTIONS_ROW_CAP} rows"
            )),
            "{error}"
        );
        // The same table under the bound: `since` 2 drops the first
        // record, leaving exactly the cap — answered in full.
        let under = builtin_procedure_rows(
            &view,
            signature,
            None,
            &BTreeMap::from([("since".to_string(), Value::Int(2))]),
        )
        .expect("a call matching exactly the cap answers");
        assert_eq!(under.len(), TRANSACTIONS_ROW_CAP);
    }

    /// The byte-side answer bound (ADR-0610 + ADR-0555): rows well under
    /// the row cap, each carrying a metadata map a large fraction of the
    /// ADR-0086 envelope — the shape a count-only cap would let build to
    /// ~16 GiB before it refused. The refusal names both bounds and the
    /// `since` remedy, and `since` past the wide records narrows the
    /// same table back under the budget — the narrow records after them
    /// still answer.
    #[test]
    fn the_byte_budget_refuses_a_page_of_wide_rows_and_since_narrows_under_it() {
        let graph = crate::graph::Graph::default();
        // One-megabyte metadata maps: a page of them crosses the 64 MiB
        // budget at the 64th row, far under the 1024-row cap.
        let pad = Value::String("x".repeat(1024 * 1024).into());
        let wide = |commit_ts: u64| TransactionRecord {
            lsn: commit_ts,
            commit_ts,
            metadata: Some(BTreeMap::from([("payload".to_string(), pad.clone())])),
            changed_keys: None,
            record_type: RecordType::default(),
        };
        for commit_ts in 1..=65u64 {
            graph.record_transaction(wide(commit_ts));
        }
        // The narrow records after the wide page: `since` past the wide
        // ones must still answer them.
        for commit_ts in 66..=68u64 {
            graph.record_transaction(record(commit_ts, "a", 4, "title"));
        }
        let view = ExecGraph::read(&graph);
        let signature =
            executable_signature("db.transactions").expect("db.transactions is registered");
        // The page of wide rows: the refusal names both bounds.
        let error = builtin_procedure_rows(&view, signature, None, &BTreeMap::new())
            .expect_err("a page of wide rows crosses the byte budget");
        let rendered = error.to_string();
        assert!(
            rendered.contains(&format!(
                "db.transactions answers at most {TRANSACTIONS_ROW_CAP} rows and \
                 {TRANSACTIONS_ANSWER_BYTES} bytes of rows"
            )),
            "{rendered}"
        );
        // The same table narrowed past the wide page answers its narrow
        // tail — under both bounds, in full.
        let narrowed = builtin_procedure_rows(
            &view,
            signature,
            None,
            &BTreeMap::from([("since".to_string(), Value::Int(66))]),
        )
        .expect("a `since` past the wide records answers");
        assert_eq!(narrowed.len(), 3);
        assert_eq!(narrowed[0]["commitTs"], Value::Int(66));
    }

    /// The INTEGER cells come from the shared checked conversion: a
    /// record whose `lsn` is beyond i64's range is the typed error
    /// naming the field — never a silently wrapped negative.
    #[test]
    fn an_out_of_range_lsn_is_the_typed_integer_error() {
        let graph = crate::graph::Graph::default();
        graph.record_transaction(TransactionRecord {
            lsn: i64::MAX as u64 + 1,
            commit_ts: 1,
            metadata: None,
            changed_keys: Some(vec![ChangedKey {
                kind: ChangedKeyKind::Node,
                id: 4,
                key: "title".to_string(),
            }]),
            record_type: RecordType::default(),
        });
        let view = ExecGraph::read(&graph);
        let signature =
            executable_signature("db.transactions").expect("db.transactions is registered");
        let error = builtin_procedure_rows(&view, signature, None, &BTreeMap::new())
            .expect_err("an lsn beyond the INTEGER range is refused");
        assert!(
            error
                .to_string()
                .contains("ADR-0610 audit surface: lsn exceeds the INTEGER range"),
            "{error}"
        );
    }
}
