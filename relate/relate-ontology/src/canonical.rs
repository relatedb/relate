//! The canonicalize path (ADR-0667): the storage codec behind
//! `db.ontology.set`'s document handling.
//!
//! [`canonicalize_ontology_document`] parses the submitted bytes as JSON
//! first and as YAML when the JSON parse fails, gates the parsed root on
//! the pinned spec version, runs the structural validation with
//! unknown members **dropped**, and re-serializes the validated document
//! as canonical JSON — the form that gets stored. The canonical form is
//! model-built, so it carries only the schema's members in the schema's
//! member order, with the `label` shorthand expanded into
//! `labels.identifier` and the `must_exist`/`unique`/`key` property
//! flags expanded into constraint entries; equivalent JSON and YAML
//! submissions of one document derive byte-identical canonical output,
//! and canonicalizing the canonical output is a fixed point.
//!
//! The canonical member rule: a member the schema requires is always
//! written (the root's `schema`/`version`/`id`, a constraint's
//! `type`/`properties`, an endpoint's object), the `labels` object is
//! always written (the one expanded home of a node entry's label
//! state), and every other member is written iff the model carries it
//! with content — an option iff `Some`, a list or map iff non-empty, an
//! extensions envelope iff it holds a known member. Empty and absent
//! are the same state, so equivalent inputs collapse to one form.
//! Extension payloads are the one verbatim carry: they have no typed
//! view (ADR-0364 §5.1), so each known envelope member is grafted from
//! the retained root tree as submitted — the drop rule applies to the
//! document's own members and to the envelopes' closed key sets, never
//! to a payload's interior.
//!
//! Every failure renders one of three user-facing envelopes, verbatim:
//!
//! - both parses failed —
//!   `The ontology document could not be parsed as a graph-spec JSON or
//!   YAML document.` with each attempt's message under its syntax label;
//! - the `schema` field names an unsupported version —
//!   `The ontology document uses an unsupported graph-spec version.` with
//!   the reason;
//! - the validation found issues —
//!   `The ontology document failed validation.` with every issue as
//!   `[<code>] <message> (<path>)`, semicolon-joined, capped at
//!   [`MAX_ISSUES_IN_ERROR`] issues plus an ellipsis-and-count suffix.

use indexmap::IndexMap;

use crate::error::{render_issues, render_stored_issues, Issue, OntologyError};
use crate::format::DocumentFormat;
use crate::model::{
    flagged_constraint, generated_constraint_name, Constraint, ConstraintType, NodeType,
    OntologyDocument, Property, RelationshipType,
};
use crate::parse::{parse_json_value, parse_yaml_value};
use crate::serialize::{to_json_string, to_yaml_string};
use crate::validate::{schema_uri_mismatch, validate_document, UnknownMembers};
use crate::value::Value;

/// Parse and canonicalize an ontology document from JSON or YAML bytes:
/// the canonical JSON text the storage layer keeps.
///
/// # Errors
///
/// - [`OntologyError::UnparsableDocument`] — the bytes parse neither as
///   JSON nor as YAML.
/// - [`OntologyError::UnsupportedGraphSpecVersion`] — the root `schema`
///   member names a version the gate does not support.
/// - [`OntologyError::ValidationFailed`] — the document violates the
///   structural validation; the envelope carries every issue found,
///   capped at [`MAX_ISSUES_IN_ERROR`] with an ellipsis-and-count
///   suffix.
/// - [`OntologyError::Mapping`] — the validated document declares an
///   explicit constraint whose id collides with the name one of its
///   property's flags generates; the canonical form keys constraints
///   by id and cannot carry both under one key.
///
/// The invalid-UTF-8, construct and nesting failures of the two parse
/// attempts ride the unparsable envelope: neither attempt can read such
/// bytes, so each attempt's line carries the same message.
pub fn canonicalize_ontology_document(bytes: &[u8]) -> Result<String, OntologyError> {
    let root = codec_root(bytes)?;
    let document = validate_document(root, UnknownMembers::Drop).map_err(validation_failed)?;
    let canonical = canonical_tree(&document)?;
    to_json_string(&canonical)
}

/// The codec's parse steps every stored-document reader shares (ADR-0667):
/// the UTF-8 decode, the JSON-then-YAML parse, and the spec-version gate —
/// each failure rendered as its user-facing envelope, so `set`, `get` and
/// `apply` report one wording per failure.
fn codec_root(bytes: &[u8]) -> Result<Value, OntologyError> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => {
            let message = err.to_string();
            return Err(OntologyError::UnparsableDocument {
                json: message.clone(),
                yaml: message,
            });
        }
    };
    // JSON first: a document that parses as strict JSON is JSON
    // (ADR-0227 §3); only a document that parses as neither syntax is
    // refused here, each attempt's message riding the envelope.
    let root = match parse_json_value(text) {
        Ok(value) => value,
        Err(json) => parse_yaml_value(text).map_err(|yaml| OntologyError::UnparsableDocument {
            json: json.to_string(),
            yaml: yaml.to_string(),
        })?,
    };
    if let Some(uri) = schema_uri_mismatch(&root) {
        return Err(OntologyError::UnsupportedGraphSpecVersion { uri });
    }
    Ok(root)
}

/// Parse and validate an already-stored document (ADR-0667): the codec's
/// parse and version gate, then the structural validation with unknown
/// members dropped and every issue collected — the reader `apply`
/// interprets the stored bytes with. Unlike [`canonicalize_ontology_document`]
/// the validated model is the answer, and a validation failure renders the
/// stored-document envelope: every row, newline-joined, never elided.
///
/// # Errors
///
/// - [`OntologyError::UnparsableDocument`] / [`OntologyError::UnsupportedGraphSpecVersion`]
///   — the stored bytes failed the parse or the version gate; the caller
///   that names the document as stored wraps these.
/// - [`OntologyError::StoredInvalid`] — the stored document failed the
///   structural validation.
pub fn parse_stored_ontology_document(bytes: &[u8]) -> Result<OntologyDocument, OntologyError> {
    let root = codec_root(bytes)?;
    validate_document(root, UnknownMembers::Drop).map_err(|issues| OntologyError::StoredInvalid {
        rendered: render_stored_issues(&issues),
    })
}

/// Re-render an already-stored document in the requested format
/// (ADR-0667): the codec's parse and version gate, then the canonical
/// serialization of the parsed value tree — no structural validation, so
/// a document whose members the current schema does not know still
/// renders. A parse or version-gate failure is the envelope verbatim: the
/// stored bytes no longer decode, and the reader that serves them fails.
pub fn render_ontology_document(
    bytes: &[u8],
    format: DocumentFormat,
) -> Result<String, OntologyError> {
    let root = codec_root(bytes)?;
    match format {
        DocumentFormat::Json => to_json_string(&root),
        DocumentFormat::Yaml => to_yaml_string(&root),
    }
}

/// The validation envelope variant from the walk's full issue list — the
/// one rendering of the capped join.
fn validation_failed(issues: Vec<Issue>) -> OntologyError {
    OntologyError::ValidationFailed {
        rendered: render_issues(&issues),
    }
}

/// Insert `value` at `key`, keeping insertion order — the canonical
/// member order is the insertion order the builder walks the schema's
/// member tables in.
fn set(map: &mut IndexMap<String, Value>, key: &str, value: Value) {
    map.insert(key.to_owned(), value);
}

/// Insert the carried string at `key` — the optional-member write
/// ladder every builder's `Some`-iff-carried member goes through, so
/// the branch policy lives in one place.
fn set_str(map: &mut IndexMap<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        set(map, key, Value::String(value.to_owned()));
    }
}

/// Insert the carried integer at `key`.
fn set_int(map: &mut IndexMap<String, Value>, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        set(map, key, Value::Integer(value));
    }
}

/// Insert the list at `key` when it carries entries — empty and absent
/// are the same state.
fn set_list(map: &mut IndexMap<String, Value>, key: &str, values: &[String]) {
    if !values.is_empty() {
        set(map, key, strings(values));
    }
}

/// A list of strings, in carried order.
fn strings(values: &[String]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|value| Value::String(value.clone()))
            .collect(),
    )
}

/// The source object of one element entry on the retained root tree —
/// the graft point for its extension payloads. `None` when the retained
/// tree carries no such entry (never for a validated document: the model
/// is built from that tree).
fn element_source<'a>(
    root: &'a Value,
    section: &str,
    key: &str,
) -> Option<&'a IndexMap<String, Value>> {
    root.as_object()?
        .get(section)?
        .as_object()?
        .get(key)?
        .as_object()
}

/// The raw-source graft ladder, walked once: from a graft point's
/// source object, descend the key path — every step but the last an
/// object — to the grafted value. The property, constraint and labels
/// grafts are its callers, each naming only its key path.
fn dig<'a>(from: Option<&'a IndexMap<String, Value>>, keys: &[&str]) -> Option<&'a Value> {
    let mut obj = from?;
    let (last, path) = keys.split_last()?;
    for key in path {
        obj = obj.get(*key)?.as_object()?;
    }
    obj.get(*last)
}

/// One extensions envelope, rebuilt with only its known members in the
/// published member order: `None` when the envelope is absent or holds
/// no known member. The payload interiors are carried verbatim — they
/// have no typed view (ADR-0364 §5.1).
fn canonical_extensions(source: Option<&Value>, keys: &[&str]) -> Option<IndexMap<String, Value>> {
    let obj = source?.as_object()?;
    let mut out = IndexMap::new();
    for key in keys {
        if let Some(value) = obj.get(*key) {
            out.insert((*key).to_owned(), value.clone());
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Graft one rebuilt extensions envelope into `map` at `"extensions"`,
/// nothing when the envelope holds no known member.
fn graft_extensions(map: &mut IndexMap<String, Value>, source: Option<&Value>, keys: &[&str]) {
    if let Some(extensions) = canonical_extensions(source, keys) {
        set(map, "extensions", Value::Object(extensions));
    }
}

/// The document's canonical value tree: the validated model re-emitted
/// with only the schema's members, in the schema's member order, with
/// the shorthand expanded and the flags moved into constraint entries.
/// The model is validated, so every member the builder reads is present
/// and shaped; the one failure left is a flag-derived constraint whose
/// generated id collides with an explicit entry's
/// ([`canonical_constraints`]).
fn canonical_tree(doc: &OntologyDocument) -> Result<Value, OntologyError> {
    let mut root = IndexMap::new();
    set(&mut root, "schema", Value::String(doc.schema().to_owned()));
    set(
        &mut root,
        "version",
        Value::String(doc.version().to_owned()),
    );
    set(&mut root, "id", Value::String(doc.id().to_owned()));
    set_str(&mut root, "name", doc.name());
    set_str(&mut root, "description", doc.description());
    if !doc.nodes().is_empty() {
        let mut nodes = IndexMap::new();
        for entry in doc.nodes() {
            set(&mut nodes, &entry.key, canonical_node(doc, entry)?);
        }
        set(&mut root, "nodes", Value::Object(nodes));
    }
    if !doc.relationships().is_empty() {
        let mut relationships = IndexMap::new();
        for entry in doc.relationships() {
            set(
                &mut relationships,
                &entry.key,
                canonical_relationship(doc, entry)?,
            );
        }
        set(&mut root, "relationships", Value::Object(relationships));
    }
    graft_extensions(
        &mut root,
        doc.root()
            .as_object()
            .and_then(|root| root.get("extensions")),
        crate::validate::DOCUMENT_ENVELOPE_KEYS,
    );
    Ok(Value::Object(root))
}

/// The canonical node entry: the `labels` object always written (the one
/// expanded home of the entry's label state), then the element body
/// every element kind shares.
fn canonical_node(doc: &OntologyDocument, entry: &NodeType) -> Result<Value, OntologyError> {
    let source = element_source(doc.root(), "nodes", &entry.key);
    let mut node = IndexMap::new();
    // The `labels` object: validation folds the `label` shorthand into
    // `identifier`, so the canonical form has the one label shape.
    let mut labels = IndexMap::new();
    set_str(
        &mut labels,
        "identifier",
        entry.labels.identifier.as_deref(),
    );
    set_list(&mut labels, "implied", &entry.labels.implied);
    set_list(&mut labels, "optional", &entry.labels.optional);
    graft_extensions(
        &mut labels,
        dig(source, &["labels", "extensions"]),
        crate::validate::CUSTOM_ENVELOPE_KEYS,
    );
    set(&mut node, "labels", Value::Object(labels));
    graft_element_body(&mut node, source, &ElementBody::from(entry))?;
    Ok(Value::Object(node))
}

/// The canonical relationship entry: `type` when the entry carries one
/// (the converter is the gate for a type-less entry, ADR-0667), both
/// endpoints always (the schema requires them), then the element body
/// every element kind shares.
fn canonical_relationship(
    doc: &OntologyDocument,
    entry: &RelationshipType,
) -> Result<Value, OntologyError> {
    let source = element_source(doc.root(), "relationships", &entry.key);
    let mut relationship = IndexMap::new();
    set_str(&mut relationship, "type", entry.rel_type.as_deref());
    set(&mut relationship, "from", canonical_target(&entry.from));
    set(&mut relationship, "to", canonical_target(&entry.to));
    graft_element_body(&mut relationship, source, &ElementBody::from(entry))?;
    Ok(Value::Object(relationship))
}

/// The element-level members both entry kinds carry alike, plus the
/// kind's own extensions envelope keys — what the shared
/// [`graft_element_body`] emitter reads. [`From`] fills it from a
/// [`NodeType`] or a [`RelationshipType`], so a field added to one
/// kind's element body joins the shared shape by construction.
struct ElementBody<'a> {
    /// The entry kind: names it in the element-level failures the shared
    /// body reports and selects its closed extensions envelope key set.
    kind: ElementKind,
    /// The entry's id in the document's map.
    key: &'a str,
    /// The entry's properties, in document order.
    properties: &'a [Property],
    /// The entry's constraint objects, in document order.
    constraints: &'a [Constraint],
    /// Human-readable description.
    description: Option<&'a str>,
    /// Alternative names for the entry.
    aliases: &'a [String],
    /// Informational URI pointing at an external definition.
    reference: Option<&'a str>,
}

/// The two element kinds a document carries — the one typed encoding of
/// node-vs-relationship the shared element body reads, so the kind's
/// word and its envelope key set cannot drift apart.
#[derive(Debug, Clone, Copy)]
enum ElementKind {
    /// A node entry.
    Node,
    /// A relationship entry.
    Relationship,
}

impl ElementKind {
    /// The kind's word in element-level failures.
    fn word(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Relationship => "relationship",
        }
    }

    /// The kind's closed extensions envelope key set.
    fn envelope_keys(self) -> &'static [&'static str] {
        match self {
            Self::Node => crate::validate::NODE_ENVELOPE_KEYS,
            Self::Relationship => crate::validate::RELATIONSHIP_ENVELOPE_KEYS,
        }
    }
}

impl<'a> From<&'a NodeType> for ElementBody<'a> {
    fn from(entry: &'a NodeType) -> Self {
        Self {
            kind: ElementKind::Node,
            key: entry.key.as_str(),
            properties: &entry.properties,
            constraints: &entry.constraints,
            description: entry.description.as_deref(),
            aliases: &entry.aliases,
            reference: entry.reference.as_deref(),
        }
    }
}

impl<'a> From<&'a RelationshipType> for ElementBody<'a> {
    fn from(entry: &'a RelationshipType) -> Self {
        Self {
            kind: ElementKind::Relationship,
            key: entry.key.as_str(),
            properties: &entry.properties,
            constraints: &entry.constraints,
            description: entry.description.as_deref(),
            aliases: &entry.aliases,
            reference: entry.reference.as_deref(),
        }
    }
}

/// The canonical element body both kinds share — the one spelling of an
/// element's canonical member order after its kind-specific head: the
/// properties map with the flags left out, the explicit plus
/// flag-derived constraint entries, the known extension envelopes
/// grafted verbatim, and the description/aliases/reference tail.
fn graft_element_body(
    out: &mut IndexMap<String, Value>,
    source: Option<&IndexMap<String, Value>>,
    entry: &ElementBody<'_>,
) -> Result<(), OntologyError> {
    if !entry.properties.is_empty() {
        let mut properties = IndexMap::new();
        for property in entry.properties {
            set(
                &mut properties,
                &property.id,
                canonical_property(source, property),
            );
        }
        set(out, "properties", Value::Object(properties));
    }
    let constraints = canonical_constraints(source, entry)?;
    if !constraints.is_empty() {
        set(out, "constraints", Value::Object(constraints));
    }
    graft_element_extensions(out, source, entry.kind.envelope_keys());
    set_str(out, "description", entry.description);
    set_list(out, "aliases", entry.aliases);
    set_str(out, "reference", entry.reference);
    Ok(())
}

/// The canonical `RelationshipTarget`: the members the entry carried,
/// in the schema's member order — an endpoint with nothing to say is
/// the empty object, exactly as the model holds it.
fn canonical_target(target: &crate::model::RelationshipTarget) -> Value {
    let mut out = IndexMap::new();
    set_str(&mut out, "node", target.node.as_deref());
    set_str(&mut out, "label", target.label.as_deref());
    set_int(&mut out, "count", target.count);
    set_int(&mut out, "min_count", target.min_count);
    set_int(&mut out, "max_count", target.max_count);
    Value::Object(out)
}

/// The canonical property: the declared members with the three flags
/// left out — they live in the element's constraint entries — and the
/// property's own `custom` envelope grafted verbatim.
fn canonical_property(
    element_source: Option<&IndexMap<String, Value>>,
    property: &Property,
) -> Value {
    let mut out = IndexMap::new();
    set_str(
        &mut out,
        "type",
        property.property_type.map(|token| token.as_str()),
    );
    set_int(&mut out, "dimension", property.dimension);
    graft_extensions(
        &mut out,
        dig(
            element_source,
            &["properties", property.id.as_str(), "extensions"],
        ),
        crate::validate::CUSTOM_ENVELOPE_KEYS,
    );
    set_str(&mut out, "name", property.name.as_deref());
    set_str(&mut out, "description", property.description.as_deref());
    set_list(&mut out, "aliases", &property.aliases);
    set_str(&mut out, "reference", property.reference.as_deref());
    set_str(&mut out, "pattern", property.pattern.as_deref());
    if let Some(one_of) = &property.one_of {
        set_list(&mut out, "one_of", one_of);
    }
    Value::Object(out)
}

/// The element's canonical constraint map: the explicit entries in
/// document order, then one entry per asserted property flag — keyed
/// `<name>_<element>_<kind>` from the property's resolved name, carrying
/// the generated name, referencing the property by its id. A flag
/// entry's key colliding with an explicit entry's id is a loud
/// [`OntologyError::Mapping`] failure naming both: the form keys
/// constraints by id and cannot carry the two under one key, and
/// letting either win would silently drop the other's authored
/// content. The canonical form itself cannot re-collide — it strips
/// the property flags, materializing them as explicit entries — so
/// re-canonicalizing the form stays a no-op.
fn canonical_constraints(
    element_source: Option<&IndexMap<String, Value>>,
    element: &ElementBody<'_>,
) -> Result<IndexMap<String, Value>, OntologyError> {
    let mut out = IndexMap::new();
    for constraint in element.constraints {
        set(
            &mut out,
            &constraint.id,
            canonical_constraint(element_source, constraint),
        );
    }
    for property in element.properties {
        let name = property.resolved_name();
        // The flag-derived entries: key/unique from the one naming-policy
        // home (key wins over unique, ADR-0667) — the same home the
        // converter's flag-derived definitions consult — then the
        // existence flag, with the same name shape.
        for (kind, generated) in [
            flagged_constraint(name, element.key, property.key, property.unique),
            (property.must_exist == Some(true)).then(|| {
                (
                    ConstraintType::Exists,
                    generated_constraint_name(name, element.key, "exists"),
                )
            }),
        ]
        .into_iter()
        .flatten()
        {
            // The one collision the form can see: a generated name
            // spelling an explicit entry's id. Explicit ids come from
            // the document's own map keys, which the parser collapses,
            // so only a synthesized key can introduce a second entry
            // under a live id.
            if out.contains_key(generated.as_str()) {
                return Err(OntologyError::Mapping(format!(
                    "Ontology {} '{}' declares an explicit constraint whose \
                     id '{generated}' collides with the name a property's {} \
                     flag generates.",
                    element.kind.word(),
                    element.key,
                    kind.as_str().to_lowercase()
                )));
            }
            let mut entry = IndexMap::new();
            set(&mut entry, "type", Value::String(kind.as_str().to_owned()));
            set(
                &mut entry,
                "properties",
                strings(std::slice::from_ref(&property.id)),
            );
            set(&mut entry, "name", Value::String(generated.clone()));
            set(&mut out, &generated, Value::Object(entry));
        }
    }
    Ok(out)
}

/// One canonical constraint entry: the kind token, the listed property
/// ids (always written — the schema requires the member), the entry's
/// own `custom` envelope grafted verbatim, and the name when carried.
fn canonical_constraint(
    element_source: Option<&IndexMap<String, Value>>,
    constraint: &Constraint,
) -> Value {
    let mut out = IndexMap::new();
    set(
        &mut out,
        "type",
        Value::String(constraint.constraint_type.as_str().to_owned()),
    );
    set(&mut out, "properties", strings(&constraint.properties));
    graft_extensions(
        &mut out,
        dig(
            element_source,
            &["constraints", constraint.id.as_str(), "extensions"],
        ),
        crate::validate::CUSTOM_ENVELOPE_KEYS,
    );
    set_str(&mut out, "name", constraint.name.as_deref());
    Value::Object(out)
}

/// An element's own extensions envelope, grafted under the element's
/// closed envelope key set.
fn graft_element_extensions(
    map: &mut IndexMap<String, Value>,
    source: Option<&IndexMap<String, Value>>,
    keys: &[&str],
) {
    graft_extensions(
        map,
        source.and_then(|element| element.get("extensions")),
        keys,
    );
}

#[cfg(test)]
mod tests;
