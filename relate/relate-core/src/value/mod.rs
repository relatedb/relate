use std::collections::BTreeMap;
use std::fmt;
use std::ops::ControlFlow;
use std::sync::Arc;

pub use relate_model::value::PointValue;
pub use relate_model::VectorCoordKind;

use relate_model::identifier::quote_identifier_shaped;

use crate::temporal::Temporal;
use crate::{Error, NodeId, RelId};

mod depth;
mod json;
mod ordering;
mod props;
pub(crate) use depth::{descends, DEPTH_LIMIT_MARKER};
pub use depth::{next_depth, value_depth_exceeded, MAX_VALUE_DEPTH};
pub use json::{tagged_str, TaggedStr};
pub use props::PropertyMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub labels: Arc<Vec<String>>,
    pub properties: Arc<PropertyMap>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Relationship {
    pub id: RelId,
    pub start: NodeId,
    pub end: NodeId,
    pub rel_type: String,
    pub properties: Arc<PropertyMap>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub nodes: Vec<Node>,
    pub rels: Vec<Relationship>,
}

/// OpenCypher runtime value.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(Arc<str>),
    List(Arc<Vec<Value>>),
    Map(Arc<BTreeMap<String, Value>>),
    Node(Arc<Node>),
    Relationship(Arc<Relationship>),
    Path(Arc<Path>),
    Temporal(Temporal),
    Point(PointValue),
    /// CIP-246 UUID: 128-bit value, rendered as the canonical lowercase
    /// hyphenated string (`to_display_string`/`iso`-style bare form) and
    /// constructible from a string or two 64-bit halves.
    Uuid(u128),
    Vector {
        coord: VectorCoordKind,
        values: Arc<Vec<Value>>,
    },
    /// A byte array (ADR-0327). Statement-scoped: it enters only as a query
    /// parameter (Bolt PackStream Bytes) and flows through expressions,
    /// lists and maps into results; storing it as a property fails with a
    /// typed `TypeError`. Shared payload like `String` (ADR-0170).
    Bytes(Arc<[u8]>),
}

// Query results cross worker threads, so the shared payloads must keep
// `Value: Send + Sync` (ADR-0170).
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Value>();
};

/// Whether the summed logical bytes (ADR-0042, as
/// [`Value::logical_bytes`]) of `values` stay within `limit`, or `None`
/// once the running total exceeds it. The one early-stopping logical-bytes
/// walk (ADR-0341): the total stops being computed as soon as it exceeds
/// `limit` instead of walking every value.
///
/// Shared payloads (ADR-0170) still count once per reference: many rows
/// holding one large list are small in memory but large in logical bytes,
/// and an unbounded sum over them does work proportional to the product.
/// The Bolt server sums its open results against one connection-wide cap
/// with it (ADR-0311).
pub fn logical_bytes_within<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    limit: u64,
) -> Option<u64> {
    let mut total = 0;
    let over = values
        .into_iter()
        .try_for_each(|v| v.add_logical_bytes(&mut total, limit, 0))
        .is_break();
    (!over).then_some(total)
}

/// Adds `bytes` to `acc` (saturating) and breaks once `acc` exceeds `budget`.
fn add_bytes(acc: &mut u64, bytes: u64, budget: u64) -> ControlFlow<()> {
    *acc = acc.saturating_add(bytes);
    if *acc > budget {
        ControlFlow::Break(())
    } else {
        ControlFlow::Continue(())
    }
}

/// Add one string's logical size — [`str_logical_bytes`] is the one
/// spelling of the size; this is only its budgeted accumulation.
fn add_str_bytes(s: &str, acc: &mut u64, budget: u64) -> ControlFlow<()> {
    add_bytes(acc, str_logical_bytes(s), budget)
}

/// Add one byte payload's logical size (`Value::Bytes`): header 16 plus
/// its length — the same shape [`str_logical_bytes`] spells for a
/// string's UTF-8 bytes.
fn add_payload_bytes(payload: &[u8], acc: &mut u64, budget: u64) -> ControlFlow<()> {
    add_bytes(acc, bytes_logical_bytes(payload), budget)
}

fn add_property_bytes<'a>(
    props: impl IntoIterator<Item = (&'a String, &'a Value)>,
    acc: &mut u64,
    budget: u64,
    depth: u32,
) -> ControlFlow<()> {
    props.into_iter().try_for_each(|(k, v)| {
        add_str_bytes(k, acc, budget)?;
        v.add_logical_bytes(acc, budget, depth)
    })
}

fn add_node_bytes(n: &Node, acc: &mut u64, budget: u64, depth: u32) -> ControlFlow<()> {
    n.labels
        .iter()
        .try_for_each(|l| add_str_bytes(l, acc, budget))?;
    add_property_bytes(n.properties.iter(), acc, budget, depth)
}

fn add_rel_bytes(r: &Relationship, acc: &mut u64, budget: u64, depth: u32) -> ControlFlow<()> {
    add_str_bytes(&r.rel_type, acc, budget)?;
    add_property_bytes(r.properties.iter(), acc, budget, depth)
}

/// A string's documented logical size (ADR-0042): the 16-byte header
/// plus its UTF-8 bytes — the one spelling every string-size sum reads
/// (row keys, node and relationship property keys), beside
/// [`Value::logical_bytes`].
#[inline]
pub fn str_logical_bytes(s: &str) -> u64 {
    16 + s.len() as u64
}

/// A byte payload's documented logical size (ADR-0042): the 16-byte header
/// plus its bytes — the one spelling every payload-size site reads, beside
/// [`str_logical_bytes`] and [`Value::logical_bytes`].
#[inline]
pub fn bytes_logical_bytes(payload: &[u8]) -> u64 {
    16 + payload.len() as u64
}

/// The one float rendering policy: `NaN`, `±Infinity`, otherwise Rust's
/// shortest round-tripping decimal form with a `.0` appended when `Display`
/// emits no fractional part (so `1.0` never reads as the integer `1`).
///
/// Never scientific: the decimal form parses back to the same `f64`, and a
/// second exponent policy for vector coordinates (the historical `1e16`
/// threshold) made a value's literal and display forms disagree. Both
/// [`Value::to_cypher_literal_at`] (its scalar and vector-coordinate arms)
/// and [`Value::to_display_string_at`] call this, so scalar and vector
/// coordinates share one spelling.
fn float_literal(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f == f64::INFINITY {
        return "Infinity".into();
    }
    if f == f64::NEG_INFINITY {
        return "-Infinity".into();
    }
    let mut rendered = format!("{f}");
    if !rendered.contains('.') {
        rendered.push_str(".0");
    }
    rendered
}

impl Value {
    /// Share one node across every row that clones this value.
    pub fn from_node(node: Node) -> Self {
        Value::Node(Arc::new(node))
    }

    /// Share one relationship across every row that clones this value.
    pub fn from_relationship(rel: Relationship) -> Self {
        Value::Relationship(Arc::new(rel))
    }

    /// Share one path across every row that clones this value.
    pub fn from_path(path: Path) -> Self {
        Value::Path(Arc::new(path))
    }

    /// Documented logical payload size (ADR-0042) — the per-variant
    /// accumulation of the sizes [`str_logical_bytes`] spells for
    /// strings.
    pub fn logical_bytes(&self) -> u64 {
        let mut total = 0;
        let flow = self.add_logical_bytes(&mut total, u64::MAX, 0);
        // The byte budget is `u64::MAX`, so the only reachable `Break` is
        // the ADR-0462 depth guard: were it to fire (unreachable behind the
        // checked constructors) the result would be the partial sum counted
        // down to the cap rather than a panic, since this walk has no error
        // channel.
        debug_assert!(
            flow.is_continue(),
            "logical_bytes broke on the ADR-0462 depth guard at the cap"
        );
        total
    }

    /// The single implementation of the ADR-0042 formula, shared by
    /// [`Value::logical_bytes`] (budget `u64::MAX`, never breaks) and
    /// [`logical_bytes_within`]: adds this value's logical bytes to `acc`
    /// and breaks as soon as `acc` exceeds `budget`.
    ///
    /// `depth` is the level of the container whose contents are being
    /// added; past [`MAX_VALUE_DEPTH`] the walk breaks (ADR-0462). The
    /// guard is unreachable behind the checked constructors and only keeps
    /// an over-deep value from recursing without a byte budget to stop it.
    fn add_logical_bytes(&self, acc: &mut u64, budget: u64, depth: u32) -> ControlFlow<()> {
        match self {
            Value::Null | Value::Bool(_) => add_bytes(acc, 1, budget),
            Value::Int(_) | Value::Float(_) => add_bytes(acc, 8, budget),
            Value::String(s) => add_str_bytes(s, acc, budget),
            Value::Bytes(b) => add_payload_bytes(b, acc, budget),
            Value::List(xs) | Value::Vector { values: xs, .. } => {
                let Some(next) = next_depth(depth) else {
                    return ControlFlow::Break(());
                };
                add_bytes(acc, 16, budget)?;
                xs.iter()
                    .try_for_each(|x| x.add_logical_bytes(acc, budget, next))
            }
            Value::Map(m) => {
                let Some(next) = next_depth(depth) else {
                    return ControlFlow::Break(());
                };
                add_bytes(acc, 16, budget)?;
                add_property_bytes(m.iter(), acc, budget, next)
            }
            Value::Node(n) => {
                let Some(next) = next_depth(depth) else {
                    return ControlFlow::Break(());
                };
                add_node_bytes(n, acc, budget, next)
            }
            Value::Relationship(r) => {
                let Some(next) = next_depth(depth) else {
                    return ControlFlow::Break(());
                };
                add_rel_bytes(r, acc, budget, next)
            }
            Value::Path(p) => {
                let Some(next) = next_depth(depth) else {
                    return ControlFlow::Break(());
                };
                p.nodes
                    .iter()
                    .try_for_each(|n| add_node_bytes(n, acc, budget, next))?;
                p.rels
                    .iter()
                    .try_for_each(|r| add_rel_bytes(r, acc, budget, next))
            }
            Value::Temporal(_) => add_bytes(acc, 24, budget),
            Value::Point(_) => add_bytes(acc, 32, budget),
            Value::Uuid(_) => add_bytes(acc, 16, budget),
        }
    }

    pub fn as_temporal(&self) -> Option<&Temporal> {
        match self {
            Value::Temporal(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(&**s),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Cypher toBoolean-ish: true/false stay, null stays none.
    pub fn truthy(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Null => None,
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f)
                if f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 =>
            {
                Some(*f as i64)
            }
            _ => None,
        }
    }

    pub fn to_cypher_literal(&self) -> String {
        self.to_cypher_literal_at(0)
    }

    /// [`Value::to_cypher_literal`] with the ADR-0462 depth guard: a
    /// container past [`MAX_VALUE_DEPTH`] renders
    /// [`DEPTH_LIMIT_MARKER`] instead of recursing. `depth` is the number of
    /// containers enclosing `self`, so a root is 0 and its children 1.
    fn to_cypher_literal_at(&self, depth: u32) -> String {
        match self {
            Value::Null => "null".into(),
            Value::Bool(true) => "true".into(),
            Value::Bool(false) => "false".into(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => float_literal(*f),
            Value::String(s) => format!("'{}'", escape_cypher_string(s)),
            Value::Bytes(b) => bytes_as_int_list(b),
            Value::List(xs) => {
                let Some(next) = next_depth(depth) else {
                    return DEPTH_LIMIT_MARKER.into();
                };
                let inner: Vec<_> = xs.iter().map(|v| v.to_cypher_literal_at(next)).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Map(m) => {
                let Some(next) = next_depth(depth) else {
                    return DEPTH_LIMIT_MARKER.into();
                };
                map_to_cypher_literal(m.iter(), next)
            }
            Value::Node(n) => n.to_cypher_literal_at(depth),
            Value::Relationship(r) => r.to_cypher_literal_at(depth),
            Value::Path(p) => p.to_cypher_literal_at(depth),
            Value::Temporal(t) => format!("'{}'", escape_cypher_string(&t.iso_string())),
            // Round-trippable form like VECTOR: uuid('...') (CIP-246).
            Value::Uuid(u) => format!("uuid('{}')", uuid_hyphenated(*u)),
            Value::Point(p) => p.to_string(),
            Value::Vector { coord, values } => {
                let Some(next) = next_depth(depth) else {
                    return DEPTH_LIMIT_MARKER.into();
                };
                let inner = values
                    .iter()
                    .map(|value| value.to_cypher_literal_at(next))
                    .collect::<Vec<_>>();
                vector_text(&inner, *coord)
            }
        }
    }

    /// The CIP-271 `toString()` rendering: the cypher-shell display form
    /// rather than the round-trippable literal form — strings are unquoted,
    /// temporals are bare full-precision ISO strings (ADR-0448), and
    /// maps/lists render recursively with `, ` separators (verified
    /// against Neo4j 2026.09.0).
    pub fn to_display_string(&self) -> String {
        self.to_display_string_at(0)
    }

    /// [`Value::to_display_string`] with the ADR-0462 `depth` guard (the
    /// number of containers enclosing `self`); a container past
    /// [`MAX_VALUE_DEPTH`] renders [`DEPTH_LIMIT_MARKER`].
    fn to_display_string_at(&self, depth: u32) -> String {
        match self {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => float_literal(*f),
            // Unquoted: Neo4j 2026.09.0 renders toString({a:'x y'}) as
            // "{a: x y}".
            Value::String(s) => String::from(&**s),
            Value::Bytes(b) => bytes_as_int_list(b),
            Value::List(xs) => {
                let Some(next) = next_depth(depth) else {
                    return DEPTH_LIMIT_MARKER.into();
                };
                let inner: Vec<_> = xs.iter().map(|v| v.to_display_string_at(next)).collect();
                format!("[{}]", inner.join(", "))
            }
            // A key outside the plain identifier shape is backticked
            // (CIP-271: `{`123`: digit key, `a.b`: dot, `first name`:
            // Alice}`); the map's own iteration order is already the
            // sorted one.
            Value::Map(m) => {
                let Some(next) = next_depth(depth) else {
                    return DEPTH_LIMIT_MARKER.into();
                };
                let inner: Vec<_> = m
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{}: {}",
                            quote_identifier_shaped(k),
                            v.to_display_string_at(next)
                        )
                    })
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
            Value::Node(n) => node_display(n),
            Value::Relationship(r) => relationship_display(r),
            // No depth: the display walk renders no properties, so it never
            // recurses and has no ADR-0462 budget to thread.
            Value::Path(p) => p.display_string(),
            Value::Temporal(t) => t.full_seconds_string(),
            // Bare hyphenated string (verified on 2026.09.0).
            Value::Uuid(u) => uuid_hyphenated(*u),
            Value::Point(pt) => point_display(pt),
            Value::Vector { coord, values } => {
                let Some(next) = next_depth(depth) else {
                    return DEPTH_LIMIT_MARKER.into();
                };
                let inner: Vec<_> = values
                    .iter()
                    .map(|v| v.to_display_string_at(next))
                    .collect();
                vector_text(&inner, *coord)
            }
        }
    }
}

/// One component accessor of a point (`p.x`, `p.longitude`, `p.crs`, …) —
/// the one point-accessor vocabulary, the data-model sibling of the
/// temporal accessor ([`crate::temporal::Temporal::component`]): a key no
/// component names is the same typed rejection the temporal reader answers,
/// never a silently null read. The coordinate aliases (`x` is `longitude`)
/// are the shared axis vocabulary beside [`PointValue`].
///
/// Which axis spellings a point exposes is live Neo4j 2026.10.1's rule
/// (probed 2026-11-11): a cartesian point has only the cartesian
/// spellings (`x`/`y`/`z`), a geographic point accepts both (`x` and
/// `longitude`, …), and the third axis exists only on a 3D point. An
/// unavailable spelling is `ArgumentError` "Field: <key> is not
/// available", not a null read.
pub fn point_component(p: &PointValue, key: &str) -> Result<Value, Error> {
    let name = key.to_ascii_lowercase();
    Ok(match name.as_str() {
        "srid" => Value::Int(i64::from(p.srid)),
        // The canonical CRS spelling; a decoder-supplied srid/dimensionality
        // pair outside the four canonical codes keeps the srid table's
        // spelling rather than claiming no CRS.
        "crs" => Value::String(
            p.crs_name()
                .unwrap_or_else(|| relate_model::wire::crs_name(p.srid))
                .into(),
        ),
        _ => {
            let Some(axis) = relate_model::point::point_axis(&name) else {
                // 22G03 invalid argument, not a runtime TypeError.
                return Err(Error::argument_error(format!(
                    "no such component '{key}' on a point value"
                )));
            };
            // A cartesian point has no geographic spelling; any point has
            // the third axis only when it is 3D (`p.z` is its presence).
            let cartesian = !relate_model::point::is_geographic(p.srid);
            let unavailable =
                (cartesian && axis.key(true) == name) || (axis.index == 2 && p.z.is_none());
            if unavailable {
                return Err(Error::argument_error(format!(
                    "Field: {key} is not available on a point value"
                )));
            }
            match axis.index {
                0 => Value::Float(p.x),
                1 => Value::Float(p.y),
                _ => Value::Float(p.z.expect("the 3D check admitted only a present z")),
            }
        }
    })
}

/// The Neo4j text form of a point (the CIP-271 display policy `toString()`
/// and string concatenation use): `point({x: 1.0, y: 2.0, crs:
/// 'cartesian'})`. Coordinates render through the float-literal policy (so
/// an integral coordinate keeps its `.0`), a 3D point adds `z`, and the CRS
/// name comes from [`PointValue::crs_name`]. A srid/dimensionality pair
/// outside the four `point()` codes — only reachable from a decoder, never
/// from `point()` — renders the srid spelling rather than claiming a CRS
/// the value does not have.
fn point_display(p: &PointValue) -> String {
    let coord = |value: f64| Value::Float(value).to_cypher_literal();
    match (p.crs_name(), p.z) {
        (Some(crs), Some(z)) => format!(
            "point({{x: {}, y: {}, z: {}, crs: '{crs}'}})",
            coord(p.x),
            coord(p.y),
            coord(z)
        ),
        (Some(crs), None) => format!(
            "point({{x: {}, y: {}, crs: '{crs}'}})",
            coord(p.x),
            coord(p.y)
        ),
        (None, Some(z)) => format!(
            "point({{srid: {}, x: {}, y: {}, z: {}}})",
            p.srid,
            coord(p.x),
            coord(p.y),
            coord(z)
        ),
        (None, None) => format!(
            "point({{srid: {}, x: {}, y: {}}})",
            p.srid,
            coord(p.x),
            coord(p.y)
        ),
    }
}

/// The `vector([…], N, COORD)` text shape shared by the literal and
/// display renderers; `elements` are the already-rendered coordinates.
/// The round-trippable value-constructor form (CIP-271): a vector VALUE
/// renders as `vector([1, 2], 2, INTEGER8)` — the coordinate type is the
/// third argument, not a `NOT NULL`-suffixed type expression (that is the
/// TYPE rendering's own path, e.g. `valueType()`).
fn vector_text(elements: &[String], coord: VectorCoordKind) -> String {
    format!(
        "vector([{}], {}, {})",
        elements.join(", "),
        elements.len(),
        coord.canonical_name()
    )
}

impl Node {
    pub fn to_cypher_literal(&self) -> String {
        self.to_cypher_literal_at(0)
    }

    /// [`Node::to_cypher_literal`] with the ADR-0462 `depth` (the number of
    /// containers enclosing this node).
    fn to_cypher_literal_at(&self, depth: u32) -> String {
        let Some(next) = next_depth(depth) else {
            return DEPTH_LIMIT_MARKER.into();
        };
        let labels: String = self.labels.iter().map(|l| format!(":{l}")).collect();
        let props = props_suffix(&self.properties, next);
        format!("({labels}{props})")
    }
}

impl Relationship {
    pub fn to_cypher_literal(&self) -> String {
        self.to_cypher_literal_at(0)
    }

    /// [`Relationship::to_cypher_literal`] with the ADR-0462 `depth`.
    fn to_cypher_literal_at(&self, depth: u32) -> String {
        let Some(next) = next_depth(depth) else {
            return DEPTH_LIMIT_MARKER.into();
        };
        let props = props_suffix(&self.properties, next);
        format!("[:{}{}]", self.rel_type, props)
    }
}

impl Path {
    pub fn to_cypher_literal(&self) -> String {
        self.to_cypher_literal_at(0)
    }

    /// [`Path::to_cypher_literal`] with the ADR-0462 `depth`.
    fn to_cypher_literal_at(&self, depth: u32) -> String {
        // The path itself is the container (`depth + 1`); its embedded
        // entities are not an extra level, so they get `depth`.
        if next_depth(depth).is_none() {
            return DEPTH_LIMIT_MARKER.into();
        }
        format!(
            "<{}>",
            self.walk(
                |n| n.to_cypher_literal_at(depth),
                |r| r.to_cypher_literal_at(depth)
            )
        )
    }

    /// The CIP-271 `toString()` form of a path: no enclosing `<…>` and no
    /// properties — the same label-only / type-only element rendering a
    /// standalone node or relationship gets (Neo4j 2026.10.1 renders
    /// `toString(path)` as `(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C)`). The
    /// display walk renders no properties, so nothing recurses and no
    /// depth budget applies — the literal twin's guard bounds its own
    /// property-rendering recursion, not this walk.
    fn display_string(&self) -> String {
        self.walk(node_display, relationship_display)
    }

    /// The path's element chain — one node then, if the path continues past
    /// it, the relationship traversed from it, with the arrows of the
    /// direction actually stored. `node`/`rel` render one element, so the
    /// literal and display walks share the direction policy.
    fn walk(
        &self,
        node: impl Fn(&Node) -> String,
        rel: impl Fn(&Relationship) -> String,
    ) -> String {
        let mut s = String::new();
        for (i, n) in self.nodes.iter().enumerate() {
            s.push_str(&node(n));
            if let Some(r) = self.rels.get(i) {
                // `rels[i]` connects `nodes[i]` and `nodes[i+1]`, so the
                // arrow is the one bit that is always defined: which
                // endpoint is the current node. A self-loop is outgoing.
                let (left, right) = if r.start == n.id {
                    ("-", "->")
                } else {
                    ("<-", "-")
                };
                s.push_str(&format!("{left}{}{right}", rel(r)));
            }
        }
        s
    }
}

/// Labels only, no properties, alphabetically (CIP-271): Neo4j 2026.09.0
/// renders `toString(node-with-props)` as `(:X)` and a multi-label node as
/// `(:A:B:C)` whatever the creation order.
fn node_display(n: &Node) -> String {
    let mut labels: Vec<&str> = n.labels.iter().map(String::as_str).collect();
    labels.sort_unstable();
    let labels: String = labels
        .iter()
        .map(|l| format!(":{}", quote_identifier_shaped(l)))
        .collect();
    format!("({labels})")
}

/// Type only, no properties (CIP-271), the relationship half of
/// [`node_display`].
fn relationship_display(r: &Relationship) -> String {
    format!("[:{}]", quote_identifier_shaped(&r.rel_type))
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_cypher_literal())
    }
}

/// Cypher has no byte-array literal: a [`Value::Bytes`] renders as the list
/// of its byte values 0-255 (`[0, 1, 255]`), the same shape it crosses the
/// `value_to_cypher` bridge in (ADR-0327).
fn bytes_as_int_list(bytes: &[u8]) -> String {
    let inner: Vec<_> = bytes.iter().map(u8::to_string).collect();
    format!("[{}]", inner.join(", "))
}

fn escape_cypher_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The ` {…}` property suffix shared by the node and relationship literal
/// renderers (one owner for the policy and the ADR-0462 depth hand-off);
/// empty when the entity has no properties. `depth` is the property values'
/// enclosing-container count.
fn props_suffix(props: &PropertyMap, depth: u32) -> String {
    if props.is_empty() {
        String::new()
    } else {
        format!(" {}", map_to_cypher_literal(props.iter(), depth))
    }
}

fn map_to_cypher_literal<'a>(
    m: impl IntoIterator<Item = (&'a String, &'a Value)>,
    depth: u32,
) -> String {
    let inner: Vec<_> = m
        .into_iter()
        .map(|(k, v)| format!("{}: {}", k, v.to_cypher_literal_at(depth)))
        .collect();
    format!("{{{}}}", inner.join(", "))
}

/// Render a 128-bit UUID value as the canonical lowercase hyphenated
/// string (8-4-4-4-12, CIP-246 / RFC 4122).
pub fn uuid_hyphenated(u: u128) -> String {
    let hex = format!("{u:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Parse an RFC-4122-shaped UUID string (case-insensitive, hyphens
/// required, 8-4-4-4-12 groups) into its 128-bit value. `None` on any
/// non-conforming input, mirroring Java's `UUID.fromString` strictness
/// (CIP-246).
pub fn uuid_parse(s: &str) -> Option<u128> {
    // Allocation-free single pass over the original string: `to_digit(16)`
    // accepts both hex cases, so no lowercasing or concatenation is needed.
    // Strictness is unchanged -- exactly five groups of 8-4-4-4-12 hex digits
    // separated by four hyphens, with nothing before or after.
    const GROUP_LEN: [usize; 5] = [8, 4, 4, 4, 12];
    let mut chars = s.chars();
    let mut u: u128 = 0;
    for (i, &len) in GROUP_LEN.iter().enumerate() {
        if i > 0 && chars.next()? != '-' {
            return None;
        }
        for _ in 0..len {
            u = (u << 4) | chars.next()?.to_digit(16)? as u128;
        }
    }
    if chars.next().is_some() {
        return None;
    }
    Some(u)
}

#[cfg(test)]
mod depth_tests;
#[cfg(test)]
mod point_tests;
#[cfg(test)]
mod tests;
