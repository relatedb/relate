//! JSON encode/decode for [`Value`]: the tagged `{"t":..., "v":...}` form
//! (WAL/checkpoint bytes) and the plain FFI form, plus the `Tagged*Ref` serde
//! impls behind the canonical tagged encoder.

use super::*;

impl Value {
    /// The tagged `{"t":..., "v":...}` JSON form. Built from the canonical
    /// [`serde::Serialize`] impl (via [`TaggedRef`]), so the tree here and the
    /// direct WAL/checkpoint bytes cannot diverge. Graph entities are not
    /// persistence properties: the encoder rejects them (see [`TaggedRef`]),
    /// and that internal error maps to `Null` here.
    pub fn to_json_tagged(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    pub fn from_json_tagged(v: &serde_json::Value) -> crate::Result<Value> {
        Self::from_json_tagged_at(v, 0)
    }

    /// [`Value::from_json_tagged`] with the ADR-0462 `depth` (the number of
    /// tagged containers enclosing `v`): an untrusted persistence input
    /// nested past [`MAX_VALUE_DEPTH`] is rejected with a typed storage
    /// error instead of driving the recursive decoder off the stack.
    fn from_json_tagged_at(v: &serde_json::Value, depth: u32) -> crate::Result<Value> {
        fn malformed(expected: &str) -> crate::Error {
            crate::Error::storage(format!("malformed tagged value: expected {expected}"))
        }
        fn too_deep() -> crate::Error {
            crate::Error::storage(value_depth_exceeded("tagged value"))
        }
        let obj = v
            .as_object()
            .ok_or_else(|| crate::Error::storage("property JSON must be an object"))?;
        let t = obj
            .get("t")
            .and_then(|x| x.as_str())
            .ok_or_else(|| crate::Error::storage("missing type tag"))?;
        Ok(match t {
            "null" => Value::Null,
            "bool" => Value::Bool(
                obj.get("v")
                    .and_then(|x| x.as_bool())
                    .ok_or_else(|| malformed("\"bool\" with a boolean \"v\""))?,
            ),
            "int" => Value::Int(
                obj.get("v")
                    .and_then(|x| x.as_i64())
                    .ok_or_else(|| malformed("\"int\" with an integer \"v\""))?,
            ),
            "float" => Value::Float(match obj.get("v") {
                Some(serde_json::Value::String(s)) if s == "NaN" => f64::NAN,
                Some(serde_json::Value::String(s)) if s == "Infinity" => f64::INFINITY,
                Some(serde_json::Value::String(s)) if s == "-Infinity" => f64::NEG_INFINITY,
                other => other
                    .and_then(|x| x.as_f64())
                    .ok_or_else(|| malformed("\"float\" with a number \"v\""))?,
            }),
            "string" => Value::String(
                obj.get("v")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| malformed("\"string\" with a string \"v\""))?
                    .into(),
            ),
            "uuid" => {
                let s = obj
                    .get("v")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| malformed("\"uuid\" with a string \"v\""))?;
                Value::Uuid(
                    uuid_parse(s)
                        .ok_or_else(|| malformed("\"uuid\" with a canonical hyphenated \"v\""))?,
                )
            }
            "list" => {
                let Some(next) = next_depth(depth) else {
                    return Err(too_deep());
                };
                let arr = obj
                    .get("v")
                    .and_then(|x| x.as_array())
                    .ok_or_else(|| malformed("\"list\" with an array \"v\""))?;
                Value::List(
                    arr.iter()
                        .map(|x| Value::from_json_tagged_at(x, next))
                        .collect::<crate::Result<Vec<_>>>()?
                        .into(),
                )
            }
            "map" => {
                let Some(next) = next_depth(depth) else {
                    return Err(too_deep());
                };
                let mut map = BTreeMap::new();
                let o = obj
                    .get("v")
                    .and_then(|x| x.as_object())
                    .ok_or_else(|| malformed("\"map\" with an object \"v\""))?;
                for (k, val) in o {
                    map.insert(k.clone(), Value::from_json_tagged_at(val, next)?);
                }
                Value::Map(map.into())
            }
            "date" | "time" | "localtime" | "datetime" | "localdatetime" | "duration" => {
                let s = obj
                    .get("v")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| crate::Error::storage("missing temporal value"))?;
                Value::Temporal(crate::temporal::parse_iso(t, s)?)
            }
            "point" => {
                let p = obj
                    .get("v")
                    .and_then(|x| x.as_object())
                    .ok_or_else(|| malformed("\"point\" with an object \"v\""))?;
                Value::Point(PointValue {
                    srid: p
                        .get("srid")
                        .and_then(|x| x.as_i64())
                        .and_then(|x| i32::try_from(x).ok())
                        .ok_or_else(|| malformed("\"point\" with an integer \"srid\""))?,
                    x: p.get("x")
                        .and_then(|x| x.as_f64())
                        .ok_or_else(|| malformed("\"point\" with a number \"x\""))?,
                    y: p.get("y")
                        .and_then(|x| x.as_f64())
                        .ok_or_else(|| malformed("\"point\" with a number \"y\""))?,
                    z: p.get("z").and_then(|x| x.as_f64()),
                })
            }
            "vector" => {
                let Some(next) = next_depth(depth) else {
                    return Err(too_deep());
                };
                let coord = obj
                    .get("coord")
                    .and_then(|x| x.as_str())
                    .and_then(VectorCoordKind::from_canonical_name)
                    .ok_or_else(|| crate::Error::storage("invalid vector coordinate type"))?;
                let xs = obj
                    .get("v")
                    .and_then(|x| x.as_array())
                    .ok_or_else(|| malformed("\"vector\" with an array \"v\""))?;
                // Each entry decodes through the same scalar tagged decode
                // the `Vec<Value>` walk used, then lands packed per family
                // (ADR-0706) behind the checked constructor.
                let values = xs
                    .iter()
                    .map(|x| Value::from_json_tagged_at(x, next))
                    .collect::<crate::Result<Vec<_>>>()?;
                let coords = pack_decoded_values(coord, &values).map_err(crate::Error::storage)?;
                Value::vector(coord, coords)
            }
            other => return Err(crate::Error::storage(format!("unknown type tag {other:?}"))),
        })
    }

    /// Best-effort Python/JSON-ish value for FFI clients.
    pub fn to_json_plain(&self) -> serde_json::Value {
        self.to_json_plain_at(0)
    }

    /// [`Value::to_json_plain`] with the ADR-0462 `depth` guard: a container
    /// past [`MAX_VALUE_DEPTH`] encodes as JSON `null` instead of recursing.
    /// Unreachable behind the checked constructors.
    fn to_json_plain_at(&self, depth: u32) -> serde_json::Value {
        use serde_json::json;
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => json!(b),
            Value::Int(i) => json!(i),
            Value::Float(f) => json!(f),
            Value::String(s) => json!(&**s),
            Value::Uuid(u) => json!(uuid_hyphenated(*u)),
            // The byte values 0-255, the `value_to_cypher` shape (ADR-0327).
            Value::Bytes(b) => json!(&**b),
            Value::List(xs) => {
                let Some(next) = next_depth(depth) else {
                    return serde_json::Value::Null;
                };
                json!(xs
                    .iter()
                    .map(|v| v.to_json_plain_at(next))
                    .collect::<Vec<_>>())
            }
            Value::Map(m) => {
                let Some(next) = next_depth(depth) else {
                    return serde_json::Value::Null;
                };
                map_to_json_plain(m.iter(), next)
            }
            Value::Node(n) => node_json(n, depth),
            Value::Relationship(r) => rel_json(r, depth),
            Value::Path(p) => {
                if next_depth(depth).is_none() {
                    return serde_json::Value::Null;
                }
                // A path's embedded entities are not an extra container
                // level (ADR-0462 depth accounting); they share the path's.
                json!({
                    "_type": "path",
                    "nodes": p.nodes.iter().map(|n| node_json(n, depth)).collect::<Vec<_>>(),
                    "relationships": p.rels.iter().map(|r| rel_json(r, depth)).collect::<Vec<_>>(),
                })
            }
            Value::Temporal(t) => json!({
                "_type": t.kind_name(),
                "value": t.iso_string(),
            }),
            Value::Point(p) => json!({
                "_type": "point", "srid": p.srid, "x": p.x, "y": p.y, "z": p.z
            }),
            Value::Vector { coord, coords } => {
                // No depth: the packed coordinates are scalars, nothing
                // recurses.
                json!({
                    "_type": "vector",
                    "coordinateType": coord.canonical_name(),
                    // The per-`Value` scalars these replaced encoded as
                    // plain JSON numbers; the packed read (ADR-0706) is
                    // the same encoding.
                    "values": coords.iter().map(|c| match c {
                        Coord::I(n) => json!(n),
                        Coord::F(f) => json!(f),
                    }).collect::<Vec<_>>()
                })
            }
        }
    }

    pub fn from_json_plain(v: &serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Float(f)
                } else {
                    Value::Null
                }
            }
            serde_json::Value::String(s) => Value::String(s.as_str().into()),
            serde_json::Value::Array(a) => Value::List(
                a.iter()
                    .map(Value::from_json_plain)
                    .collect::<Vec<_>>()
                    .into(),
            ),
            serde_json::Value::Object(o) => {
                // Probe the `_type` tag exactly once: the handled set
                // ("node", "point", "vector") and the fall-through to a
                // plain map are explicit in one place.
                match o.get("_type").and_then(|t| t.as_str()) {
                    Some("node") => {
                        return Value::Node(Arc::new(Node {
                            id: o.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
                            labels: Arc::new(
                                o.get("labels")
                                    .and_then(|x| x.as_array())
                                    .map(|a| {
                                        a.iter()
                                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default(),
                            ),
                            properties: match o.get("properties") {
                                Some(p) => match Value::from_json_plain(p) {
                                    Value::Map(m) => PropertyMap::share(Arc::unwrap_or_clone(m)),
                                    _ => PropertyMap::share(PropertyMap::new()),
                                },
                                None => PropertyMap::share(PropertyMap::new()),
                            },
                        }));
                    }
                    Some("point") => {
                        return Value::Point(PointValue {
                            srid: o
                                .get("srid")
                                .and_then(|x| x.as_i64())
                                .and_then(|x| i32::try_from(x).ok())
                                .unwrap_or(7203),
                            x: o.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0),
                            y: o.get("y").and_then(|x| x.as_f64()).unwrap_or(0.0),
                            z: o.get("z").and_then(|x| x.as_f64()),
                        });
                    }
                    Some("vector") => {
                        if let Some(coord) = o
                            .get("coordinateType")
                            .and_then(|x| x.as_str())
                            .and_then(VectorCoordKind::from_canonical_name)
                        {
                            // A missing or non-array `values` is not a
                            // vector — fall through to the plain-map path
                            // (an explicit `[]` stays the legitimate
                            // empty vector).
                            let Some(values) = o.get("values").and_then(|x| x.as_array()) else {
                                return Value::Map(
                                    o.iter()
                                        .map(|(k, v)| (k.clone(), Value::from_json_plain(v)))
                                        .collect::<BTreeMap<_, _>>()
                                        .into(),
                                );
                            };
                            let values: Vec<Value> =
                                values.iter().map(Value::from_json_plain).collect();
                            // Best-effort decoder: a shape the packed
                            // invariant rejects (ADR-0706) falls back to
                            // this decoder's own unrecognized-tag path —
                            // the plain map — instead of a panic or a
                            // malformed Vector.
                            if let Ok(coords) = pack_decoded_values(coord, &values) {
                                return Value::vector(coord, coords);
                            }
                        }
                    }
                    _ => {}
                }
                let mut m = BTreeMap::new();
                for (k, val) in o {
                    m.insert(k.clone(), Value::from_json_plain(val));
                }
                Value::Map(m.into())
            }
        }
    }

    /// Decode JSON whose objects are always maps. A `_type` of `node`,
    /// `point`, or `vector` stays a string property. This is the decoder for
    /// untrusted property documents. [`from_json_plain`] is the inverse of
    /// [`to_json_plain`] and still recognizes those tags.
    pub fn from_json_scalars(v: &serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Float(f)
                } else {
                    Value::Null
                }
            }
            serde_json::Value::String(s) => Value::String(s.as_str().into()),
            serde_json::Value::Array(a) => Value::List(
                a.iter()
                    .map(Value::from_json_scalars)
                    .collect::<Vec<_>>()
                    .into(),
            ),
            serde_json::Value::Object(o) => {
                let mut m = BTreeMap::new();
                for (k, val) in o {
                    m.insert(k.clone(), Value::from_json_scalars(val));
                }
                Value::Map(m.into())
            }
        }
    }
}

/// Borrowing encoder of the tagged string `{"t":"string","v":...}`.
/// Construct it with [`tagged_str`]. [`Value`]'s tagged encoder writes the
/// same two entries.
pub struct TaggedStr<'a>(&'a str);

fn write_tagged_str<M: serde::ser::SerializeMap>(m: &mut M, s: &str) -> Result<(), M::Error> {
    m.serialize_entry("t", "string")?;
    m.serialize_entry("v", s)?;
    Ok(())
}

impl serde::Serialize for TaggedStr<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = serializer.serialize_map(Some(2))?;
        write_tagged_str(&mut m, self.0)?;
        m.end()
    }
}

/// The tagged string [`TaggedRef`] writes for [`Value::String`].
pub fn tagged_str(s: &str) -> TaggedStr<'_> {
    TaggedStr(s)
}

/// Serializes a [`Value`] as the tagged `{"t":..., ...}` JSON that both the
/// WAL and the Parquet checkpoint codec record — directly, without building an
/// intermediate `serde_json::Value` tree. This is the single canonical tagged
/// encoder; [`Value::to_json_tagged`] produces its `serde_json::Value` from it,
/// so the two cannot drift.
///
/// Graph entities ([`Value::Node`], [`Value::Relationship`],
/// [`Value::Path`]) are not persistence properties: encoding one is a
/// serialization error, mirroring the v2 persistence codec's "graph entity
/// is not a persistence property" rejection. Byte arrays are the second
/// not-a-persistence-property rejection class (ADR-0327: statement-scoped,
/// and the executor's property check rejects them with a typed `TypeError`
/// before persistence), with their own "byte array is not a persistence
/// property" error.
struct TaggedRef<'a>(&'a Value);

impl serde::Serialize for TaggedRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        // The arms write two entries except the vector arm's three
        // ("coord"/"t"/"v"), so no fixed arity is declared.
        let mut m = serializer.serialize_map(None)?;
        match self.0 {
            Value::Null => m.serialize_entry("t", "null")?,
            Value::Bool(b) => {
                m.serialize_entry("t", "bool")?;
                m.serialize_entry("v", b)?;
            }
            Value::Int(i) => write_tagged_number(&mut m, Coord::I(*i))?,
            Value::Float(f) => write_tagged_number(&mut m, Coord::F(*f))?,
            Value::String(s) => write_tagged_str(&mut m, s)?,
            Value::Uuid(u) => {
                m.serialize_entry("t", "uuid")?;
                m.serialize_entry("v", &uuid_hyphenated(*u))?;
            }
            Value::List(xs) => {
                m.serialize_entry("t", "list")?;
                m.serialize_entry("v", &TaggedListRef(xs))?;
            }
            Value::Map(map) => {
                m.serialize_entry("t", "map")?;
                m.serialize_entry("v", &TaggedMapRef(map))?;
            }
            // Invariant: this encoder records graph *property* values only,
            // and a property is never a graph entity -- `sanitize_props`
            // (relate-executor) rejects Node/Relationship/Path before
            // persistence, and the documented v2 persistence codec
            // (relate-model `serde_value`) errors with "graph entity is
            // not a persistence property". Fail loudly instead of silently
            // writing the entity as a null tag.
            Value::Node(_) | Value::Relationship(_) | Value::Path(_) => {
                return Err(serde::ser::Error::custom(
                    "graph entity is not a persistence property",
                ));
            }
            // Invariant, as for graph entities: a byte array is
            // statement-scoped (ADR-0327) and the executor's property check
            // rejects it with a typed TypeError before persistence. The
            // tagged codec has no byte-array tag; fail loudly instead.
            Value::Bytes(_) => {
                return Err(serde::ser::Error::custom(
                    "byte array is not a persistence property",
                ));
            }
            Value::Temporal(t) => {
                m.serialize_entry("t", t.kind_name())?;
                m.serialize_entry("v", &t.iso_string())?;
            }
            Value::Point(p) => {
                m.serialize_entry("t", "point")?;
                m.serialize_entry("v", &TaggedPointRef(p))?;
            }
            Value::Vector { coord, coords } => {
                // serde_json's Map is a BTreeMap: keys serialize sorted, so
                // "coord" precedes "t". Match the historical bytes.
                m.serialize_entry("coord", coord.canonical_name())?;
                m.serialize_entry("t", "vector")?;
                m.serialize_entry("v", &TaggedCoordsRef(coords))?;
            }
        }
        m.end()
    }
}

struct TaggedListRef<'a>(&'a [Value]);
impl serde::Serialize for TaggedListRef<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for v in self.0 {
            seq.serialize_element(&TaggedRef(v))?;
        }
        seq.end()
    }
}

/// A packed vector's coordinates as the tagged sequence the
/// `Vec<Value>` form serialized (ADR-0706): each coordinate is the same
/// `{"t": "int"/"float", "v": …}` entry `TaggedRef` emitted for the
/// scalar it replaces (coordinates are finite by construction, so the
/// NaN/Infinity spellings are unreachable).
struct TaggedCoordsRef<'a>(&'a VectorCoords);
impl serde::Serialize for TaggedCoordsRef<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let coords = self.0;
        let mut seq = s.serialize_seq(Some(coords.len()))?;
        for c in coords.iter() {
            seq.serialize_element(&TaggedCoordRef(&c))?;
        }
        seq.end()
    }
}

struct TaggedCoordRef<'a>(&'a Coord);
impl serde::Serialize for TaggedCoordRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = serializer.serialize_map(Some(2))?;
        write_tagged_number(&mut m, *self.0)?;
        m.end()
    }
}

struct TaggedMapRef<'a>(&'a BTreeMap<String, Value>);
impl serde::Serialize for TaggedMapRef<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in self.0 {
            m.serialize_entry(k, &TaggedRef(v))?;
        }
        m.end()
    }
}

struct TaggedPointRef<'a>(&'a PointValue);
impl serde::Serialize for TaggedPointRef<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let p = self.0;
        let mut m = s.serialize_map(Some(4))?;
        m.serialize_entry("srid", &p.srid)?;
        m.serialize_entry("x", &p.x)?;
        m.serialize_entry("y", &p.y)?;
        m.serialize_entry("z", &p.z)?;
        m.end()
    }
}

impl serde::Serialize for Value {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // ADR-0462: one bounded pre-walk at the public entry keeps the
        // recursive `TaggedRef` encoder inside the depth cap. Unreachable
        // behind the checked constructors — a graph property cannot nest
        // past the cap without a typed error at construction.
        if !self.depth_within(MAX_VALUE_DEPTH) {
            return Err(serde::ser::Error::custom(value_depth_exceeded("value")));
        }
        TaggedRef(self).serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Value {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let json = serde_json::Value::deserialize(deserializer)?;
        Value::from_json_tagged(&json).map_err(serde::de::Error::custom)
    }
}

/// The tagged number vocabulary (`{"t": "int"/"float", "v": …}`) — one
/// spelling for `TaggedRef`'s scalar arms and the packed-coordinate
/// encoder (ADR-0706), including the non-finite float spellings.
fn write_tagged_number<M: serde::ser::SerializeMap>(m: &mut M, c: Coord) -> Result<(), M::Error> {
    match c {
        Coord::I(n) => {
            m.serialize_entry("t", "int")?;
            m.serialize_entry("v", &n)?;
        }
        Coord::F(f) => {
            m.serialize_entry("t", "float")?;
            if f.is_nan() {
                m.serialize_entry("v", "NaN")?;
            } else if f == f64::INFINITY {
                m.serialize_entry("v", "Infinity")?;
            } else if f == f64::NEG_INFINITY {
                m.serialize_entry("v", "-Infinity")?;
            } else {
                m.serialize_entry("v", &f)?;
            }
        }
    }
    Ok(())
}

fn map_to_json_plain<'a>(
    m: impl IntoIterator<Item = (&'a String, &'a Value)>,
    depth: u32,
) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    for (k, v) in m {
        o.insert(k.clone(), v.to_json_plain_at(depth));
    }
    serde_json::Value::Object(o)
}

/// Plain-JSON form of a node, shared by the [`Value::Node`] arm and the
/// [`Value::Path`] arm of [`Value::to_json_plain`], which renders each path
/// element directly instead of cloning it into a [`Value`] and re-entering
/// the dispatcher one arm down.
fn node_json(n: &Node, depth: u32) -> serde_json::Value {
    // `depth` is the node's ancestor-container count; the property values sit
    // one container deeper. The guard matches the sibling container arms and
    // returns the null form (ADR-0462).
    let Some(next) = next_depth(depth) else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "_type": "node",
        "id": n.id,
        "labels": n.labels,
        "properties": map_to_json_plain(n.properties.iter(), next),
    })
}

/// Plain-JSON form of a relationship (see [`node_json`]).
fn rel_json(r: &Relationship, depth: u32) -> serde_json::Value {
    let Some(next) = next_depth(depth) else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "_type": "relationship",
        "id": r.id,
        "start": r.start,
        "end": r.end,
        "type": r.rel_type,
        "properties": map_to_json_plain(r.properties.iter(), next),
    })
}

/// Decode-side packing (ADR-0706): scalars a tagged/plain JSON decode
/// produced, validated per `coord` kind and packed into the family
/// buffer. The storage path fails closed on a crafted document (the
/// `Vec<Value>` decode this replaces accepted inconsistent coordinates
/// and let the encoder reject them later); the lenient
/// `from_json_plain` path treats an `Err` as an unrecognized shape.
pub fn pack_decoded_values(
    coord: VectorCoordKind,
    values: &[Value],
) -> Result<VectorCoords, String> {
    // The decode-side classification (fail-closed with this codec's own
    // error detail), landing in the ONE family-selection policy
    // (ADR-0706's `VectorCoords::from_coords`) — the family mapping, the
    // empty vector and the one-family rule have their single spelling
    // there.
    let reject = |what: &str| format!("vector {what}");
    let mut tokens = Vec::with_capacity(values.len());
    if coord.is_integer() {
        for v in values {
            let Value::Int(n) = v else {
                return Err(reject("coordinate is not an integer"));
            };
            if !coord.holds_int(*n) {
                return Err(reject("coordinate out of kind range"));
            }
            tokens.push(Coord::I(*n));
        }
    } else {
        for v in values {
            match v {
                Value::Int(n) => tokens.push(Coord::I(*n)),
                Value::Float(f) if f.is_finite() && coord.is_exact(*f) => {
                    tokens.push(Coord::F(*f));
                }
                Value::Float(_) => {
                    return Err(reject("coordinate is not finite or loses precision"));
                }
                _ => return Err(reject("coordinate is not a number")),
            }
        }
    }
    VectorCoords::from_coords(coord, tokens.into_iter())
        .ok_or_else(|| reject("coordinates mix integer and float forms"))
}
