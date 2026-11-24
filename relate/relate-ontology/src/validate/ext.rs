//! Deep validation of the schema's typed extension payloads (ADR-0364
//! §2, on the 1.1.0 key spellings of ADR-0487 §2), in place over the
//! retained root [`Value`] tree.
//!
//! The spec defines node and relationship indexes, the index-type tokens
//! and their tagged `options` union, node display, data-source tables
//! with columns and foreign-key references, and the
//! `NODE`/`QUERY`/`RELATIONSHIP` mapping union with property and target
//! mappings. The checks here mirror those definitions — closed members,
//! required fields, enum and scalar types — so a malformed payload the
//! schema rejects does not pass `set`. `custom` members are the one
//! exception: the schema defines them as arbitrary JSON objects, so only
//! the object shape is checked.
//!
//! Nothing validated here is ever converted (ADR-0364 §5.1), so there is
//! no typed extension tree: the document's root value tree is the one
//! carrier, and these checks only prove the payloads spec-shaped. Every
//! walk runs under the caller's [`UnknownMembers`] policy like the
//! document walk itself.

use indexmap::IndexMap;

use super::read::{
    check_allowed, expect_object, join, number_list, opt_bool, opt_int, opt_list, opt_neo4j_type,
    opt_neo4j_type_list, opt_nullable_int, opt_nullable_number, opt_nullable_string,
    opt_nullable_sub, opt_string, opt_string_list, opt_sub, read_list, req_number, req_string,
    req_string_list, req_sub, verr, UnknownMembers,
};
use super::validate_custom_object;
use crate::error::{Issue, IssueCode};
use crate::value::Value;

/// The closed key sets of the extension envelopes, shared with the
/// canonicalize path's grafts so the drop rule and the canonical member
/// order read one table per envelope.
pub(crate) const NODE_ENVELOPE_KEYS: &[&str] = &["indexes", "display", "custom"];
pub(crate) const RELATIONSHIP_ENVELOPE_KEYS: &[&str] = &["indexes", "custom"];
pub(crate) const DOCUMENT_ENVELOPE_KEYS: &[&str] =
    &["data_source_tables", "data_source_mappings", "custom"];
/// The envelope of the members whose only extension is `custom`:
/// `labels`, `properties` and `constraints`.
pub(crate) const CUSTOM_ENVELOPE_KEYS: &[&str] = &["custom"];

const NODE_INDEX_KEYS: &[&str] = &["type", "labels", "properties", "options", "custom", "name"];
const RELATIONSHIP_INDEX_KEYS: &[&str] = &["type", "properties", "options", "custom", "name"];

const FULLTEXT_KEYS: &[&str] = &[
    "type",
    "fulltext.default_analyzer",
    "fulltext.analyzer",
    "fulltext.eventually_consistent",
    "fulltext.eventually_consistent_apply_parallelism",
    "fulltext.eventually_consistent_refresh_interval",
    "fulltext.eventually_consistent_refresh_parallelism",
    "fulltext.eventually_consistent_index_update_queue_max_length",
];
const POINT_KEYS: &[&str] = &[
    "type",
    "spatial.cartesian.min",
    "spatial.cartesian.max",
    "spatial.cartesian-3d.min",
    "spatial.cartesian-3d.max",
    "spatial.wgs-84.min",
    "spatial.wgs-84.max",
    "spatial.wgs-84-3d.min",
    "spatial.wgs-84-3d.max",
];
const VECTOR_KEYS: &[&str] = &[
    "type",
    "vector.dimensions",
    "vector.similarity_function",
    "vector.default_search_expansion_factor",
    "vector.quantization.enabled",
    "vector.quantization.type",
    "vector.hnsw.m",
    "vector.hnsw.ef_construction",
];

/// The validator a table row pairs with its `options.type` tag: it receives
/// the whole options object (with `type` among its keys), the options
/// path and the caller's unknown-member policy.
type IndexOptionValidator = fn(&IndexMap<String, Value>, &str, UnknownMembers) -> Result<(), Issue>;

/// The closed `options.type` vocabulary shared by node and relationship
/// indexes, in schema order, each row pairing the tag with the validator
/// that checks its option shape. [`validate_index_option`] looks a
/// submitted tag up here and calls the matching validator;
/// [`expected_index_option_types`] renders the same rows for the error
/// message — the pattern [`CONSTRAINTS`](crate::model::CONSTRAINTS) and
/// `ConstraintType::parse` follow.
pub(super) const INDEX_OPTION_TYPES: &[(&str, IndexOptionValidator)] = &[
    ("FULLTEXT", validate_fulltext_option),
    ("POINT", validate_point_option),
    ("VECTOR", validate_vector_option),
];

const DISPLAY_KEYS: &[&str] = &["x", "y", "colour", "custom"];
const TABLE_KEYS: &[&str] = &[
    "source",
    "columns",
    "primary_keys",
    "foreign_keys",
    "custom",
];
const TABLE_COLUMN_KEYS: &[&str] = &[
    "name",
    "type",
    "size",
    "suggested",
    "supported",
    "nullable",
    "dimension",
    "custom",
];
const FOREIGN_KEY_KEYS: &[&str] = &["columns", "references", "custom"];
const FOREIGN_KEY_REFERENCE_KEYS: &[&str] = &["table", "columns", "custom"];

const NODE_MAPPING_KEYS: &[&str] = &[
    "type",
    "node",
    "table",
    "properties",
    "mode",
    "match_label",
    "key",
    "depends_on",
    "id",
];
const QUERY_MAPPING_KEYS: &[&str] = &["type", "table", "query", "depends_on", "id"];
const RELATIONSHIP_MAPPING_KEYS: &[&str] = &[
    "type",
    "relationship",
    "table",
    "from_node",
    "to_node",
    "match_label",
    "properties",
    "mode",
    "key",
    "depends_on",
    "id",
];
const PROPERTY_MAPPING_KEYS: &[&str] = &["column"];
const TARGET_MAPPING_KEYS: &[&str] = &["label", "properties"];

/// One id-keyed entry map: `what` must be an object and every member is
/// validated at `<path>/<id>`. The simple entry maps — node and
/// relationship `indexes`, document `data_source_tables`, `foreign_keys`
/// and property mappings — share it instead of open-coding the loop.
/// `entry` receives one member value, that member's path and the
/// caller's policy.
fn validate_entry_map(
    value: &Value,
    path: &str,
    what: &str,
    policy: UnknownMembers,
    entry: impl Fn(&Value, &str, UnknownMembers) -> Result<(), Issue>,
) -> Result<(), Issue> {
    let obj = expect_object(value, path, what)?;
    for (id, entry_value) in obj {
        entry(entry_value, &join(path, id), policy)?;
    }
    Ok(())
}

/// A document's `extensions`: `data_source_tables`,
/// `data_source_mappings` (the 1.1.0 spellings, ADR-0487 §2) and a
/// `custom` object.
pub(super) fn validate_document_extensions(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    let obj = expect_object(value, path, "'extensions'")?;
    check_allowed(obj, DOCUMENT_ENVELOPE_KEYS, path, policy)?;
    opt_sub(obj, "data_source_tables", path, |v, p| {
        validate_tables(v, p, policy)
    })?;
    opt_sub(obj, "data_source_mappings", path, |v, p| {
        validate_mappings(v, p, policy)
    })?;
    validate_custom_object(obj, path)?;
    Ok(())
}

/// A node entry's `extensions`: `indexes`, optional `display` and `custom`.
pub(super) fn validate_node_extensions(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    let obj = expect_object(value, path, "'extensions'")?;
    check_allowed(obj, NODE_ENVELOPE_KEYS, path, policy)?;
    opt_sub(obj, "indexes", path, |v, p| {
        validate_node_indexes(v, p, policy)
    })?;
    opt_nullable_sub(obj, "display", path, |v, p| {
        validate_node_display(v, p, policy)
    })?;
    validate_custom_object(obj, path)?;
    Ok(())
}

/// A relationship entry's `extensions`: `indexes` and `custom`.
pub(super) fn validate_relationship_extensions(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    let obj = expect_object(value, path, "'extensions'")?;
    check_allowed(obj, RELATIONSHIP_ENVELOPE_KEYS, path, policy)?;
    opt_sub(obj, "indexes", path, |v, p| {
        validate_relationship_indexes(v, p, policy)
    })?;
    validate_custom_object(obj, path)?;
    Ok(())
}

fn validate_node_indexes(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    validate_entry_map(value, path, "'indexes'", policy, validate_node_index)
}

fn validate_node_index(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    let obj = expect_object(value, path, "a node index")?;
    check_allowed(obj, NODE_INDEX_KEYS, path, policy)?;
    validate_index_type(obj, path)?;
    req_string_list(obj, "labels", path)?;
    req_string_list(obj, "properties", path)?;
    opt_nullable_sub(obj, "options", path, |v, p| {
        validate_index_option(v, p, policy)
    })?;
    validate_custom_object(obj, path)?;
    opt_nullable_string(obj, "name", path)?;
    Ok(())
}

fn validate_relationship_indexes(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    validate_entry_map(
        value,
        path,
        "'indexes'",
        policy,
        validate_relationship_index,
    )
}

fn validate_relationship_index(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    let obj = expect_object(value, path, "a relationship index")?;
    check_allowed(obj, RELATIONSHIP_INDEX_KEYS, path, policy)?;
    validate_index_type(obj, path)?;
    req_string_list(obj, "properties", path)?;
    opt_nullable_sub(obj, "options", path, |v, p| {
        validate_index_option(v, p, policy)
    })?;
    validate_custom_object(obj, path)?;
    opt_nullable_string(obj, "name", path)?;
    Ok(())
}

/// The closed index `type` vocabulary shared by node and relationship
/// indexes, in schema order. [`validate_index_type`] checks a submitted
/// token against this table and [`expected_index_types`] renders the
/// expected-list message from it — the pattern
/// [`INDEX_OPTION_TYPES`] and [`CONSTRAINTS`](crate::model::CONSTRAINTS)
/// follow, so a seventh token is one row, not two literals.
const INDEX_TYPES: &[&str] = &["FULLTEXT", "POINT", "RANGE", "TEXT", "VECTOR", "LOOKUP"];

fn validate_index_type(obj: &IndexMap<String, Value>, path: &str) -> Result<(), Issue> {
    let token = req_string(obj, "type", path)?;
    if INDEX_TYPES.contains(&token.as_str()) {
        return Ok(());
    }
    Err(verr(
        IssueCode::InvalidEnumToken,
        &join(path, "type"),
        format!(
            "invalid index type '{token}': expected {}",
            expected_index_types()
        ),
    ))
}

/// The one expected-token list renderer: the tokens quoted,
/// comma-joined, the tail behind an `or` — `'A', 'B' or 'C'` — with a
/// one-token vocabulary rendering as just `'A'`. Both index
/// vocabularies' expected-list messages derive from it, each over its
/// own projection.
fn quoted_or_list<'a>(tokens: impl IntoIterator<Item = &'a str>) -> String {
    let quoted: Vec<String> = tokens
        .into_iter()
        .map(|token| format!("'{token}'"))
        .collect();
    let (last, rest) = quoted
        .split_last()
        .expect("an expected-token vocabulary is never empty");
    let head = rest.join(", ");
    if head.is_empty() {
        last.clone()
    } else {
        format!("{head} or {last}")
    }
}

/// The expected-token list for index `type` fields, derived from
/// [`INDEX_TYPES`] in the message's `'A', 'B' or 'C'` form.
fn expected_index_types() -> String {
    quoted_or_list(INDEX_TYPES.iter().copied())
}

/// The expected-token list for index `options.type` fields, derived from
/// [`INDEX_OPTION_TYPES`] in the message's `'A', 'B' or 'C'` form.
fn expected_index_option_types() -> String {
    quoted_or_list(INDEX_OPTION_TYPES.iter().map(|(token, _)| *token))
}

/// A node or relationship index's `options`: an object tagged by
/// `options.type`, whose members are decided by that tag's
/// [`INDEX_OPTION_TYPES`] row.
pub(super) fn validate_index_option(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    let obj = expect_object(value, path, "'options'")?;
    let tag = req_string(obj, "type", path)?;
    let validate = INDEX_OPTION_TYPES
        .iter()
        .find_map(|(candidate, validate)| (*candidate == tag).then_some(*validate))
        .ok_or_else(|| {
            verr(
                IssueCode::InvalidEnumToken,
                &join(path, "type"),
                format!(
                    "invalid index options type '{tag}': expected {}",
                    expected_index_option_types()
                ),
            )
        })?;
    validate(obj, path, policy)
}

/// `options.type: 'FULLTEXT'` — the `fulltext.*` members (main's
/// `FulltextIndexOptions`).
fn validate_fulltext_option(
    obj: &IndexMap<String, Value>,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    check_allowed(obj, FULLTEXT_KEYS, path, policy)?;
    opt_string(obj, "fulltext.default_analyzer", path)?;
    opt_nullable_string(obj, "fulltext.analyzer", path)?;
    opt_bool(obj, "fulltext.eventually_consistent", path)?;
    opt_int(
        obj,
        "fulltext.eventually_consistent_apply_parallelism",
        path,
    )?;
    opt_string(obj, "fulltext.eventually_consistent_refresh_interval", path)?;
    opt_int(
        obj,
        "fulltext.eventually_consistent_refresh_parallelism",
        path,
    )?;
    opt_int(
        obj,
        "fulltext.eventually_consistent_index_update_queue_max_length",
        path,
    )?;
    Ok(())
}

/// `options.type: 'POINT'` — the `spatial.*` members (main's
/// `PointIndexOptions`).
fn validate_point_option(
    obj: &IndexMap<String, Value>,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    check_allowed(obj, POINT_KEYS, path, policy)?;
    opt_list(obj, "spatial.cartesian.min", path, number_list)?;
    opt_list(obj, "spatial.cartesian.max", path, number_list)?;
    opt_list(obj, "spatial.cartesian-3d.min", path, number_list)?;
    opt_list(obj, "spatial.cartesian-3d.max", path, number_list)?;
    opt_list(obj, "spatial.wgs-84.min", path, number_list)?;
    opt_list(obj, "spatial.wgs-84.max", path, number_list)?;
    opt_list(obj, "spatial.wgs-84-3d.min", path, number_list)?;
    opt_list(obj, "spatial.wgs-84-3d.max", path, number_list)?;
    Ok(())
}

/// `options.type: 'VECTOR'` — the `vector.*` members (main's
/// `VectorIndexOptions`).
fn validate_vector_option(
    obj: &IndexMap<String, Value>,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    check_allowed(obj, VECTOR_KEYS, path, policy)?;
    opt_nullable_int(obj, "vector.dimensions", path)?;
    opt_string(obj, "vector.similarity_function", path)?;
    opt_nullable_number(obj, "vector.default_search_expansion_factor", path)?;
    opt_bool(obj, "vector.quantization.enabled", path)?;
    opt_string(obj, "vector.quantization.type", path)?;
    opt_int(obj, "vector.hnsw.m", path)?;
    opt_int(obj, "vector.hnsw.ef_construction", path)?;
    Ok(())
}

fn validate_node_display(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    let obj = expect_object(value, path, "'display'")?;
    check_allowed(obj, DISPLAY_KEYS, path, policy)?;
    req_number(obj, "x", path)?;
    req_number(obj, "y", path)?;
    opt_nullable_string(obj, "colour", path)?;
    validate_custom_object(obj, path)?;
    Ok(())
}

fn validate_tables(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    validate_entry_map(value, path, "'data_source_tables'", policy, validate_table)
}

fn validate_table(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    let obj = expect_object(value, path, "a table")?;
    check_allowed(obj, TABLE_KEYS, path, policy)?;
    req_string(obj, "source", path)?;
    opt_sub(obj, "columns", path, |v, p| {
        validate_table_columns(v, p, policy)
    })?;
    opt_string_list(obj, "primary_keys", path)?;
    opt_sub(obj, "foreign_keys", path, |v, p| {
        validate_foreign_keys(v, p, policy)
    })?;
    validate_custom_object(obj, path)?;
    Ok(())
}

fn validate_table_columns(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    read_list(value, "columns", path, |item, at| {
        validate_table_column(item, at, policy)
    })
    .map(|_| ())
}

fn validate_table_column(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    let obj = expect_object(value, path, "a table column")?;
    check_allowed(obj, TABLE_COLUMN_KEYS, path, policy)?;
    opt_string(obj, "name", path)?;
    opt_string(obj, "type", path)?;
    opt_int(obj, "size", path)?;
    opt_neo4j_type(obj, "suggested", path)?;
    opt_neo4j_type_list(obj, "supported", path)?;
    // Whether the column can hold null values (the 1.1.0 shape's new
    // member, ADR-0487 §2): a plain optional boolean.
    opt_bool(obj, "nullable", path)?;
    opt_nullable_int(obj, "dimension", path)?;
    validate_custom_object(obj, path)?;
    Ok(())
}

fn validate_foreign_keys(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    validate_entry_map(
        value,
        path,
        "'foreign_keys'",
        policy,
        |entry, path, policy| {
            let obj = expect_object(entry, path, "a foreign key")?;
            check_allowed(obj, FOREIGN_KEY_KEYS, path, policy)?;
            req_string_list(obj, "columns", path)?;
            req_sub(obj, "references", path, |v, p| {
                validate_foreign_key_reference(v, p, policy)
            })?;
            validate_custom_object(obj, path)
        },
    )
}

fn validate_foreign_key_reference(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    let obj = expect_object(value, path, "'references'")?;
    check_allowed(obj, FOREIGN_KEY_REFERENCE_KEYS, path, policy)?;
    req_string(obj, "table", path)?;
    opt_string_list(obj, "columns", path)?;
    validate_custom_object(obj, path)?;
    Ok(())
}

fn validate_mappings(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    validate_entry_map(
        value,
        path,
        "'data_source_mappings'",
        policy,
        |entry, path, policy| {
            // The walked value is one entry of the map, not the field the
            // outer path names, so the array sentence names the entry, not
            // the map's key.
            read_list(entry, "data_source_mappings entry", path, |item, at| {
                validate_mapping(item, at, policy)
            })
            .map(|_| ())
        },
    )
}

/// The closed mapping `type` vocabulary, in schema order.
/// [`validate_mapping`]'s dispatch arms name the tokens and
/// [`expected_mapping_types`] renders the expected-list message from
/// this table — the pattern [`INDEX_TYPES`] and
/// [`INDEX_OPTION_TYPES`] follow, so a fourth token is one row here and
/// one dispatch arm, never a second message spelling.
const MAPPING_TYPES: &[&str] = &["NODE", "QUERY", "RELATIONSHIP"];

/// The expected-token list for a mapping `type`, derived from
/// [`MAPPING_TYPES`] in the message's `'A', 'B' or 'C'` form.
fn expected_mapping_types() -> String {
    quoted_or_list(MAPPING_TYPES.iter().copied())
}

/// The closed mapping `mode` vocabulary, in schema order.
/// [`validate_mapping_mode`] checks a submitted token against this
/// table and [`expected_mapping_modes`] renders the expected-list
/// message from it.
const MAPPING_MODES: &[&str] = &["MERGE", "CREATE"];

/// The expected-token list for a mapping `mode`, derived from
/// [`MAPPING_MODES`] in the message's `'A' or 'B'` form.
fn expected_mapping_modes() -> String {
    quoted_or_list(MAPPING_MODES.iter().copied())
}

fn validate_mapping(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    let obj = expect_object(value, path, "a mapping")?;
    match req_string(obj, "type", path)?.as_str() {
        "NODE" => {
            check_allowed(obj, NODE_MAPPING_KEYS, path, policy)?;
            req_string(obj, "node", path)?;
            req_string(obj, "table", path)?;
            req_sub(obj, "properties", path, |v, p| {
                validate_property_mappings(v, p, policy)
            })?;
            validate_mapping_mode(obj, path)?;
            opt_nullable_string(obj, "match_label", path)?;
            opt_string_list(obj, "key", path)?;
            opt_string(obj, "depends_on", path)?;
            opt_string(obj, "id", path)?;
            Ok(())
        }
        "QUERY" => {
            check_allowed(obj, QUERY_MAPPING_KEYS, path, policy)?;
            req_string(obj, "table", path)?;
            req_string(obj, "query", path)?;
            opt_string(obj, "depends_on", path)?;
            opt_string(obj, "id", path)?;
            Ok(())
        }
        "RELATIONSHIP" => {
            check_allowed(obj, RELATIONSHIP_MAPPING_KEYS, path, policy)?;
            req_string(obj, "relationship", path)?;
            req_string(obj, "table", path)?;
            req_sub(obj, "from_node", path, |v, p| {
                validate_target_mapping(v, p, policy)
            })?;
            req_sub(obj, "to_node", path, |v, p| {
                validate_target_mapping(v, p, policy)
            })?;
            opt_nullable_string(obj, "match_label", path)?;
            opt_sub(obj, "properties", path, |v, p| {
                validate_property_mappings(v, p, policy)
            })?;
            validate_mapping_mode(obj, path)?;
            opt_string_list(obj, "key", path)?;
            opt_string(obj, "depends_on", path)?;
            opt_string(obj, "id", path)?;
            Ok(())
        }
        other => Err(verr(
            IssueCode::InvalidEnumToken,
            &join(path, "type"),
            format!(
                "invalid mapping type '{other}': expected {}",
                expected_mapping_types()
            ),
        )),
    }
}

fn validate_mapping_mode(obj: &IndexMap<String, Value>, path: &str) -> Result<(), Issue> {
    let Some(token) = opt_string(obj, "mode", path)? else {
        return Ok(());
    };
    if MAPPING_MODES.contains(&token.as_str()) {
        return Ok(());
    }
    Err(verr(
        IssueCode::InvalidEnumToken,
        &join(path, "mode"),
        format!(
            "invalid mapping mode '{token}': expected {}",
            expected_mapping_modes()
        ),
    ))
}

fn validate_property_mappings(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    validate_entry_map(
        value,
        path,
        "'properties'",
        policy,
        |entry, path, policy| {
            let obj = expect_object(entry, path, "a property mapping")?;
            check_allowed(obj, PROPERTY_MAPPING_KEYS, path, policy)?;
            req_string(obj, "column", path)?;
            Ok(())
        },
    )
}

fn validate_target_mapping(value: &Value, path: &str, policy: UnknownMembers) -> Result<(), Issue> {
    let obj = expect_object(value, path, "a target mapping")?;
    check_allowed(obj, TARGET_MAPPING_KEYS, path, policy)?;
    opt_string(obj, "label", path)?;
    opt_sub(obj, "properties", path, |v, p| {
        validate_property_mappings(v, p, policy)
    })?;
    Ok(())
}
