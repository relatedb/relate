//! The statement text and the row-parameter contract (ADR-0401 §2):
//! the `UNWIND $rows AS row …` statements per entity kind, and the
//! parameter row body each statement reads — one home for the pair, so
//! the fields a row carries and the statement that reads them cannot
//! drift.

use std::collections::BTreeMap;

use relate::Value;

use super::plan::{IdentityField, PlanRow};
use super::policy::UpdatePolicy;
use super::wire::{encode_props, quote_ident};
use serde_json::Value as Json;

use crate::LoadError;

/// The parameter-map projection for one key under `row.<field>`:
/// `` `p`: row.<field>.`p` `` joined by commas, in the key's order.
pub(crate) fn key_map(field: &str, key: &[String]) -> Result<String, LoadError> {
    key.iter()
        .map(|prop| {
            let ident = quote_ident(prop)?;
            Ok(format!("{ident}: row.{field}.{ident}"))
        })
        .collect::<Result<Vec<_>, LoadError>>()
        .map(|parts| parts.join(", "))
}

/// The labels of a node group as pattern text: `` `A`:`B` `` — every
/// label the group's nodes carry, so secondary labels survive the push
/// (LABS-907). An empty set is the labelless spelling.
fn quoted_labels(labels: &[String]) -> Result<String, LoadError> {
    let quoted = labels
        .iter()
        .map(|label| quote_ident(label))
        .collect::<Result<Vec<_>, LoadError>>()?;
    Ok(quoted.join(":"))
}

/// The node pattern for a label set with an optional in-pattern key
/// map: `(n)`, `(n:`A`:`B`)`, `(n {…})`, `(n:`A`:`B` {…})` — the ONE
/// place the labelless spelling (an empty set omits the `:`) is
/// decided, shared by the pre-read, the CREATE and the MERGE shapes.
fn node_pattern(labels: &[String], key_map: Option<&str>) -> Result<String, LoadError> {
    let labels = quoted_labels(labels)?;
    let body = match key_map {
        Some(map) => format!(" {{{map}}}"),
        None => String::new(),
    };
    if labels.is_empty() {
        Ok(format!("(n{body})"))
    } else {
        Ok(format!("(n:{labels}{body})"))
    }
}

/// The node pattern for a label set: `(n:`A`:`B`)`, or the labelless
/// `(n)`.
pub(crate) fn label_pattern(labels: &[String]) -> Result<String, LoadError> {
    node_pattern(labels, None)
}

/// The pre-read's relationship pattern `(a:`S`)-[r:`T`]->(b:`E`)` — the
/// grammar's one home beside the write statements' endpoint matches.
pub(crate) fn rel_pattern(
    rel_type: &str,
    start_label: &str,
    end_label: &str,
) -> Result<String, LoadError> {
    Ok(format!(
        "(a:{})-[r:{}]->(b:{})",
        quote_ident(start_label)?,
        quote_ident(rel_type)?,
        quote_ident(end_label)?
    ))
}

/// The whole SET clause from its comma-joined items — statement.rs
/// owns every Cypher keyword; the policy contributes only its item.
fn set_clause(items: &str) -> String {
    format!("SET {items}")
}

/// `UNWIND $rows AS row MERGE (n:`A` {`p`: row.key.`p`}) SET
/// n:`B`:`C`, … SET …` (ADR-0401 §1): the merge identity is the set's
/// KEYED label alone (the label the constraint/config keys), and the
/// secondary labels are SET onto the node the merge found — never
/// spelled into the MERGE pattern. A multi-label MERGE
/// (`MERGE (n:`A`:`B` {key})`) only matches nodes carrying BOTH labels,
/// so a node that exists remotely under the keyed label but without the
/// secondary (pushed before the label was added locally, or seeded
/// otherwise) would not match and the merge would FORK a second node
/// with the label combination — an update was meant (LABS-907
/// follow-up). Merging on the keyed label makes a local label-set
/// change an update: the secondaries are added to the existing node.
/// The `SET` operator comes from the scoped policy.
pub(crate) fn node_merge_statement(
    labels: &[String],
    keyed_label: &str,
    key: &[String],
    policy: UpdatePolicy,
) -> Result<String, LoadError> {
    if key.is_empty() {
        return Err(LoadError::Push(format!(
            "the keyed node group {keyed_label:?} has an empty merge key"
        )));
    }
    if !labels.iter().any(|label| label.as_str() == keyed_label) {
        return Err(LoadError::Push(format!(
            "the keyed label {keyed_label:?} is not among the group's labels {labels:?}"
        )));
    }
    // The partition lives here, the one place it is derived: the
    // secondaries are the group's label set minus the keyed label —
    // SET onto the node the keyed-label merge found.
    let secondary_labels: Vec<String> = labels
        .iter()
        .filter(|candidate| candidate.as_str() != keyed_label)
        .cloned()
        .collect();
    let map = key_map("key", key)?;
    // One SET, assembled here — the statement module's grammar: the
    // secondary labels as a label item (`n:`A`:`B``, an empty set
    // omits the item) ahead of the policy's own item.
    let mut items = Vec::new();
    if !secondary_labels.is_empty() {
        items.push(format!("n:{}", quoted_labels(&secondary_labels)?));
    }
    items.push(policy.set_item("n"));
    Ok(format!(
        "UNWIND $rows AS row MERGE {} {}",
        node_pattern(&[keyed_label.to_owned()], Some(&map))?,
        set_clause(&items.join(", "))
    ))
}

/// `UNWIND $rows AS row CREATE (n:`A`:`B`) SET …` — the
/// `--unkeyed-labels create` escape hatch (non-idempotent).
pub(crate) fn node_create_statement(
    labels: &[String],
    policy: UpdatePolicy,
) -> Result<String, LoadError> {
    Ok(format!(
        "UNWIND $rows AS row CREATE {} {}",
        label_pattern(labels)?,
        set_clause(&policy.set_item("n"))
    ))
}

/// How a rel group is written (ADR-0401 §2/§5), the mode and the
/// resolved key collapsed into the three shapes the statement spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelWrite<'a> {
    /// `CREATE` — the non-idempotent escape hatch.
    Create,
    /// `MERGE` on the type's key properties (keyed mode, a key resolved:
    /// override or constraint).
    MergeOnKey(&'a [String]),
    /// `MERGE` on the endpoint pair alone (structural mode, or keyed with
    /// no resolved key — the structural fallback, ADR-0401 §5).
    MergeOnPair,
}

impl<'a> RelWrite<'a> {
    /// The key properties the write merges on, when it has any.
    pub(crate) fn key(self) -> Option<&'a [String]> {
        match self {
            RelWrite::MergeOnKey(key) => Some(key),
            RelWrite::Create | RelWrite::MergeOnPair => None,
        }
    }
}

/// The rel statement (ADR-0401 §2/§5): endpoints matched by their locally
/// resolved keys, then merged structurally, on the type's key properties,
/// or created.
pub(crate) fn rel_statement(
    rel_type: &str,
    start: (&str, &[String]),
    end: (&str, &[String]),
    write: RelWrite<'_>,
    policy: UpdatePolicy,
) -> Result<String, LoadError> {
    // The endpoint MATCH binds `a`/`b` but reads the row's `start`/`end`
    // key maps — the write rows carry the endpoints under those names.
    let match_endpoint =
        |var: &str, field: &str, (label, key): (&str, &[String])| -> Result<String, LoadError> {
            Ok(format!(
                "MATCH ({var}:{} {{{}}})",
                quote_ident(label)?,
                key_map(field, key)?
            ))
        };
    // The write decides two independent facts — the verb and whether the
    // property-key map is spelled — so the pattern is spelled once.
    let verb = match write {
        RelWrite::Create => "CREATE",
        RelWrite::MergeOnKey(_) | RelWrite::MergeOnPair => "MERGE",
    };
    let key = match write.key() {
        Some(key) => format!(" {{{}}}", key_map("key", key)?),
        None => String::new(),
    };
    let merge = format!("{verb} (a)-[r:{}{key}]->(b)", quote_ident(rel_type)?);
    Ok(format!(
        "UNWIND $rows AS row {} {} {merge} {}",
        match_endpoint("a", "start", start)?,
        match_endpoint("b", "end", end)?,
        set_clause(&policy.set_item("r"))
    ))
}

/// The parameter row one applying planned row becomes (ADR-0401 §2): every
/// value a typed parameter envelope; keys, endpoints, and the payload in
/// the maps the statement reads. `name` is the entity's display name,
/// carried into encoding errors.
pub(crate) fn write_row(name: &str, row: &PlanRow) -> Result<Json, LoadError> {
    // Every container in a typed request body is itself an envelope: the
    // row's inner maps (key, props, start, end) travel as `Map` envelopes,
    // their values as typed envelopes (verified on Aura — a plain JSON
    // object nested in the typed structure reads as a typed value whose
    // `$type` is null: "Type null is not supported").
    let enc = |props: &BTreeMap<String, Value>| {
        encode_props(props).map_err(|e| e.with_context(&format!("entity {name}: ")))
    };
    let mut body = serde_json::Map::new();
    // Each map the row's shape carries is a field the statement reads:
    // the endpoints of a rel, then the merge key (a keyed node, or the
    // keyed mode's resolved rel key — the only shapes reading row.key).
    for (field, identity) in [
        ("start", IdentityField::Start),
        ("end", IdentityField::End),
        ("key", IdentityField::Key),
    ] {
        if let Some(map) = row.identity_map(identity) {
            body.insert(field.into(), enc(map)?);
        }
    }
    body.insert("props".into(), enc(&row.props)?);
    Ok(Json::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The node statement shape under every policy: constraint-derived
    /// merge key in the MERGE map, the policy's SET operator, every value a
    /// parameter.
    #[test]
    fn node_statement_shapes_follow_the_policy() {
        let merge_key = ["name".to_string()];
        let person = ["Person".to_string()];
        assert_eq!(
            node_merge_statement(&person, "Person", &merge_key, UpdatePolicy::Add).unwrap(),
            "UNWIND $rows AS row MERGE (n:`Person` {`name`: row.key.`name`}) SET n += row.props"
        );
        assert_eq!(
            node_merge_statement(&person, "Person", &merge_key, UpdatePolicy::Replace).unwrap(),
            "UNWIND $rows AS row MERGE (n:`Person` {`name`: row.key.`name`}) SET n = row.props"
        );
        assert_eq!(
            node_merge_statement(&person, "Person", &merge_key, UpdatePolicy::Skip).unwrap(),
            "UNWIND $rows AS row MERGE (n:`Person` {`name`: row.key.`name`}) SET n = row.props"
        );
        // A composite key names every property.
        assert_eq!(
            node_merge_statement(
                &["Event".to_string()],
                "Event",
                &["tenant".to_string(), "code".to_string()],
                UpdatePolicy::Add
            )
            .unwrap(),
            "UNWIND $rows AS row MERGE (n:`Event` {`tenant`: row.key.`tenant`, `code`: row.key.`code`}) SET n += row.props"
        );
        assert!(node_merge_statement(&person, "Person", &[], UpdatePolicy::Add).is_err());
        // The keyed label must be one of the group's labels — the
        // partition is derived, not assumed.
        assert!(node_merge_statement(&person, "Movie", &merge_key, UpdatePolicy::Add).is_err());
    }

    /// Secondary labels survive the push (LABS-907): the merge identity
    /// is the keyed label alone and the secondaries are SET onto the
    /// node the merge found, so a re-push updates the same node instead
    /// of forking one per label combination — and the unkeyed CREATE
    /// escape hatch still writes every label in its CREATE pattern.
    #[test]
    fn node_statements_carry_secondary_labels() {
        // Chunk is the keyed label: the merge identity; SessionChunk is
        // SET onto the node the merge found — an existing :Chunk {id}
        // gains the label instead of being forked into a second node.
        assert_eq!(
            node_merge_statement(
                &["Chunk".to_string(), "SessionChunk".to_string()],
                "Chunk",
                &["id".to_string()],
                UpdatePolicy::Add
            )
            .unwrap(),
            "UNWIND $rows AS row MERGE (n:`Chunk` {`id`: row.key.`id`}) SET n:`SessionChunk`, n += row.props"
        );
        // Several secondaries, in the set's sorted order.
        assert_eq!(
            node_merge_statement(
                &["Admin".to_string(), "Person".to_string(), "Staff".to_string()],
                "Person",
                &["name".to_string()],
                UpdatePolicy::Add
            )
            .unwrap(),
            "UNWIND $rows AS row MERGE (n:`Person` {`name`: row.key.`name`}) SET n:`Admin`:`Staff`, n += row.props"
        );
        assert_eq!(
            node_create_statement(
                &["Chunk".to_string(), "SessionChunk".to_string()],
                UpdatePolicy::Add
            )
            .unwrap(),
            "UNWIND $rows AS row CREATE (n:`Chunk`:`SessionChunk`) SET n += row.props"
        );
        // The labelless spellings are unchanged.
        assert_eq!(
            node_create_statement(&[], UpdatePolicy::Add).unwrap(),
            "UNWIND $rows AS row CREATE (n) SET n += row.props"
        );
        // Every label is identifier-quoted — a secondary label with a
        // space cannot break the pattern out of its backticks.
        let odd = ["Odd Label".to_string()];
        assert_eq!(
            node_create_statement(&odd, UpdatePolicy::Add).unwrap(),
            "UNWIND $rows AS row CREATE (n:`Odd Label`) SET n += row.props"
        );
    }

    /// The rel statement shape under every mode: endpoints matched by their
    /// locally resolved keys, property-keyed MERGE when a key is resolved
    /// under the keyed default, structural as the forced/fallback shape,
    /// CREATE as the escape hatch.
    #[test]
    fn rel_statement_shapes_follow_the_mode() {
        let start = ("Person", &["name".to_string()][..]);
        let end = ("Event", &["tenant".to_string(), "code".to_string()][..]);
        let rel_key = &["slot".to_string()][..];
        let compound = &["tenant".to_string(), "slot".to_string()][..];
        // Keyed (the default) with a resolved key: property-keyed MERGE.
        assert_eq!(
            rel_statement(
                "SPOKE_AT",
                start,
                end,
                RelWrite::MergeOnKey(rel_key),
                UpdatePolicy::Add,
            )
            .unwrap(),
            "UNWIND $rows AS row MATCH (a:`Person` {`name`: row.start.`name`}) MATCH (b:`Event` {`tenant`: row.end.`tenant`, `code`: row.end.`code`}) MERGE (a)-[r:`SPOKE_AT` {`slot`: row.key.`slot`}]->(b) SET r += row.props"
        );
        // Keyed with a compound key: every key property in the MERGE map.
        assert_eq!(
            rel_statement(
                "SPOKE_AT",
                start,
                end,
                RelWrite::MergeOnKey(compound),
                UpdatePolicy::Add,
            )
            .unwrap(),
            "UNWIND $rows AS row MATCH (a:`Person` {`name`: row.start.`name`}) MATCH (b:`Event` {`tenant`: row.end.`tenant`, `code`: row.end.`code`}) MERGE (a)-[r:`SPOKE_AT` {`tenant`: row.key.`tenant`, `slot`: row.key.`slot`}]->(b) SET r += row.props"
        );
        // Keyed with no resolved key: the structural fallback (no error).
        assert_eq!(
            rel_statement(
                "SPOKE_AT",
                start,
                end,
                RelWrite::MergeOnPair,
                UpdatePolicy::Add,
            )
            .unwrap(),
            "UNWIND $rows AS row MATCH (a:`Person` {`name`: row.start.`name`}) MATCH (b:`Event` {`tenant`: row.end.`tenant`, `code`: row.end.`code`}) MERGE (a)-[r:`SPOKE_AT`]->(b) SET r += row.props"
        );
        assert_eq!(
            rel_statement(
                "SPOKE_AT",
                start,
                end,
                RelWrite::Create,
                UpdatePolicy::Replace,
            )
            .unwrap(),
            "UNWIND $rows AS row MATCH (a:`Person` {`name`: row.start.`name`}) MATCH (b:`Event` {`tenant`: row.end.`tenant`, `code`: row.end.`code`}) CREATE (a)-[r:`SPOKE_AT`]->(b) SET r = row.props"
        );
    }

    /// A row body carries exactly the maps its shape has: a keyed node
    /// `key` + `props`, a created node `props`, a pair rel `start` + `end`
    /// + `props`, a keyed rel all four.
    #[test]
    fn write_row_carries_the_maps_of_the_row_shape() {
        let map = |k: &str| BTreeMap::from([(k.to_string(), Value::Int(1))]);
        let fields = |row: PlanRow| -> Vec<String> {
            let Json::Object(body) = write_row("e", &row).unwrap() else {
                panic!("a row body is an object");
            };
            let mut names: Vec<String> = body.keys().cloned().collect();
            names.sort();
            names
        };
        assert_eq!(
            fields(PlanRow::keyed_node(map("k"), map("p"))),
            ["key", "props"]
        );
        assert_eq!(fields(PlanRow::created_node(map("p"))), ["props"]);
        assert_eq!(
            fields(PlanRow::pair_rel(map("a"), map("b"), map("p"))),
            ["end", "props", "start"]
        );
        assert_eq!(
            fields(PlanRow::keyed_rel(map("a"), map("b"), map("k"), map("p"))),
            ["end", "key", "props", "start"]
        );
    }
}
