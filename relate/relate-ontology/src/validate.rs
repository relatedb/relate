//! Hand-rolled structural validation against the vendored schema
//! (ADR-0487 §2, succeeding ADR-0364 §2):
//! `schema/ontology-graph-spec.schema.json` (`neo4j/graph-spec` tag
//! `release-spec/v1.1.0` @ `5ff2c20aa7e428e7c563b17f573157293959a009`).
//!
//! The checks below mirror that schema exactly: the required root fields
//! (`schema`, `version`, `id`) with `version` as a string; `name`/`description`
//! as strings; `additionalProperties: false` on every object the schema
//! closes; the one constraint shape shared by node and relationship
//! owners with the uppercase `type` enum; the closed 39-token
//! [`Neo4jType`] enum; `must_exist` (the pinned draft's `mustExist` is
//! rejected as unknown, while property `aliases` is back and accepted);
//! nullable flags, dimensions and endpoint counts with no minimums;
//! `one_of` items as strings only; and the closed per-element extension
//! envelopes, whose typed payloads are validated deeply in [`ext`] over
//! the retained root value tree (ADR-0364 §2). Node and relationship
//! entries carry no `name` member in 1.1.0, and their old spelling is
//! rejected as unknown. `format: uri` /
//! `format: regex` are annotations and are not checked, as JSON Schema
//! prescribes.
//!
//! Two revisions ADR-0667 records against that schema: a relationship
//! entry's `type` is optional here (the converter fails a type-less
//! entry at `apply`, naming it), and the walk runs under an
//! unknown-member [`UnknownMembers`] policy — the typed-parse path
//! rejects unknown members as the schema's `additionalProperties: false`
//! prescribes, while the canonicalize path drops them, so the canonical
//! form carries only the schema's members.
//!
//! Spec-valid constructs stay accepted even when the ADR-0226 converter
//! cannot express them: a missing identifier label, a label-only or empty
//! relationship target, an empty property name and an empty constraint
//! `properties` list are all validation-valid and fail later, at `apply`,
//! with [`OntologyError::Mapping`] (ADR-0364 §3). Two decisions are made
//! here, at the validation boundary: a node entry's `label` shorthand is
//! desugared into `labels.identifier` (a conflicting pair is rejected as
//! ambiguous, per ADR-0364 §3), and an endpoint's `count` may not be
//! combined with `min_count`/`max_count`.
//!
//! Issue collection is per entry map: a node or relationship entry that
//! fails contributes its first issue and the walk continues with its
//! siblings, so one bad entry does not hide the rest — the typed-parse
//! path reports the first issue found (the same sentence the fail-fast
//! walk produced), the canonicalize path renders all of them.

mod ext;
mod read;
#[cfg(test)]
mod tests;

pub(crate) use ext::{
    CUSTOM_ENVELOPE_KEYS, DOCUMENT_ENVELOPE_KEYS, NODE_ENVELOPE_KEYS, RELATIONSHIP_ENVELOPE_KEYS,
};
pub(crate) use read::UnknownMembers;

use indexmap::IndexMap;

use crate::error::{Issue, IssueCode, OntologyError, PINNED_SCHEMA_URI};
use crate::model::{
    Constraint, ConstraintType, Labels, NodeType, OntologyDocument, Property, RelationshipTarget,
    RelationshipType, CONSTRAINTS,
};
use crate::value::Value;

use read::{
    check_allowed, expect_object, join, kind_of, opt_list, opt_neo4j_type, opt_nullable_bool,
    opt_nullable_int, opt_nullable_string, opt_string, opt_string_list, opt_sub, req_string,
    req_string_list, req_sub, string_list, verr,
};

/// Allowed root keys (`additionalProperties: false` at the root), in the
/// schema's member order — the order the canonical form emits them in.
const ROOT_KEYS: &[&str] = &[
    "schema",
    "version",
    "id",
    "name",
    "description",
    "nodes",
    "relationships",
    "extensions",
];

/// Allowed keys of a node entry. 1.1.0 carries no `name` member here
/// (ADR-0487 §2); the 1.0.0 spelling is rejected as unknown.
const NODE_KEYS: &[&str] = &[
    "label",
    "labels",
    "properties",
    "constraints",
    "extensions",
    "description",
    "aliases",
    "reference",
];

/// Allowed keys of a `labels` object.
const LABELS_KEYS: &[&str] = &["identifier", "implied", "optional", "extensions"];

/// Allowed keys of a property object. The 1.1.0 shape carries `aliases`
/// (ADR-0487 §2); the pinned draft's `mustExist` stays rejected as an
/// unknown member.
const PROPERTY_KEYS: &[&str] = &[
    "type",
    "dimension",
    "must_exist",
    "unique",
    "key",
    "extensions",
    "name",
    "description",
    "aliases",
    "reference",
    "pattern",
    "one_of",
];

/// Allowed keys of a relationship entry. 1.1.0 carries no `name` member
/// here (ADR-0487 §2); the 1.0.0 spelling is rejected as unknown.
const RELATIONSHIP_KEYS: &[&str] = &[
    "type",
    "from",
    "to",
    "properties",
    "constraints",
    "extensions",
    "description",
    "aliases",
    "reference",
];

/// Allowed keys of a main `RelationshipTarget`.
const TARGET_KEYS: &[&str] = &["node", "label", "count", "min_count", "max_count"];

/// Allowed keys of a constraint object — one shape shared by node and
/// relationship owners since 1.1.0 dropped the node constraint's `label`
/// (ADR-0487 §2); the `label` spelling is rejected as unknown on both.
const CONSTRAINT_KEYS: &[&str] = &["type", "properties", "extensions", "name"];

/// The spec-version gate's one detection (ADR-0487 §1): the URI a root
/// object's string `schema` member carries when it is not the pinned
/// one. `None` for every other shape — an absent or non-string `schema`
/// is the structural walk's to report. Both document paths consult this
/// before structure: the typed parse fails with
/// [`OntologyError::UnsupportedSpecVersion`], the canonicalize path with
/// [`OntologyError::UnsupportedGraphSpecVersion`].
pub(crate) fn schema_uri_mismatch(root: &Value) -> Option<String> {
    match root.as_object()?.get("schema") {
        Some(Value::String(uri)) if uri != PINNED_SCHEMA_URI => Some(uri.clone()),
        _ => None,
    }
}

/// Validate a parsed value model into a typed [`OntologyDocument`] under
/// `policy` (the unknown-member rule; see [`UnknownMembers`]).
///
/// The spec-version gate runs before this walk (each caller renders its
/// own failure for [`schema_uri_mismatch`]). A non-string `schema` is
/// not a URI at all and falls through to the structural type error on
/// that field. The walk is fail-fast within an entry and per-entry
/// across the node and relationship maps: `Err` carries every issue
/// found, in walk order, so the first is the sentence the fail-fast
/// parse reported.
pub(crate) fn validate_document(
    root: Value,
    policy: UnknownMembers,
) -> Result<OntologyDocument, Vec<Issue>> {
    let obj = expect_object(&root, "/", "the document root")?;

    check_allowed(obj, ROOT_KEYS, "/", policy)?;
    let schema = req_string(obj, "schema", "/")?;
    let version = req_string(obj, "version", "/")?;
    let id = req_string(obj, "id", "/")?;
    let name = opt_string(obj, "name", "/")?;
    let description = opt_string(obj, "description", "/")?;
    let nodes = opt_sub(obj, "nodes", "/", |v, p| {
        validate_node_entries(v, p, policy)
    })?;
    let relationships = opt_sub(obj, "relationships", "/", |v, p| {
        validate_relationship_entries(v, p, policy)
    })?;
    opt_sub(obj, "extensions", "/", |v, p| {
        ext::validate_document_extensions(v, p, policy)
    })?;

    Ok(OntologyDocument::new(
        root,
        schema,
        id,
        version,
        name,
        description,
        nodes,
        relationships,
    ))
}

/// One entry-map walk under per-entry collection: every entry that
/// validates is kept, every failed entry contributes its first issue,
/// and the walk fails with the accumulated issues only once at least one
/// entry failed — so one bad entry does not hide its siblings'
/// findings. The two element maps share it; their per-entry validators
/// stay fail-fast.
fn collect_entries<T>(
    obj: &IndexMap<String, Value>,
    path: &str,
    validate: impl Fn(&str, &str, &Value) -> Result<T, Issue>,
) -> Result<Vec<T>, Vec<Issue>> {
    let mut out = Vec::with_capacity(obj.len());
    let mut issues = Vec::new();
    for (key, entry) in obj {
        let at = join(path, key);
        match validate(key, &at, entry) {
            Ok(value) => out.push(value),
            Err(issue) => issues.push(issue),
        }
    }
    if issues.is_empty() {
        Ok(out)
    } else {
        Err(issues)
    }
}

fn validate_node_entries(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<Vec<NodeType>, Vec<Issue>> {
    let obj = expect_object(value, path, "'nodes'")?;
    collect_entries(obj, path, |key, at, entry| {
        validate_node_entry(key, at, entry, policy)
    })
}

fn validate_node_entry(
    key: &str,
    at: &str,
    entry: &Value,
    policy: UnknownMembers,
) -> Result<NodeType, Issue> {
    let obj = expect_object(entry, at, "a node entry")?;
    check_allowed(obj, NODE_KEYS, at, policy)?;

    let shorthand = opt_nullable_string(obj, "label", at)?;
    let labels = opt_sub(obj, "labels", at, |v, p| {
        validate_labels(v, p, policy).map(Some)
    })?;
    let labels = desugar_labels(shorthand, labels, at)?;

    let properties = opt_sub(obj, "properties", at, |v, p| {
        validate_properties(v, p, policy)
    })?;
    let constraints = opt_sub(obj, "constraints", at, |v, p| {
        validate_constraints(v, p, policy)
    })?;
    let aliases = opt_string_list(obj, "aliases", at)?;
    let reference = opt_string(obj, "reference", at)?;
    let description = opt_string(obj, "description", at)?;
    opt_sub(obj, "extensions", at, |v, p| {
        ext::validate_node_extensions(v, p, policy)
    })?;

    Ok(NodeType {
        key: key.to_owned(),
        labels,
        properties,
        constraints,
        aliases,
        reference,
        description,
    })
}

/// Desugar the `label` shorthand into `labels.identifier` (ADR-0364
/// §3). Both forms together must agree; a lone form is used as-is; neither
/// leaves the identifier null, which the spec allows and `apply` rejects.
fn desugar_labels(
    shorthand: Option<String>,
    labels: Option<Labels>,
    path: &str,
) -> Result<Labels, Issue> {
    let mut labels = labels.unwrap_or_default();
    let Some(shorthand) = shorthand else {
        return Ok(labels);
    };
    match &labels.identifier {
        Some(identifier) if *identifier != shorthand => Err(verr(
            IssueCode::ConflictingMembers,
            path,
            format!(
                "a node entry's 'label' ('{shorthand}') and 'labels.identifier' \
                 ('{identifier}') must be equal"
            ),
        )),
        Some(_) => Ok(labels),
        None => {
            labels.identifier = Some(shorthand);
            Ok(labels)
        }
    }
}

fn validate_labels(value: &Value, path: &str, policy: UnknownMembers) -> Result<Labels, Issue> {
    let obj = expect_object(value, path, "'labels'")?;
    check_allowed(obj, LABELS_KEYS, path, policy)?;
    let identifier = opt_nullable_string(obj, "identifier", path)?;
    let implied = opt_string_list(obj, "implied", path)?;
    let optional = opt_string_list(obj, "optional", path)?;
    opt_sub(obj, "extensions", path, |v, p| {
        validate_custom_extensions(v, p, policy)
    })?;
    Ok(Labels {
        identifier,
        implied,
        optional,
    })
}

fn validate_relationship_entries(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<Vec<RelationshipType>, Vec<Issue>> {
    let obj = expect_object(value, path, "'relationships'")?;
    collect_entries(obj, path, |key, at, entry| {
        validate_relationship_entry(key, at, entry, policy)
    })
}

fn validate_relationship_entry(
    key: &str,
    at: &str,
    entry: &Value,
    policy: UnknownMembers,
) -> Result<RelationshipType, Issue> {
    let obj = expect_object(entry, at, "a relationship entry")?;
    check_allowed(obj, RELATIONSHIP_KEYS, at, policy)?;
    // ADR-0667: `type` is optional here — the published schema requires
    // it, the revised gate accepts a type-less entry, and the converter
    // fails it at `apply` naming the entry.
    let rel_type = opt_string(obj, "type", at)?;
    let from = req_sub(obj, "from", at, |v, p| {
        validate_relationship_target(v, p, policy)
    })?;
    let to = req_sub(obj, "to", at, |v, p| {
        validate_relationship_target(v, p, policy)
    })?;
    let properties = opt_sub(obj, "properties", at, |v, p| {
        validate_properties(v, p, policy)
    })?;
    let constraints = opt_sub(obj, "constraints", at, |v, p| {
        validate_constraints(v, p, policy)
    })?;
    let aliases = opt_string_list(obj, "aliases", at)?;
    let reference = opt_string(obj, "reference", at)?;
    let description = opt_string(obj, "description", at)?;
    opt_sub(obj, "extensions", at, |v, p| {
        ext::validate_relationship_extensions(v, p, policy)
    })?;
    Ok(RelationshipType {
        key: key.to_owned(),
        rel_type,
        from,
        to,
        properties,
        constraints,
        aliases,
        reference,
        description,
    })
}

fn validate_relationship_target(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<RelationshipTarget, Issue> {
    let obj = expect_object(value, path, "a relationship target")?;
    check_allowed(obj, TARGET_KEYS, path, policy)?;
    let node = opt_string(obj, "node", path)?;
    let label = opt_string(obj, "label", path)?;
    let count = opt_nullable_int(obj, "count", path)?;
    let min_count = opt_nullable_int(obj, "min_count", path)?;
    let max_count = opt_nullable_int(obj, "max_count", path)?;
    // ADR-0364 §3: the model represents one cardinality form; combining
    // the exact count with a range bound would silently drop one side.
    if count.is_some() && (min_count.is_some() || max_count.is_some()) {
        return Err(verr(
            IssueCode::ConflictingMembers,
            path,
            "'count' cannot be combined with 'min_count' or 'max_count'",
        ));
    }
    Ok(RelationshipTarget {
        node,
        label,
        count,
        min_count,
        max_count,
    })
}

fn validate_properties(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<Vec<Property>, Issue> {
    let obj = expect_object(value, path, "'properties'")?;
    let mut out = Vec::with_capacity(obj.len());
    for (id, entry) in obj {
        let path = &join(path, id);
        let obj = expect_object(entry, path, "a property")?;
        check_allowed(obj, PROPERTY_KEYS, path, policy)?;
        let property_type = opt_neo4j_type(obj, "type", path)?;
        let dimension = opt_nullable_int(obj, "dimension", path)?;
        let must_exist = opt_nullable_bool(obj, "must_exist", path)?;
        let unique = opt_nullable_bool(obj, "unique", path)?;
        let key = opt_nullable_bool(obj, "key", path)?;
        let one_of = opt_list(obj, "one_of", path, string_list)?;
        let pattern = opt_string(obj, "pattern", path)?;
        let name = opt_nullable_string(obj, "name", path)?;
        let aliases = opt_string_list(obj, "aliases", path)?;
        let reference = opt_string(obj, "reference", path)?;
        let description = opt_string(obj, "description", path)?;
        opt_sub(obj, "extensions", path, |v, p| {
            validate_custom_extensions(v, p, policy)
        })?;
        out.push(Property {
            id: id.clone(),
            property_type,
            dimension,
            must_exist,
            unique,
            key,
            one_of,
            pattern,
            name,
            aliases,
            reference,
            description,
        });
    }
    Ok(out)
}

/// The expected-token list for constraint `type` fields, derived from the
/// canonical [`CONSTRAINTS`] table [`ConstraintType::parse`] consults.
fn expected_constraint_types() -> String {
    CONSTRAINTS
        .iter()
        .map(|(token, _)| format!("'{token}'"))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn validate_constraints(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<Vec<Constraint>, Issue> {
    let obj = expect_object(value, path, "'constraints'")?;
    let mut out = Vec::with_capacity(obj.len());
    for (id, entry) in obj {
        let path = &join(path, id);
        let obj = expect_object(entry, path, "a constraint")?;
        check_allowed(obj, CONSTRAINT_KEYS, path, policy)?;
        let token = req_string(obj, "type", path)?;
        let constraint_type = ConstraintType::parse(&token).ok_or_else(|| {
            verr(
                IssueCode::InvalidEnumToken,
                &join(path, "type"),
                format!(
                    "invalid constraint type '{token}': expected {}",
                    expected_constraint_types()
                ),
            )
        })?;
        let properties = req_string_list(obj, "properties", path)?;
        let name = opt_nullable_string(obj, "name", path)?;
        opt_sub(obj, "extensions", path, |v, p| {
            validate_custom_extensions(v, p, policy)
        })?;
        out.push(Constraint {
            id: id.clone(),
            constraint_type,
            properties,
            name,
        });
    }
    Ok(out)
}

/// A `custom`-object-only extensions envelope: `custom`, when present,
/// must be an object.
fn validate_custom_extensions(
    value: &Value,
    path: &str,
    policy: UnknownMembers,
) -> Result<(), Issue> {
    let obj = expect_object(value, path, "'extensions'")?;
    check_allowed(obj, CUSTOM_ENVELOPE_KEYS, path, policy)?;
    validate_custom_object(obj, path)
}

/// A present `custom` member must be an object; its members are free-form.
fn validate_custom_object(obj: &IndexMap<String, Value>, path: &str) -> Result<(), Issue> {
    match obj.get("custom") {
        None | Some(Value::Object(_)) => Ok(()),
        Some(v) => Err(verr(
            IssueCode::InvalidType,
            &join(path, "custom"),
            format!("'custom' must be an object, found {}", kind_of(v)),
        )),
    }
}

/// The first issue of a validated document as the typed-parse path's
/// [`OntologyError::Validation`] — the sentence and path the fail-fast
/// walk reported. An `Err` from [`validate_document`] carries at least
/// one issue by construction.
pub(crate) fn first_issue(issues: Vec<Issue>) -> OntologyError {
    issues
        .into_iter()
        .next()
        .map(Issue::into_validation_error)
        .expect("a failed validation carries at least one issue")
}
