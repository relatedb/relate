//! The one `Value` text renderer and its two public forms (REL8-217): the
//! round-trippable literal and the CIP-271 `toString()` display rendering
//! are one recursive walk parameterized by a [`RenderMode`] policy. The
//! walk's structure — the ADR-0462 depth budget, the container recursion,
//! the backticking of every name outside the plain identifier shape
//! (labels, types, map keys) — is one policy for both forms; the mode
//! carries only the spellings that genuinely differ. Extracted from
//! `value/mod.rs` beside `depth`/`json`/`ordering`/`props` (the same
//! file-size precedent); the methods stay inherent, so no caller changes.

use std::fmt;

use super::depth::DEPTH_LIMIT_MARKER;
use super::{
    next_depth, uuid_hyphenated, Node, Path, PointValue, PropertyMap, Relationship, Temporal,
    Value, VectorCoordKind,
};
use relate_model::identifier::quote_identifier_shaped;
use relate_model::value::float_text;

impl Value {
    /// The round-trippable literal form — [`RenderMode::Literal`] of the
    /// one [`Value::render`] walk: quoted strings and temporals, the
    /// `uuid('…')` constructor, entity properties included.
    pub fn to_cypher_literal(&self) -> String {
        self.render(RenderMode::Literal, 0)
    }

    /// The CIP-271 `toString()` rendering — [`RenderMode::Display`] of the
    /// one [`Value::render`] walk: the cypher-shell display form rather
    /// than the round-trippable literal form — strings are unquoted,
    /// temporals are bare full-precision ISO strings (ADR-0448), and
    /// maps/lists render recursively with `, ` separators (verified
    /// against Neo4j 2026.09.0).
    pub fn to_display_string(&self) -> String {
        self.render(RenderMode::Display, 0)
    }

    /// The one recursive renderer over every [`Value`] variant, shared by
    /// [`Value::to_cypher_literal`] and [`Value::to_display_string`]
    /// (REL8-217): the walk's structure — the ADR-0462 depth budget, the
    /// container recursion, the backticking of every name outside the
    /// plain identifier shape (labels, types, map keys) — is one policy
    /// for both forms; `mode` carries only the spellings that genuinely
    /// differ. `depth` is the number of containers enclosing `self`, so a
    /// root is 0 and its children 1; a container past
    /// [`super::MAX_VALUE_DEPTH`] renders the depth marker instead of
    /// recursing (the one [`descend`] guard).
    fn render(&self, mode: RenderMode, depth: u32) -> String {
        match self {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => float_text(*f),
            Value::String(s) => mode.string(s),
            Value::Bytes(b) => bytes_as_int_list(b),
            Value::List(xs) => descend(depth, |next| {
                let inner: Vec<_> = xs.iter().map(|v| v.render(mode, next)).collect();
                format!("[{}]", sequence_text(&inner))
            }),
            Value::Map(m) => descend(depth, |next| map_text(m.iter(), mode, next)),
            Value::Node(n) => n.render(mode, depth),
            Value::Relationship(r) => r.render(mode, depth),
            Value::Path(p) => p.render(mode, depth),
            Value::Temporal(t) => mode.temporal(t),
            Value::Uuid(u) => mode.uuid(*u),
            Value::Point(p) => mode.point(p),
            Value::Vector { coord, values } => descend(depth, |next| {
                let inner = values
                    .iter()
                    .map(|value| value.render(mode, next))
                    .collect::<Vec<_>>();
                vector_text(&inner, *coord)
            }),
        }
    }
}

/// The literal form is `Value`'s `Display` (REL8-217): a value interpolated
/// with `{}` renders round-trippable Cypher ([`Value::to_cypher_literal`]),
/// not the CIP-271 `toString()` display text — render
/// [`Value::to_display_string`] explicitly when the display form is the
/// one wanted.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_cypher_literal())
    }
}

/// The text-policy table behind the two [`Value`] renderings (REL8-217):
/// the **scalar** spellings that genuinely differ ([`RenderMode::string`],
/// [`temporal`], [`uuid`], [`point`]) plus the two entity policies —
/// [`RenderMode::entity_text`] (the display is the name chain only,
/// CIP-271; the literal carries the properties through the one
/// [`descend`] guard) and [`RenderMode::labels`] (the label order). The
/// container recursion, the ADR-0462 depth budget and the backticking of
/// every name outside the plain identifier shape are the one
/// [`Value::render`] walk's own policy, shared by both modes; the `<…>`
/// wrapper and traversal chain are [`Path::render`]'s.
#[derive(Copy, Clone)]
enum RenderMode {
    /// The round-trippable scalar form: strings and temporals are quoted
    /// and escaped, a UUID is `uuid('…')` (CIP-246), a point keeps the
    /// decoder-faithful `srid` spelling.
    Literal,
    /// The cypher-shell scalar form (verified against Neo4j
    /// 2026.09.0/2026.10.1): strings are unquoted, temporals are bare
    /// full-precision ISO strings (ADR-0448), a UUID is bare and
    /// hyphenated, a point names the CRS when the pair is canonical.
    Display,
}

impl RenderMode {
    /// A string: `'x y'` quoted and escaped in the literal, bare in the
    /// display (Neo4j 2026.09.0 renders `toString({a:'x y'})` as
    /// `{a: x y}`).
    fn string(self, s: &str) -> String {
        match self {
            RenderMode::Literal => format!("'{}'", escape_cypher_string(s)),
            RenderMode::Display => String::from(s),
        }
    }

    /// A temporal: a quoted ISO literal in the round-trippable form, the
    /// bare full-precision spelling (ADR-0448) in the display.
    fn temporal(self, t: &Temporal) -> String {
        match self {
            RenderMode::Literal => format!("'{}'", escape_cypher_string(&t.iso_string())),
            RenderMode::Display => t.full_seconds_string(),
        }
    }

    /// A UUID: the round-trippable `uuid('…')` value constructor
    /// (CIP-246, like VECTOR) or the bare hyphenated string (verified on
    /// 2026.09.0).
    fn uuid(self, u: u128) -> String {
        match self {
            RenderMode::Literal => format!("uuid('{}')", uuid_hyphenated(u)),
            RenderMode::Display => uuid_hyphenated(u),
        }
    }

    /// A point: both spellings render coordinates through the engine's
    /// one float policy (an integral coordinate keeps its `.0`); the
    /// literal keeps the decoder-faithful `srid` spelling, the display
    /// names the CRS when the srid/dimensionality pair is canonical.
    fn point(self, p: &PointValue) -> String {
        match self {
            RenderMode::Literal => point_literal(p),
            RenderMode::Display => point_display(p),
        }
    }

    /// The entity label chain — the mode's label-order policy: the
    /// literal keeps the stored order; the display sorts (CIP-271: Neo4j
    /// 2026.09.0 renders a multi-label node `(:A:B:C)` whatever the
    /// creation order). The `:name` spelling itself is the shared
    /// [`colon_names`] policy, not duplicated here.
    fn labels(self, labels: &[String]) -> String {
        match self {
            RenderMode::Literal => colon_names(labels.iter().map(String::as_str)),
            RenderMode::Display => {
                let mut sorted: Vec<&str> = labels.iter().map(String::as_str).collect();
                sorted.sort_unstable();
                colon_names(sorted)
            }
        }
    }

    /// An entity's text from its bracket-open part, name chain and
    /// bracket close — the one spelling of the mode's entity policy, so
    /// no entity renderer repeats the conditional (REL8-217): the
    /// display is the name chain only (CIP-271 — no properties, so it
    /// never recurses and spends no ADR-0462 budget); the literal
    /// carries the property map and charges the entity's own container
    /// level through the one [`descend`] guard — an entity past the cap
    /// saturates whole, to the bare depth marker, like every other
    /// container (never a spliced `(:X<marker>)`), and the guard sits
    /// before the emptiness test exactly where the pre-unification
    /// renderer put it. `depth` is the number of containers enclosing
    /// the entity.
    fn entity_text(
        self,
        open: &str,
        name: &str,
        close: &str,
        props: &PropertyMap,
        depth: u32,
    ) -> String {
        match self {
            RenderMode::Literal => descend(depth, |next| {
                let props = if props.is_empty() {
                    String::new()
                } else {
                    format!(" {}", map_text(props.iter(), RenderMode::Literal, next))
                };
                format!("{open}{name}{props}{close}")
            }),
            RenderMode::Display => format!("{open}{name}{close}"),
        }
    }
}

/// The render walks' one ADR-0462 recursion guard: the body of one
/// container level — `f` renders at the level `next_depth` admits — or
/// [`DEPTH_LIMIT_MARKER`] once that level would exceed
/// [`super::MAX_VALUE_DEPTH`]. Every container arm of the one renderer
/// and the entity literal form ([`RenderMode::entity_text`]) routes
/// through here, so the guard and
/// its marker have one emission site. The path's literal arm also routes
/// its boundary charge through here but renders its elements at the
/// path's own `depth` (the walk adds no level for embedded entities) —
/// its admitted level is inert by design; see [`Path::render`].
fn descend(depth: u32, f: impl FnOnce(u32) -> String) -> String {
    match next_depth(depth) {
        Some(next) => f(next),
        None => DEPTH_LIMIT_MARKER.into(),
    }
}

/// The coordinate text shared by both point forms — `x`, `y` and, on a 3D
/// point, `z`, each through the engine's one float policy (an integral
/// coordinate keeps its `.0`). The one owner of the axis vocabulary; the
/// two forms add only their own srid/crs spelling.
fn point_coords(p: &PointValue) -> (String, String, Option<String>) {
    (float_text(p.x), float_text(p.y), p.z.map(float_text))
}

/// The literal form of a point: the decoder-faithful `srid` spelling with
/// coordinates through [`point_coords`] — the round-trippable twin of
/// [`point_display`] (REL8-217). [`PointValue`]'s own `Display` renders
/// bare `{}` floats and belongs to the relate-graph-examples diff
/// normalizer; the literal owes the round-trip the `.0` spelling.
fn point_literal(p: &PointValue) -> String {
    let (x, y, z) = point_coords(p);
    match z {
        Some(z) => format!("point({{srid: {}, x: {x}, y: {y}, z: {z}}})", p.srid),
        None => format!("point({{srid: {}, x: {x}, y: {y}}})", p.srid),
    }
}

/// The Neo4j text form of a point (the CIP-271 display policy `toString()`
/// and string concatenation use): `point({x: 1.0, y: 2.0, crs:
/// 'cartesian'})`. Coordinates render through [`point_coords`] (so an
/// integral coordinate keeps its `.0`), a 3D point adds `z`, and the CRS
/// name comes from [`PointValue::crs_name`]. A srid/dimensionality pair
/// outside the four `point()` codes — only reachable from a decoder,
/// never from `point()` — renders the [`point_literal`] `srid` spelling
/// rather than claiming a CRS the value does not have: one owner for that
/// spelling, the display's fallback is the literal.
fn point_display(p: &PointValue) -> String {
    let Some(crs) = p.crs_name() else {
        return point_literal(p);
    };
    let (x, y, z) = point_coords(p);
    match z {
        Some(z) => format!("point({{x: {x}, y: {y}, z: {z}, crs: '{crs}'}})"),
        None => format!("point({{x: {x}, y: {y}, crs: '{crs}'}})"),
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
        sequence_text(elements),
        elements.len(),
        coord.canonical_name()
    )
}

/// The `:name` chain — one owner for the colon-prefixed
/// quoted-identifier spelling, shared by the node label chain and the
/// single relationship type: a name outside the plain identifier shape
/// is backticked (CIP-271) — the literal must quote too or
/// `(:special label)` does not re-parse. Order is the caller's policy:
/// the literal keeps the stored order, the display sorts.
fn colon_names<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    names
        .into_iter()
        .map(|n| format!(":{}", quote_identifier_shaped(n)))
        .collect()
}

impl Node {
    /// The one node renderer shared by both [`Value`] forms (REL8-217):
    /// the label chain is the mode's own policy ([`RenderMode::labels`]
    /// — stored order in the literal, sorted in the display) and the
    /// whole entity frame is [`RenderMode::entity_text`]'s (properties in
    /// the literal, name chain only in the display).
    fn render(&self, mode: RenderMode, depth: u32) -> String {
        mode.entity_text(
            "(",
            &mode.labels(&self.labels),
            ")",
            &self.properties,
            depth,
        )
    }
}

impl Relationship {
    /// The one relationship renderer shared by both [`Value`] forms
    /// (REL8-217): the type spells through the one [`colon_names`] owner
    /// (backticked when outside the plain identifier shape in both
    /// forms), and the whole entity frame is [`RenderMode::entity_text`]'s
    /// (properties in the literal, type only in the display, CIP-271).
    fn render(&self, mode: RenderMode, depth: u32) -> String {
        let rel_type = colon_names(std::iter::once(self.rel_type.as_str()));
        mode.entity_text("[", &rel_type, "]", &self.properties, depth)
    }
}

impl Path {
    /// The one path renderer shared by both [`Value`] forms (REL8-217).
    /// The literal is the `<…>` chain with entity properties: the path
    /// itself is the container, so its one level is charged at the guard
    /// — routed through the one [`descend`] site — while its embedded
    /// entities are not an extra level (the walk's container accounting,
    /// `depth_within` ADR-0462, adds no level for them), so the admitted
    /// level is inert and both arms render the elements at `depth`; the
    /// four-walker agreement at that boundary is pinned by
    /// `depth_tests::path_walkers_agree_on_the_depth_boundary`. The
    /// display is the bare traversal chain with no properties (Neo4j
    /// 2026.10.1 renders `toString(path)` as
    /// `(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C)`); it never recurses, so it
    /// carries no guard — the same unguarded display shape a standalone
    /// node or relationship gets.
    fn render(&self, mode: RenderMode, depth: u32) -> String {
        match mode {
            RenderMode::Literal => descend(depth, |_next| {
                format!("<{}>", self.walk(RenderMode::Literal, depth))
            }),
            RenderMode::Display => self.walk(RenderMode::Display, depth),
        }
    }

    /// The path's element chain — one node then, if the path continues past
    /// it, the relationship traversed from it, with the arrows of the
    /// direction actually stored. `mode`/`depth` are the values the
    /// element renderers take, so the literal and display walks share the
    /// direction policy and the depth model.
    fn walk(&self, mode: RenderMode, depth: u32) -> String {
        let mut s = String::new();
        for (i, n) in self.nodes.iter().enumerate() {
            s.push_str(&n.render(mode, depth));
            if let Some(r) = self.rels.get(i) {
                // `rels[i]` connects `nodes[i]` and `nodes[i+1]`, so the
                // arrow is the one bit that is always defined: which
                // endpoint is the current node. A self-loop is outgoing.
                let (left, right) = if r.start == n.id {
                    ("-", "->")
                } else {
                    ("<-", "-")
                };
                s.push_str(&format!("{left}{}{right}", r.render(mode, depth)));
            }
        }
        s
    }
}

/// The one `a, b, c` element separator for every rendered sequence —
/// lists, map bodies, byte lists and vector coordinates (one owner for
/// the separator policy).
fn sequence_text(elements: &[String]) -> String {
    elements.join(", ")
}

/// Cypher has no byte-array literal: a [`Value::Bytes`] renders as the list
/// of its byte values 0-255 (`[0, 1, 255]`), the same shape it crosses the
/// `value_to_cypher` bridge in (ADR-0327).
fn bytes_as_int_list(bytes: &[u8]) -> String {
    let inner: Vec<_> = bytes.iter().map(u8::to_string).collect();
    format!("[{}]", sequence_text(&inner))
}

fn escape_cypher_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The `{k: v, …}` text shared by the [`Value::Map`] arm and the entity
/// property suffix (REL8-217): one key-quoting policy for both render
/// forms — a key outside the plain identifier shape is backticked
/// (CIP-271: `{`123`: digit key, `a.b`: dot, `first name`: Alice}`),
/// because the literal must quote too or `{first name: …}` does not
/// re-parse. The map's own iteration order is already the sorted one;
/// `depth` is the values' enclosing-container count.
fn map_text<'a>(
    m: impl IntoIterator<Item = (&'a String, &'a Value)>,
    mode: RenderMode,
    depth: u32,
) -> String {
    let inner: Vec<_> = m
        .into_iter()
        .map(|(k, v)| format!("{}: {}", quote_identifier_shaped(k), v.render(mode, depth)))
        .collect();
    format!("{{{}}}", sequence_text(&inner))
}
