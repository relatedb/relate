use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::Arc;

pub use relate_model::value::PointValue;
pub use relate_model::VectorCoordKind;

use crate::temporal::Temporal;
use crate::{Error, NodeId, RelId};

mod depth;
mod json;
mod ordering;
mod props;
mod render;
pub(crate) use depth::descends;
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
    logical_bytes_within_memo(values, limit, &mut ExpansionMemo::off())
}

/// [`logical_bytes_within`] with the container payloads it measures
/// remembered in `memo` (ADR-0691): a payload met again, under another row
/// or wrapped by a fresh container, costs one lookup instead of a re-walk,
/// so the work is linear in the *distinct* payloads. The sizes are keyed by
/// allocation address: every value passed must stay alive while `memo`
/// does (a freed allocation's address may be reused), so a caller scopes
/// one memo to values it holds for the call.
pub fn logical_bytes_within_memo<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    limit: u64,
    memo: &mut ExpansionMemo,
) -> Option<u64> {
    let mut total = 0;
    let over = values
        .into_iter()
        .try_for_each(|v| v.add_logical_bytes(&mut total, limit, 0, memo))
        .is_break();
    (!over).then_some(total)
}

/// The logical size of each container payload a [`logical_bytes_within_memo`]
/// walk finished measuring, keyed by [`depth::payload_key`] (the identity
/// ADR-0462's depth memo uses). A payload's size is its own contents',
/// independent of where it sits, so a hit adds the stored size; the depth
/// guard is unreachable behind the checked constructors, so the memo
/// ignores the level a payload was first met at. `Default` is the
/// remembering memo, like [`ExpansionMemo::new`]; the memo-free form is
/// private.
pub struct ExpansionMemo {
    /// `None` is the memo-free walk [`logical_bytes_within`] runs.
    sizes: Option<std::collections::HashMap<depth::PayloadKey, u64>>,
}

impl Default for ExpansionMemo {
    fn default() -> Self {
        Self::new()
    }
}

impl ExpansionMemo {
    /// A memo that remembers.
    pub fn new() -> Self {
        Self {
            sizes: Some(Default::default()),
        }
    }

    /// The memo [`logical_bytes_within`] walks with: remembers nothing.
    fn off() -> Self {
        Self { sizes: None }
    }

    /// Open the container `payload` met `enclosing` levels deep — the one
    /// place the ADR-0462 depth guard and the memo wrap meet — and add what
    /// it holds to `acc`: its remembered size when known, otherwise what
    /// `body` adds at the next level (remembered once `body` completes; a
    /// walk that broke at the budget measured nothing complete).
    fn open<T>(
        &mut self,
        enclosing: u32,
        payload: &Arc<T>,
        acc: &mut u64,
        budget: u64,
        body: impl FnOnce(&mut Self, &mut u64, u32) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let Some(next) = next_depth(enclosing) else {
            return ControlFlow::Break(());
        };
        let Some(sizes) = self.sizes.as_mut() else {
            return body(self, acc, next);
        };
        let key = depth::payload_key(payload);
        if let Some(size) = sizes.get(&key) {
            return add_bytes(acc, *size, budget);
        }
        let before = *acc;
        body(self, acc, next)?;
        if let Some(sizes) = self.sizes.as_mut() {
            sizes.insert(key, *acc - before);
        }
        ControlFlow::Continue(())
    }
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
    memo: &mut ExpansionMemo,
) -> ControlFlow<()> {
    props.into_iter().try_for_each(|(k, v)| {
        add_str_bytes(k, acc, budget)?;
        v.add_logical_bytes(acc, budget, depth, memo)
    })
}

fn add_node_bytes(
    n: &Node,
    acc: &mut u64,
    budget: u64,
    depth: u32,
    memo: &mut ExpansionMemo,
) -> ControlFlow<()> {
    n.labels
        .iter()
        .try_for_each(|l| add_str_bytes(l, acc, budget))?;
    add_property_bytes(n.properties.iter(), acc, budget, depth, memo)
}

fn add_rel_bytes(
    r: &Relationship,
    acc: &mut u64,
    budget: u64,
    depth: u32,
    memo: &mut ExpansionMemo,
) -> ControlFlow<()> {
    add_str_bytes(&r.rel_type, acc, budget)?;
    add_property_bytes(r.properties.iter(), acc, budget, depth, memo)
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
        let flow = self.add_logical_bytes(&mut total, u64::MAX, 0, &mut ExpansionMemo::off());
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
    fn add_logical_bytes(
        &self,
        acc: &mut u64,
        budget: u64,
        depth: u32,
        memo: &mut ExpansionMemo,
    ) -> ControlFlow<()> {
        match self {
            Value::Null | Value::Bool(_) => add_bytes(acc, 1, budget),
            Value::Int(_) | Value::Float(_) => add_bytes(acc, 8, budget),
            Value::String(s) => add_str_bytes(s, acc, budget),
            Value::Bytes(b) => add_payload_bytes(b, acc, budget),
            Value::List(xs) | Value::Vector { values: xs, .. } => {
                memo.open(depth, xs, acc, budget, |memo, acc, next| {
                    add_bytes(acc, 16, budget)?;
                    xs.iter()
                        .try_for_each(|x| x.add_logical_bytes(acc, budget, next, memo))
                })
            }
            Value::Map(m) => memo.open(depth, m, acc, budget, |memo, acc, next| {
                add_bytes(acc, 16, budget)?;
                add_property_bytes(m.iter(), acc, budget, next, memo)
            }),
            Value::Node(n) => memo.open(depth, n, acc, budget, |memo, acc, next| {
                add_node_bytes(n, acc, budget, next, memo)
            }),
            Value::Relationship(r) => memo.open(depth, r, acc, budget, |memo, acc, next| {
                add_rel_bytes(r, acc, budget, next, memo)
            }),
            Value::Path(p) => memo.open(depth, p, acc, budget, |memo, acc, next| {
                p.nodes
                    .iter()
                    .try_for_each(|n| add_node_bytes(n, acc, budget, next, memo))?;
                p.rels
                    .iter()
                    .try_for_each(|r| add_rel_bytes(r, acc, budget, next, memo))
            }),
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
mod expansion_tests;
#[cfg(test)]
mod point_tests;
#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod test_fixtures;
#[cfg(test)]
mod tests;
