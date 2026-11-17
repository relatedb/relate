//! Push execution (ADR-0401 §2/§4/§6): pre-read the remote's current keyed
//! rows per entity (one Query API SELECT per label/type group), classify
//! every planned row against them (created / updated / skipped with its
//! reason, `--updated-at` conflict decisions), then send the applying rows
//! as batched parameterized statements through the shared Query API v2
//! client. A failing batch fails the push with the statement, the batch's
//! row offset, and which batches had already landed — never a silent
//! partial state.

use std::collections::BTreeMap;
use std::time::Instant;

use relate::Value;
use serde_json::Value as Json;

use super::plan::{EntityKind, EntityRow, PlannedEntity, PushPlan};
use super::policy::{resolve_candidate, updated_at_decision, ConflictDecision, UpdatePolicy};
use super::report::{BatchSize, EntityReport, Outcome, PushReport, Totals};
use super::statement::write_row;
use super::wire::{encode_value, envelope_rows, quote_ident};
use crate::{http, LoadError, PushOpts};

/// Execute the plan (not a dry run): pre-read, classify, write, report.
/// Returns the report — on a failed batch the partial one, exactly which
/// batches landed — plus the failure when one occurred (ADR-0401 §6).
pub(crate) fn execute(
    opts: &PushOpts,
    batch_size: BatchSize,
    plan: &PushPlan,
    started: Instant,
) -> (PushReport, Option<LoadError>) {
    let mut entities: Vec<EntityReport> = Vec::with_capacity(plan.entities.len());
    let mut sent = Totals::default();
    let mut failure: Option<LoadError> = None;
    let mut iter = plan.entities.iter();
    for entity in &mut iter {
        let mut report = EntityReport::planned(entity);
        let statement = match writable_statement(entity) {
            Ok(Some(statement)) => statement,
            // Nothing to write (a skip-only group, or the dangling bucket).
            Ok(None) => {
                entities.push(report);
                continue;
            }
            Err(error) => {
                failure = Some(error);
                entities.push(report);
                break;
            }
        };
        if let Err(error) = run_entity(opts, batch_size, entity, statement, &mut report, &mut sent)
        {
            // The failing entity's counters are as far as they got.
            failure = Some(error);
            entities.push(report);
            break;
        }
        entities.push(report);
    }
    // An entity already failed: the rest were never attempted — planned
    // totals, no batches, no writes.
    entities.extend(iter.map(EntityReport::planned));
    let report = PushReport::build(false, entities, &sent, started.elapsed());
    (report, failure)
}

/// The statement an entity writes with: `None` for an entity with no rows
/// (a skip-only group, or the dangling bucket). Rows without a statement
/// break the plan's invariant — every writing entity is built with its
/// statement — so it fails loudly instead of reporting nothing to write.
fn writable_statement(entity: &PlannedEntity) -> Result<Option<&str>, LoadError> {
    match (entity.statement.as_deref(), entity.rows.is_empty()) {
        (_, true) => Ok(None),
        (Some(statement), false) => Ok(Some(statement)),
        (None, false) => Err(LoadError::Push(format!(
            "entity {}: rows planned without a statement",
            entity.kind.display_name()
        ))),
    }
}

/// The dry-run report (ADR-0401 §6): planned counts, merge keys, statement
/// shapes, batch counts — without touching the remote.
pub(crate) fn dry_run_report(
    batch_size: BatchSize,
    plan: &PushPlan,
    started: Instant,
) -> PushReport {
    let entities: Vec<EntityReport> = plan
        .entities
        .iter()
        .map(|entity| EntityReport::dry_run(batch_size, entity))
        .collect();
    PushReport::build(true, entities, &Totals::default(), started.elapsed())
}

/// One entity's classification → batched writes, one orchestrator over
/// the phase helpers. Mutates the entity's report counters as it goes,
/// so a mid-entity failure still reports what landed.
fn run_entity(
    opts: &PushOpts,
    batch_size: BatchSize,
    entity: &PlannedEntity,
    statement: &str,
    report: &mut EntityReport,
    sent: &mut Totals,
) -> Result<(), LoadError> {
    let apply = classify(opts, entity, report)?;
    let writes: Vec<(Json, Outcome)> = apply
        .iter()
        .map(|&(row, outcome)| Ok((write_row(entity, row)?, outcome)))
        .collect::<Result<_, LoadError>>()?;
    send_batches(opts, batch_size, entity, statement, &writes, report, sent)
}

/// The classification phase (ADR-0401 §4): which of the entity's rows
/// apply, each with the outcome it will count as once its batch lands —
/// the one owner of the keyed-ness decision. The never-written skips
/// (`skipped_policy`, `skipped_remote_newer`) count here, at decision
/// time. An unkeyed CREATE group has no identity to classify by: its
/// rows apply wholesale, all created (documented in --help). A keyed
/// group pre-reads the remote and folds the decision table over the map.
fn classify<'a>(
    opts: &PushOpts,
    entity: &'a PlannedEntity,
    report: &mut EntityReport,
) -> Result<Vec<(&'a EntityRow, Outcome)>, LoadError> {
    let Some(policy) = entity.kind.keyed_policy() else {
        return Ok(entity
            .rows
            .iter()
            .map(|row| (row, Outcome::Created))
            .collect());
    };
    let remote = pre_read(opts, entity)?;
    classify_keyed(opts, entity, policy, &remote, report)
}

/// The keyed classification fold over a given pre-read map — pure, so
/// the ADR-0401 §4 decision table is testable without a live fixture.
fn classify_keyed<'a>(
    opts: &PushOpts,
    entity: &'a PlannedEntity,
    policy: UpdatePolicy,
    remote: &BTreeMap<String, Option<Value>>,
    report: &mut EntityReport,
) -> Result<Vec<(&'a EntityRow, Outcome)>, LoadError> {
    let mut apply: Vec<(&EntityRow, Outcome)> = Vec::new();
    for row in &entity.rows {
        let identity = row_identity(entity, row)?;
        // One lookup decides the three-way fact: absent on the remote
        // (a new row), present without a candidate, present with one.
        let remote_value = match remote.get(&identity) {
            None => {
                apply.push((row, Outcome::Created));
                continue;
            }
            Some(value) => value.as_ref(),
        };
        if policy == UpdatePolicy::Skip {
            report.skipped_policy += 1;
            continue;
        }
        let local = resolve_candidate(row.props(), &opts.updated_at);
        let decision = match (local, remote_value) {
            (Some(local), Some(remote)) => updated_at_decision(local, remote),
            // No candidate named, or a candidate missing on either side:
            // no conflict to decide, the scoped policy applies
            // (ADR-0401 §4).
            _ => ConflictDecision::Apply,
        };
        match decision {
            ConflictDecision::Apply => apply.push((row, Outcome::Updated)),
            ConflictDecision::RemoteNewer => report.skipped_remote_newer += 1,
            ConflictDecision::Incomparable => apply.push((row, Outcome::UpdatedIncomparable)),
        }
    }
    Ok(apply)
}

/// The batched writes (ADR-0401 §6): one parameterized statement per
/// `--batch-size` slice, one failure context per batch. `batches` and the
/// created/updated counters count what actually landed: each batch's
/// outcomes are recorded only after its statement succeeded.
fn send_batches(
    opts: &PushOpts,
    batch_size: BatchSize,
    entity: &PlannedEntity,
    statement: &str,
    writes: &[(Json, Outcome)],
    report: &mut EntityReport,
    sent: &mut Totals,
) -> Result<(), LoadError> {
    let name = entity.kind.display_name();
    for (batch_no, offset, batch) in batch_size.batches(writes) {
        let mut parameters = serde_json::Map::new();
        // The rows parameter is the wire module's List-of-Map envelope
        // (the one spelling of the typed-envelope shape).
        parameters.insert(
            "rows".into(),
            envelope_rows(batch.iter().map(|(row, _)| row)),
        );
        let (_, body_bytes) = http::run_query_typed_params(
            &opts.credentials,
            statement,
            &parameters,
            opts.allow_plaintext,
        )
        .map_err(|error| {
            // The offending statement rides the error (ADR-0401 §6): the
            // parameters themselves stay off stderr — a 1000-row batch is
            // not printable — and the row range names exactly which rows of
            // the entity the statement carried.
            LoadError::Push(format!(
                "batch {batch_no} of {name} (rows {offset}..{end}) failed: {error}\nstatement: {statement}",
                end = offset + batch.len(),
            ))
        })?;
        sent.statements += 1;
        sent.rows += batch.len();
        sent.bytes += body_bytes;
        report.batches += 1;
        report.record_landed(batch.iter().map(|&(_, outcome)| outcome));
    }
    Ok(())
}

/// Pre-read the remote's current rows for one entity: the identity columns
/// (merge key, endpoint keys, the keyed mode's resolved rel keys), then the
/// `--updated-at` candidate columns — decoded through the typed wire so
/// temporals and numbers compare as themselves. The result maps the
/// canonical identity string to the remote's first present candidate
/// (`None` when the remote row carries none).
fn pre_read(
    opts: &PushOpts,
    entity: &PlannedEntity,
) -> Result<BTreeMap<String, Option<Value>>, LoadError> {
    let select = pre_read_statement(entity, &opts.updated_at)?;
    let name = entity.kind.display_name();
    let key_len = key_column_count(entity);
    let mut remote: BTreeMap<String, Option<Value>> = BTreeMap::new();
    http::run_query_streaming_typed(
        &opts.credentials,
        &select,
        &serde_json::Map::new(),
        opts.allow_plaintext,
        // Rows are decoded as they stream off the body — the pre-read is
        // never buffered whole. The callback hands over batches of complete
        // rows; each element is one row's array of cells.
        &mut |batch: &[Json]| {
            for row in batch {
                let cells: &[Json] = row.as_array().ok_or_else(|| {
                    LoadError::Push(format!("pre-read of {name} returned a non-array row"))
                })?;
                decode_pre_read_row(&name, cells, key_len, &mut remote)?;
            }
            Ok(())
        },
    )
    .map_err(|error| LoadError::Push(format!("pre-read of {name} failed: {error}")))?;
    Ok(remote)
}

/// Decode one pre-read row into the remote map: identity cells first (null
/// cannot identify a row — a remote row missing its key property is not
/// usable as an identity; a malformed cell fails closed), then the first
/// present candidate cell, in the SELECT's order.
fn decode_pre_read_row(
    name: &str,
    cells: &[Json],
    key_len: usize,
    remote: &mut BTreeMap<String, Option<Value>>,
) -> Result<(), LoadError> {
    if cells.len() < key_len {
        return Err(LoadError::Push(format!(
            "pre-read of {name} returned a short row ({:?} columns)",
            cells.len()
        )));
    }
    let mut key = Vec::with_capacity(key_len);
    for cell in &cells[..key_len] {
        match decode_pre_read_cell(cell)? {
            Some(value) => key.push(encode_value(&value)?),
            None => return Ok(()),
        }
    }
    // Candidate cells: the first present one — the same "first present
    // wins" order the local side resolves in.
    let candidate = cells[key_len..]
        .iter()
        .filter(|cell| !matches!(cell, Json::Null))
        .map(decode_pre_read_cell)
        .find_map(Result::transpose)
        .transpose()?;
    remote.insert(canonical_key(&key), candidate);
    Ok(())
}

/// The pre-read SELECT for one entity: the MATCH pattern and the identity
/// plus candidate columns.
fn pre_read_statement(entity: &PlannedEntity, candidates: &[String]) -> Result<String, LoadError> {
    Ok(format!(
        "MATCH {} RETURN {}",
        pre_read_pattern(entity)?,
        pre_read_columns(entity, candidates)?
    ))
}

/// The pre-read's MATCH pattern.
fn pre_read_pattern(entity: &PlannedEntity) -> Result<String, LoadError> {
    match &entity.kind {
        // The merge identity: the keyed label alone. Sibling label sets
        // sharing it hold distinct keys (the keyed label's uniqueness
        // constraint), so each remote row belongs to one group.
        EntityKind::NodeLabel { keyed_label, .. } => {
            super::statement::label_pattern(std::slice::from_ref(keyed_label))
        }
        EntityKind::Rel {
            rel_type,
            start_label,
            end_label,
            ..
        } => Ok(format!(
            "(a:{})-[r:{}]->(b:{})",
            quote_ident(start_label)?,
            quote_ident(rel_type)?,
            quote_ident(end_label)?
        )),
        EntityKind::NodeCreate { .. }
        | EntityKind::NodeSkipped { .. }
        | EntityKind::RelSkipped { .. }
        | EntityKind::Dangling => Err(LoadError::Push(
            "an unkeyed group has no pre-read".to_string(),
        )),
    }
}

/// The pre-read's RETURN items: identity columns in the order
/// [`row_identity`] encodes them (start keys, end keys, rel keys), then the
/// candidate columns.
fn pre_read_columns(entity: &PlannedEntity, candidates: &[String]) -> Result<String, LoadError> {
    // Every identifier is backtick-quoted or the SELECT is refused — an
    // escaping failure never downgrades to raw interpolation.
    let mut items = Vec::new();
    let column =
        |var: &str, alias: &str, prop: &String, index: usize| -> Result<String, LoadError> {
            Ok(format!("{var}.{} AS {alias}{index}", quote_ident(prop)?))
        };
    match &entity.kind {
        EntityKind::NodeLabel { .. } => {
            for (index, prop) in entity.merge_key.iter().enumerate() {
                items.push(column("n", "k", prop, index)?);
            }
        }
        EntityKind::Rel {
            start_key, end_key, ..
        } => {
            for (index, prop) in start_key.iter().enumerate() {
                items.push(column("a", "k", prop, index)?);
            }
            for (index, prop) in end_key.iter().enumerate() {
                items.push(column("b", "e", prop, index)?);
            }
            for (index, prop) in entity.merge_key.iter().enumerate() {
                items.push(column("r", "r", prop, index)?);
            }
        }
        _ => {}
    }
    let candidate_var = match &entity.kind {
        EntityKind::Rel { .. } => "r",
        _ => "n",
    };
    for (index, name) in candidates.iter().enumerate() {
        items.push(column(candidate_var, "c", name, index)?);
    }
    Ok(items.join(", "))
}

/// The number of identity columns the pre-read returns — the boundary
/// between identity cells and candidate cells in every row. The same order
/// [`row_identity`] encodes, so the two sides agree by construction.
fn key_column_count(entity: &PlannedEntity) -> usize {
    match &entity.kind {
        EntityKind::NodeLabel { .. } => entity.merge_key.len(),
        EntityKind::Rel {
            start_key, end_key, ..
        } => start_key.len() + end_key.len() + entity.merge_key.len(),
        _ => 0,
    }
}

/// Decode one pre-read cell: a typed envelope into its engine value, null
/// into `None`. A malformed cell fails closed.
fn decode_pre_read_cell(cell: &Json) -> Result<Option<Value>, LoadError> {
    match cell {
        Json::Null => Ok(None),
        other => crate::typed_value::decode_value(other)
            .map(Some)
            .map_err(|e| {
                LoadError::Push(format!(
                    "the pre-read returned a value the engine cannot decode: {e}"
                ))
            }),
    }
}

/// The canonical identity string of one row: the serde_json spelling of its
/// encoded key values — identical on both sides of the diff, so equality is
/// by construction.
fn canonical_key(encoded: &[Json]) -> String {
    serde_json::to_string(encoded).expect("serde_json serializes its own values")
}

/// The identity of one planned row, as the pre-read's identity-column order
/// spells it: node keys; rel start keys, end keys, then rel keys.
fn row_identity(entity: &PlannedEntity, row: &EntityRow) -> Result<String, LoadError> {
    let ordered =
        |props: &BTreeMap<String, Value>, names: &[String]| -> Result<Vec<Json>, LoadError> {
            names
                .iter()
                .map(|prop| {
                    props.get(prop).map(encode_value).unwrap_or_else(|| {
                        Err(LoadError::Push(format!(
                            "merge-key property {prop:?} missing from a {row_name} row",
                            row_name = entity.kind.display_name()
                        )))
                    })
                })
                .collect()
        };
    let encoded = match (&entity.kind, row) {
        (EntityKind::NodeLabel { .. }, EntityRow::Node { key, .. }) => {
            ordered(key, &entity.merge_key)?
        }
        (
            EntityKind::Rel {
                start_key, end_key, ..
            },
            EntityRow::Rel {
                start, end, key, ..
            },
        ) => {
            let mut encoded = ordered(start, start_key)?;
            encoded.extend(ordered(end, end_key)?);
            encoded.extend(ordered(key, &entity.merge_key)?);
            encoded
        }
        // A structural/CREATE rel row: the endpoints are the identity.
        (
            EntityKind::Rel {
                start_key, end_key, ..
            },
            EntityRow::RelUnkeyed { start, end, .. },
        ) => {
            let mut encoded = ordered(start, start_key)?;
            encoded.extend(ordered(end, end_key)?);
            encoded
        }
        _ => {
            return Err(LoadError::Push(
                "an unkeyed CREATE row has no remote identity".to_string(),
            ))
        }
    };
    Ok(canonical_key(&encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(statement: Option<&str>, rows: usize) -> PlannedEntity {
        PlannedEntity {
            kind: EntityKind::Dangling,
            merge_key: Vec::new(),
            statement: statement.map(str::to_string),
            rows: (0..rows)
                .map(|_| EntityRow::Node {
                    key: BTreeMap::new(),
                    props: BTreeMap::new(),
                })
                .collect(),
            skipped_unkeyed: 0,
            skipped_dangling: 0,
        }
    }

    /// Rows without a statement are a plan invariant violation, reported
    /// loudly; no rows means nothing to write, with or without a statement.
    #[test]
    fn writable_statement_pins_the_plan_invariant() {
        assert_eq!(
            writable_statement(&entity(Some("S"), 1)).unwrap(),
            Some("S")
        );
        assert_eq!(writable_statement(&entity(Some("S"), 0)).unwrap(), None);
        assert_eq!(writable_statement(&entity(None, 0)).unwrap(), None);
        let error = writable_statement(&entity(None, 2)).unwrap_err();
        assert!(error.to_string().contains("without a statement"), "{error}");
    }
}
