//! The Query API v2 value encoding (ADR-0236): `CypherValue` to the
//! Neo4j Query API v2 JSON forms — the typed
//! `application/vnd.neo4j.query` envelope and the plain
//! `application/json` rendering. The server's `DefaultResponseModule` and
//! the clients' hydration codecs are the contract; this is the server's
//! side of it.

use serde_json::{json, Map, Value as Json};

use relate::{Error, Namespace};
use relate_model::value::float_text;

use crate::http::{APPLICATION_JSON, VND_NEO4J_QUERY};
use relate_model::wire;
use relate_model::{Coord, CypherValue};

/// The response media type negotiated from the request's `Accept` header:
/// the plain `application/json` rendering, or the Neo4j Query API v2 media
/// type's typed (`{"$type","_value"}`) encoding. The one place the wire
/// content type and the value encoding are decided together, so a reply can
/// never advertise one encoding and serialize the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseMediaType {
    /// `application/json`: plain JSON values.
    Json,
    /// `application/vnd.neo4j.query`: the typed value encoding (LABS-376).
    Neo4jQuery,
}

impl ResponseMediaType {
    /// The `content-type` header value this media type advertises.
    pub(crate) fn content_type(self) -> &'static str {
        match self {
            Self::Json => APPLICATION_JSON,
            Self::Neo4jQuery => VND_NEO4J_QUERY,
        }
    }

    /// Whether values take the typed (`{"$type","_value"}`) encoding.
    pub(crate) fn typed(self) -> bool {
        matches!(self, Self::Neo4jQuery)
    }
}

/// The negotiated response form: the media type (which decides the value
/// encoding) and the statement's routed namespace, threaded as one `Copy`
/// value. The namespace is `None` only for plan-node arguments, which carry
/// scalars, lists and maps and never an entity — an entity there is a loud
/// error.
#[derive(Clone, Copy)]
pub struct Form {
    /// The negotiated media type: the one carrier of the typed/plain split.
    pub media_type: ResponseMediaType,
    /// The statement's routed namespace.
    pub namespace: Option<Namespace>,
}

impl Form {
    /// Whether values take the typed (`{"$type","_value"}`) encoding.
    pub(crate) fn typed(self) -> bool {
        self.media_type.typed()
    }

    /// A payload that is the same in both forms, tagged when the typed form
    /// was negotiated — the two-form rule, one place. An arm whose two
    /// payloads differ spells its own branches.
    fn encode(self, tag: &str, plain: Json) -> Json {
        if self.typed() {
            tagged(tag, plain)
        } else {
            plain
        }
    }

    /// A temporal value: the canonical ISO-8601 string, tagged when typed.
    fn temporal(self, value: &CypherValue, tag: &str) -> Json {
        self.encode(tag, Json::String(value.to_string()))
    }
}

fn tagged(tag: &str, value: Json) -> Json {
    json!({"$type": tag, "_value": value})
}

/// The Query API v2 value encoding — the Neo4j contract (ADR-0236; the
/// server's `DefaultResponseModule` and the clients' hydration codecs), NOT
/// ADR-0223's kind-tagged wire codec: ADR-0223 is the snapshot/CLI/MCP
/// codec, this surface speaks the HTTP API. The typed form (the negotiated
/// `application/vnd.neo4j.query`) wraps every value as
/// `{"$type": <Cypher type>, "_value": …}`; the plain form
/// (`application/json`) renders plain JSON with entities as plain objects.
/// A client decoder dispatches on `$type` — an ADR-0223 `kind` tag reads
/// as `undefined` to it ("Unknown type: undefined" was exactly that).
///
/// The row writer (ADR-0675) enters at depth 0 with the [`Form`] the reply
/// built; the plan encoder enters through [`v2_map_json_at`] the same way.
///
/// # Errors
///
/// An entity whose namespace has no element-id segment (`system`) has no id
/// the wire form can carry, so the value fails with a typed error rather
/// than fabricating a bare numeric id that `elementId()` disagrees with.
/// The reply refuses such a row before its writer starts
/// ([`carries_entity`]); this error is the encoder's own defence.
pub(crate) fn v2_value_json(value: &CypherValue, form: Form) -> relate::Result<Json> {
    v2_value_json_at(value, form, 0)
}

/// [`v2_value_json`]'s walk with the ADR-0462 `depth` guard (the number of
/// containers enclosing `value`): a container past
/// [`relate::MAX_VALUE_DEPTH`] encodes as JSON `null` instead of
/// recursing. Unreachable behind the checked constructors.
pub(crate) fn v2_value_json_at(
    value: &CypherValue,
    form: Form,
    depth: u32,
) -> relate::Result<Json> {
    use CypherValue as V;

    Ok(match value {
        V::Null => form.encode("Null", Json::Null),
        V::Bool(b) => form.encode("Boolean", json!(b)),
        // Typed JSON carries integers and floats as strings.
        V::Int(i) => {
            if form.typed() {
                tagged("Integer", json!(i.to_string()))
            } else {
                json!(i)
            }
        }
        V::Float(f) => {
            if form.typed() {
                tagged("Float", json!(float_text(*f)))
            } else if f.is_finite() {
                wire::json_f64(*f)
            } else {
                // JSON has no non-finite number: the Neo4j spelling is a
                // string that JavaScript and Java both parse.
                Json::String(float_text(*f))
            }
        }
        V::Str(s) => form.encode("String", json!(s)),
        V::List(items) => {
            let Some(next) = relate::next_depth(depth) else {
                return Ok(Json::Null);
            };
            form.encode("List", list_json(items, form, next)?)
        }
        V::Map(map) => {
            let Some(next) = relate::next_depth(depth) else {
                return Ok(Json::Null);
            };
            form.encode("Map", Json::Object(v2_map_json_at(map, form, next)?))
        }
        V::Node {
            id,
            labels,
            properties,
        } => {
            let Some(next) = relate::next_depth(depth) else {
                return Ok(Json::Null);
            };
            node_json(*id, labels, properties, form, next)?
        }
        V::Relationship {
            id,
            start_id,
            end_id,
            rel_type,
            properties,
        } => {
            let Some(next) = relate::next_depth(depth) else {
                return Ok(Json::Null);
            };
            relationship_json(*id, *start_id, *end_id, rel_type, properties, form, next)?
        }
        V::Path {
            nodes,
            relationships,
        } => {
            // Alternating [node, rel, node, …, node], typed or plain per
            // the negotiated form.
            if relate::next_depth(depth).is_none() {
                return Ok(Json::Null);
            }
            let mut items = Vec::with_capacity(nodes.len() + relationships.len());
            for (i, n) in nodes.iter().enumerate() {
                // A path's embedded entities are not an extra container
                // level (ADR-0462 depth accounting).
                items.push(v2_value_json_at(n, form, depth)?);
                if let Some(r) = relationships.get(i) {
                    items.push(v2_value_json_at(r, form, depth)?);
                }
            }
            form.encode("Path", Json::Array(items))
        }
        // Temporal values: canonical ISO-8601 strings (the wire codec's
        // documented Display), tagged with the Neo4j type name when typed.
        V::Date(_) => form.temporal(value, "Date"),
        V::LocalDateTime(_) => form.temporal(value, "LocalDateTime"),
        V::DateTime(_, zone) => {
            // A fixed offset is OffsetDateTime; an IANA zone name is
            // ZonedDateTime (the server's split).
            form.temporal(
                value,
                if zone.is_some() {
                    "ZonedDateTime"
                } else {
                    "OffsetDateTime"
                },
            )
        }
        V::Time(_, offset) => form.temporal(
            value,
            if offset.is_some() {
                "Time"
            } else {
                "LocalTime"
            },
        ),
        V::Duration(_) => form.temporal(value, "Duration"),
        // The Neo4j Query API carries a point as its EWKT string in both
        // encodings (ADR-0236); the `{coordinates, crs}` object was the
        // Relate-only shape, which a client's Point decoder does not know.
        V::Point(p) => form.encode("Point", Json::String(relate_model::point::ewkt(p))),
        V::Vector { coord, coords, .. } => {
            let Some(next) = relate::next_depth(depth) else {
                return Ok(Json::Null);
            };
            if form.typed() {
                tagged(
                    "Vector",
                    json!({
                        // The Neo4j coordinatesType vocabulary (INT8 …
                        // FLOAT64); coordinates are strings there.
                        "coordinatesType": vector_coord_name(coord),
                        "coordinates": coords
                            .iter()
                            .map(|c| Json::String(c.text()))
                            .collect::<Vec<_>>(),
                    }),
                )
            } else {
                // A plain vector is exactly a list of its coordinates —
                // the packed read widens each coordinate to the FLOAT
                // value the `Value` walk produced (ADR-0706).
                list_json(
                    &coords
                        .iter()
                        .map(|c| match c {
                            Coord::I(n) => V::Int(n),
                            Coord::F(f) => V::Float(f),
                        })
                        .collect::<Vec<_>>(),
                    form,
                    next,
                )?
            }
        }
        // Geometry is a Relate extension the Neo4j client codecs do not
        // know: typed reports it honestly as the string the spec documents,
        // plain keeps ADR-0223's wire object — the two forms carry
        // different payloads, so this arm keeps its own branches.
        V::Geometry(_) => {
            if form.typed() {
                tagged("Unsupported", json!(format!("{value}")))
            } else {
                wire::value_to_wire_json(value)
            }
        }
    })
}

/// A list of values encoded one level down — the guarded container walk the
/// `List` arm and the plain `Vector` arm share.
fn list_json(items: &[CypherValue], form: Form, next: u32) -> relate::Result<Json> {
    let mut encoded = Vec::with_capacity(items.len());
    for item in items {
        encoded.push(v2_value_json_at(item, form, next)?);
    }
    Ok(Json::Array(encoded))
}

/// One node as the v2 entity object: the plain Neo4j field names, or the
/// typed `_`-prefixed twin — one construction, the keys selected by form.
fn node_json(
    id: i64,
    labels: &[String],
    properties: &indexmap::IndexMap<String, CypherValue>,
    form: Form,
    next: u32,
) -> relate::Result<Json> {
    let element_id = element_id(id, form.namespace, ElementKind::Node)?;
    let properties = Json::Object(v2_map_json_at(properties, form, next)?);
    let (id_key, labels_key, properties_key) = if form.typed() {
        ("_element_id", "_labels", "_properties")
    } else {
        ("elementId", "labels", "properties")
    };
    let mut node = Map::new();
    node.insert(id_key.into(), Json::String(element_id));
    node.insert(labels_key.into(), json!(labels));
    node.insert(properties_key.into(), properties);
    Ok(form.encode("Node", Json::Object(node)))
}

/// One relationship as the v2 entity object — [`node_json`]'s twin.
fn relationship_json(
    id: i64,
    start_id: i64,
    end_id: i64,
    rel_type: &str,
    properties: &indexmap::IndexMap<String, CypherValue>,
    form: Form,
    next: u32,
) -> relate::Result<Json> {
    let rel_id = element_id(id, form.namespace, ElementKind::Rel)?;
    let start = element_id(start_id, form.namespace, ElementKind::Node)?;
    let end = element_id(end_id, form.namespace, ElementKind::Node)?;
    let properties = Json::Object(v2_map_json_at(properties, form, next)?);
    let (id_key, start_key, end_key, type_key, properties_key) = if form.typed() {
        (
            "_element_id",
            "_start_node_element_id",
            "_end_node_element_id",
            "_type",
            "_properties",
        )
    } else {
        (
            "elementId",
            "startNodeElementId",
            "endNodeElementId",
            "type",
            "properties",
        )
    };
    let mut relationship = Map::new();
    relationship.insert(id_key.into(), Json::String(rel_id));
    relationship.insert(start_key.into(), Json::String(start));
    relationship.insert(end_key.into(), Json::String(end));
    relationship.insert(type_key.into(), Json::String(rel_type.to_string()));
    relationship.insert(properties_key.into(), properties);
    Ok(form.encode("Relationship", Json::Object(relationship)))
}

/// A property/map value under the v2 encoding, with the ADR-0462 `depth`
/// guard. Each caller supplies the namespace it may encode under: the row
/// writer the statement's, the plan encoder `None` — so an entity in a plan
/// argument is a loud error rather than borrowing the statement's namespace.
pub(crate) fn v2_map_json_at(
    map: &indexmap::IndexMap<String, CypherValue>,
    form: Form,
    depth: u32,
) -> relate::Result<Map<String, Json>> {
    map.iter()
        .map(|(k, v)| Ok((k.clone(), v2_value_json_at(v, form, depth)?)))
        .collect()
}

/// Which entity kind an element id encodes (the `4`/`5` digit and the
/// `n`/`r` prefix travel together in [`element_id`]).
#[derive(Clone, Copy)]
enum ElementKind {
    Node,
    Rel,
}

/// The ADR-0554 element id (`4:<segment>:n<id>` / `5:<segment>:r<id>`) the
/// engine constructors produce — never the bare numeric id. Two states have
/// no id the wire can carry, and each says so instead of fabricating one:
/// no namespace at all (plan-node arguments, which carry scalars, lists and
/// maps and never an entity — a contract violation if one appears), and
/// `system`, which has no element-id segment yet a statement routed there
/// can yield an entity through a procedure or a catalog clause.
fn element_id(id: i64, namespace: Option<Namespace>, kind: ElementKind) -> relate::Result<String> {
    let Some(namespace) = namespace else {
        return Err(Error::internal("plan arguments carry no entities"));
    };
    let encoded = match kind {
        ElementKind::Node => relate::node_element_id(id, namespace),
        ElementKind::Rel => relate::rel_element_id(id, namespace),
    };
    encoded.ok_or_else(missing_element_id_error)
}

/// Whether a value carries an entity, directly or inside a container — the
/// fact that decides whether a namespace needs an element-id segment for
/// this value. One predicate for the reply's pre-flight refusal and the
/// encoder's own error.
pub(crate) fn carries_entity(value: &relate::Value) -> bool {
    // Exhaustive on purpose: a new value variant must decide whether it can
    // carry an entity, and the pre-writer refusal must cover exactly the
    // shapes the writer descends into.
    use relate::Value as V;
    match value {
        V::Node(_) | V::Relationship(_) | V::Path(_) => true,
        V::List(items) => items.iter().any(carries_entity),
        V::Map(map) => map.values().any(carries_entity),
        // A packed vector (ADR-0706) holds only finite numbers by
        // construction, so it never carries an entity — the typed form
        // spells them as numbers and the plain form never descends.
        V::Null
        | V::Bool(_)
        | V::Int(_)
        | V::Float(_)
        | V::String(_)
        | V::Vector { .. }
        | V::Temporal(_)
        | V::Point(_)
        | V::Uuid(_)
        | V::Bytes(_) => false,
    }
}

/// The one refusal for an entity whose namespace has no element-id segment
/// (`elementId()` raises the same text).
pub(crate) fn missing_element_id_error() -> Error {
    Error::execution("elements of this namespace have no element ids")
}

/// Refuse rows that carry an entity under a namespace with no element-id
/// segment: the wire form has no id to carry, and the ADR-0675 writer
/// encodes lazily, so the refusal has to happen before a row is written
/// rather than failing a chunk mid-stream. The scan is skipped entirely
/// unless the namespace lacks a segment (`system` is the only one), and
/// [`carries_entity`] descends into containers, so `collect(n)` / `{a: n}`
/// are caught too. The one check: the collected reply runs it over the
/// whole result, the statement streamer (`crate::streamed`) per batch.
pub(crate) fn refuse_entities_without_element_ids(
    namespace: Namespace,
    rows: &[Vec<relate::Value>],
) -> Result<(), Error> {
    if namespace.element_id_segment().is_none() && rows.iter().flatten().any(carries_entity) {
        return Err(missing_element_id_error());
    }
    Ok(())
}

/// The Neo4j `coordinatesType` vocabulary for a vector's coordinate kind.
fn vector_coord_name(coord: &relate_model::VectorCoordKind) -> &'static str {
    use relate_model::VectorCoordKind as K;
    match coord {
        K::Int8 => "INT8",
        K::Int16 => "INT16",
        K::Int32 => "INT32",
        K::Int64 => "INT64",
        K::Float16 => "FLOAT16",
        K::BFloat16 => "BFLOAT16",
        K::Float32 => "FLOAT32",
        K::Float64 => "FLOAT64",
    }
}
