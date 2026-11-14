//! The engine-value → `Pack` conversion family (ADR-0329, ADR-0484): the
//! result-side [`ValueCodec`] (the negotiated `Dialect` — imported
//! downward from the `dialect` leaf beside `version` — plus the session
//! namespace whose segment names element ids), its value recursion with
//! the typed [`MdbPackError`], and the Path hop-sequence validation —
//! split from `packstream` (the `Pack` model and byte codec this family
//! builds on) when the two roles outgrew one file.
//! `Pack::from_mdb` is the one entry every result value converts
//! through.

use crate::dialect::Dialect;
use crate::packstream::{child_depth, Pack, NESTING_LIMIT_EXCEEDED_TEXT};
use crate::value_structs::{point_pack, temporal_pack};

/// The typed failure of the engine-value → Pack conversion: the
/// packstream container-depth limit, or a path-invariant break (an
/// encoder-visible engine bug, never client input), or a temporal/point
/// value with no Bolt 4.4 wire form in its own range. Typed so the
/// protocol layer can distinguish the client-reachable depth limit from
/// the engine contract breaks instead of string-matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MdbPackError {
    /// A container nested past the packstream depth limit (128).
    DepthExceeded,
    /// A `Value::Path` whose node/relationship sequence breaks the
    /// hop invariant — an engine contract break, kept loud.
    PathInvariant(String),
    /// A temporal or point value outside the Bolt 4.4 structure's range
    /// (ADR-0325/0326) — an engine-side value problem, kept loud.
    NotRepresentable(String),
    /// A graph element a 5.x structure must name with an element id, in a
    /// namespace that has none (`system` has no element-id segment,
    /// ADR-0116 §7) — the engine `elementId()` precedent's failure, kept
    /// loud; never a fabricated id (ADR-0484 decision 6).
    ElementIdUnavailable,
}

impl MdbPackError {
    /// The depth-vs-fatal partition (ADR-0110): the packstream
    /// container-depth limit is client-reachable — a deeply nested
    /// parameter value returned in a row or carried in a plan — so it
    /// degrades the session or truncates the plan; every other failure
    /// is an engine contract break, kept loud as the io error. The one
    /// exhaustive classification, beside the error it partitions — the
    /// RECORD write and the plan pair both consult it.
    pub(crate) fn client_reachable(&self) -> bool {
        match self {
            MdbPackError::DepthExceeded => true,
            MdbPackError::PathInvariant(_)
            | MdbPackError::NotRepresentable(_)
            | MdbPackError::ElementIdUnavailable => false,
        }
    }
}

impl std::fmt::Display for MdbPackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MdbPackError::DepthExceeded => f.write_str(NESTING_LIMIT_EXCEEDED_TEXT),
            MdbPackError::PathInvariant(detail) => write!(f, "path invariant broken: {detail}"),
            MdbPackError::NotRepresentable(detail) => f.write_str(detail),
            MdbPackError::ElementIdUnavailable => {
                f.write_str("elements of this namespace have no element ids")
            }
        }
    }
}

impl Pack {
    /// Encode an engine value under the connection's value codec
    /// (ADR-0329, ADR-0484): the RECORD rows' and the ending summary's
    /// plan value both convert through this one entry, so every
    /// version-gated form has a single gate.
    pub(crate) fn from_mdb(v: &relate::Value, codec: ValueCodec) -> Result<Pack, MdbPackError> {
        codec.value(v, 0)
    }
}

/// The connection's value-encoding policy (ADR-0329, ADR-0484): the
/// negotiated dialect — the `patch_bolt` patches (a 4.4-only axis) and
/// the spoken Bolt version — plus the session namespace whose segment
/// names element ids: every axis the engine-value → `Pack` conversion
/// reads. The session builds one per result page from its own state and
/// the result's own namespace stamp; the 4.4 forms never read the
/// namespace axis, the 5.x forms always do.
#[derive(Clone, Copy)]
pub(crate) struct ValueCodec {
    dialect: Dialect,
    namespace: relate::Namespace,
}

impl ValueCodec {
    /// The codec of a connection speaking `dialect`, encoding a result
    /// whose elements live in `namespace`.
    pub(crate) fn new(dialect: Dialect, namespace: relate::Namespace) -> Self {
        Self { dialect, namespace }
    }

    /// The element id of node `id` when the structures carry one: the
    /// engine helper's string against the codec's namespace — the one
    /// source `elementId()` reads (ADR-0116 §7) — or the typed failure
    /// when the namespace has no segment (`system`): the `elementId()`
    /// precedent's text, never a fabricated id (ADR-0484 decision 6).
    /// Callers gate on [`Dialect::element_ids`].
    fn node_element_id(self, id: i64) -> Result<String, MdbPackError> {
        relate::node_element_id(id, self.namespace).ok_or(MdbPackError::ElementIdUnavailable)
    }

    /// The element id of relationship `id` when the structures carry one —
    /// the same contract as [`ValueCodec::node_element_id`].
    fn rel_element_id(self, id: i64) -> Result<String, MdbPackError> {
        relate::rel_element_id(id, self.namespace).ok_or(MdbPackError::ElementIdUnavailable)
    }

    /// The value-to-`Pack` recursion under this codec — the descent
    /// [`Pack::from_mdb`] starts at the root depth 0.
    ///
    /// Mirrors `enc`'s container accounting: `depth` counts enclosing
    /// containers, not scalar leaves, and the root container has depth 1.
    /// Threading the depth through the value recursion keeps a deeply nested
    /// `Value` from overflowing the stack while the `Pack` is being built.
    fn value(&self, v: &relate::Value, depth: usize) -> Result<Pack, MdbPackError> {
        use relate::Value;
        match v {
            Value::Null => Ok(Pack::Null),
            Value::Bool(b) => Ok(Pack::Bool(*b)),
            Value::Int(i) => Ok(Pack::Int(*i)),
            Value::Float(f) => Ok(Pack::Float(*f)),
            Value::String(s) => Ok(Pack::String(String::from(&**s))),
            // ADR-0328: a byte array is PackStream Bytes.
            Value::Bytes(b) => Ok(Pack::Bytes(b.to_vec())),
            // CIP-246: Bolt has no UUID wire type; send the canonical
            // STRING form (parity with Neo4j's driver serialization).
            Value::Uuid(u) => Ok(Pack::String(relate_core::uuid_hyphenated(*u))),
            Value::List(xs) => {
                let next = child_depth_mdb(depth)?;
                let mut out = Vec::with_capacity(xs.len());
                for x in xs.iter() {
                    out.push(self.value(x, next)?);
                }
                Ok(Pack::List(out))
            }
            Value::Map(m) => self.map(m.iter(), depth),
            Value::Node(n) => self.node(n, depth),
            Value::Relationship(r) => self.rel(r, depth),
            Value::Path(p) => self.path(p, depth),
            // ADR-0325/0326: temporal and point structures. A structure is a
            // container on the wire, so it is charged a depth level like
            // `enc` charges it.
            Value::Temporal(t) => {
                child_depth_mdb(depth)?;
                temporal_pack(t, self.dialect).map_err(MdbPackError::NotRepresentable)
            }
            Value::Point(p) => {
                child_depth_mdb(depth)?;
                Ok(point_pack(p))
            }
            Value::Vector { .. } => Ok(Pack::String(v.to_cypher_literal())),
        }
    }

    // Property map shared by the graph struct encoders: same wire form and
    // depth accounting as the `Value::Map` arm, without fabricating a
    // `Value`.
    fn map<'a>(
        &self,
        m: impl IntoIterator<Item = (&'a String, &'a relate::Value)>,
        depth: usize,
    ) -> Result<Pack, MdbPackError> {
        let next = child_depth_mdb(depth)?;
        let mut out = Vec::new();
        for (k, v) in m {
            out.push((Pack::String(k.clone()), self.value(v, next)?));
        }
        Ok(Pack::Map(out))
    }

    // Bolt Node struct 0x4E: [id, labels, properties] at 4.4; 5.x appends
    // the element id (ADR-0484 decision 6).
    fn node(&self, n: &relate::Node, depth: usize) -> Result<Pack, MdbPackError> {
        let next = child_depth_mdb(depth)?;
        let mut fields = vec![
            Pack::Int(n.id),
            Pack::List(n.labels.iter().cloned().map(Pack::String).collect()),
            self.map(n.properties.iter(), next)?,
        ];
        if self.dialect.element_ids() {
            fields.push(Pack::String(self.node_element_id(n.id)?));
        }
        Ok(Pack::Struct { tag: 0x4E, fields })
    }

    // Bolt UnboundRelationship struct 0x72: [id, rel_type, properties] at
    // 4.4; 5.x appends the element id (ADR-0484 decision 6). Start and end
    // are dropped; a Path's sequence re-derives them.
    fn unbound_rel(&self, r: &relate::Relationship, depth: usize) -> Result<Pack, MdbPackError> {
        let next = child_depth_mdb(depth)?;
        let mut fields = vec![
            Pack::Int(r.id),
            Pack::String(r.rel_type.clone()),
            self.map(r.properties.iter(), next)?,
        ];
        if self.dialect.element_ids() {
            fields.push(Pack::String(self.rel_element_id(r.id)?));
        }
        Ok(Pack::Struct { tag: 0x72, fields })
    }

    // Bolt Relationship struct 0x52: [id, start, end, rel_type,
    // properties] at 4.4; 5.x appends element_id, start_node_element_id
    // and end_node_element_id (ADR-0484 decision 6).
    fn rel(&self, r: &relate::Relationship, depth: usize) -> Result<Pack, MdbPackError> {
        let next = child_depth_mdb(depth)?;
        let mut fields = vec![
            Pack::Int(r.id),
            Pack::Int(r.start),
            Pack::Int(r.end),
            Pack::String(r.rel_type.clone()),
            self.map(r.properties.iter(), next)?,
        ];
        if self.dialect.element_ids() {
            fields.push(Pack::String(self.rel_element_id(r.id)?));
            fields.push(Pack::String(self.node_element_id(r.start)?));
            fields.push(Pack::String(self.node_element_id(r.end)?));
        }
        Ok(Pack::Struct { tag: 0x52, fields })
    }

    // ADR-0272: a Path is a struct 0x50 of (nodes, relationships,
    // sequence). Nodes reuse the 0x4E Node encoder; relationships use the
    // 0x72 UnboundRelationship form (start/end are re-derived from the
    // sequence, so they are dropped). The whole structure threads `depth`
    // like the Node/Relationship encoders so nesting stays bounded.
    fn path(&self, p: &relate::Path, depth: usize) -> Result<Pack, MdbPackError> {
        let next = child_depth_mdb(depth)?;
        // The three field lists are real containers on the wire: charge
        // their depth like enc/dec_list so from_mdb and encode share one
        // rule.
        let field = child_depth_mdb(next)?;
        let nodes = p
            .nodes
            .iter()
            .map(|n| self.node(n, field))
            .collect::<Result<Vec<_>, _>>()?;
        let rels = p
            .rels
            .iter()
            .map(|r| self.unbound_rel(r, field))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Pack::Struct {
            tag: 0x50,
            fields: vec![
                Pack::List(nodes),
                Pack::List(rels),
                Pack::List(path_sequence(p)?),
            ],
        })
    }

    /// The plain Bolt 4.4 codec — no patch, the 4.4 structures — the unit
    /// tests' spelling. The namespace axis is unread at 4.4 (element ids
    /// are a 5.x field, ADR-0484 decision 6), so the user-data namespace
    /// fills it.
    #[cfg(test)]
    pub(crate) fn plain_44() -> Self {
        Self::new(Dialect::plain_44(), relate::Namespace::Neo4j)
    }

    /// The 5.4 codec of a session on `namespace` — the spelling the 5.4
    /// tests encode under; no patch applies (patch negotiation is
    /// 4.4-only, ADR-0484 decision 3).
    #[cfg(test)]
    pub(crate) fn at_5_4(namespace: relate::Namespace) -> Self {
        Self::new(Dialect::at_5_4(), namespace)
    }
}

// Validate-and-build the hop sequence. The engine guarantees
// nodes.len() == rels.len() + 1 and that hop i traverses rels[i] between
// nodes[i] and nodes[i+1]. A break on either side — too many
// relationships for the nodes, or nodes left over after the last hop —
// is an encoder-visible bug: fail loudly rather than guess a sign and
// silently emit a wrong-direction or truncated sequence. (A node-less,
// relationship-less path, and a single node with none, are degenerate
// but harmless and stay encodable: both saturate to equality.)
fn path_sequence(p: &relate::Path) -> Result<Vec<Pack>, MdbPackError> {
    if p.rels.len() != p.nodes.len().saturating_sub(1) {
        return Err(MdbPackError::PathInvariant(format!(
            "{} nodes for {} relationships (need nodes == rels + 1)",
            p.nodes.len(),
            p.rels.len()
        )));
    }
    // Two entries per hop: the 1-based relationship index, negative
    // when the hop runs against that relationship's start-to-end
    // direction, then the 0-based index of the node arrived at.
    let mut sequence = Vec::with_capacity(p.rels.len() * 2);
    for (i, r) in p.rels.iter().enumerate() {
        let index = (i + 1) as i64;
        let from = &p.nodes[i];
        let to = &p.nodes[i + 1];
        let signed = if r.start == from.id && r.end == to.id {
            index
        } else if r.start == to.id && r.end == from.id {
            -index
        } else {
            return Err(MdbPackError::PathInvariant(format!(
                "rel {} ({} -> {}) does not connect nodes {} and {}",
                r.id, r.start, r.end, from.id, to.id
            )));
        };
        sequence.push(Pack::Int(signed));
        sequence.push(Pack::Int(index));
    }
    Ok(sequence)
}

// The engine-value family's view of [`child_depth`]: the raw helper stays
// `String` because the encode and decode families share it, so the one
// error the value family can hit is adapted here — at that boundary — into
// its typed `MdbPackError::DepthExceeded` (same `Display` text).
fn child_depth_mdb(depth: usize) -> Result<usize, MdbPackError> {
    child_depth(depth).map_err(|_| MdbPackError::DepthExceeded)
}

#[cfg(test)]
#[path = "value_codec_tests.rs"]
mod tests;
