//! The bridge's one iterative walk (ADR-0462's data-value cap, ADR-0628's
//! plan-wire budget): the explicit heap work stack of [`Frame`]s building
//! the `CypherValue` bottom-up, constant caller-stack depth however deep
//! the value nests — the rust round's remedy, so a value's depth costs
//! memory, never stack. Both crossings ride this ONE walk, so the
//! Value→CypherValue mapping is single-sourced in its one exhaustive
//! match: a new `Value` variant lands in both crossings by construction,
//! never in a second copy that can drift.

use relate_model::{CypherValue, IndexMap, VectorCoordKind};

use crate::convert::temporal_to_cypher;
use crate::value::{next_depth, Node, Relationship, Value};
use crate::Error;

/// The deepest plan-wire nesting the crossing accepts (ADR-0628's
/// stack-safe budget, the security round's final shape): 1 000 container
/// levels — about 500 chained clauses (a plan node costs its map plus the
/// children list around the next, ~2 levels per clause), ~2× headroom
/// over ADR-0628's own 251-clause repro (~502 levels). Past it a plan
/// gets the typed refusal below — rare, loud, explained.
///
/// The closure, each layer named: the **probe** bounds the builds — every
/// plan-crossing boundary runs `PlanNode::ensure_plan_wire_budget`, an
/// iterative depth probe over the typed tree, before any recursive
/// producer build (`to_value`/`to_neo4j_value`) runs on the caller's
/// thread, so those builds only ever see a tree within the budget; the
/// **iterative crossing** bounds the conversion — the walk below holds
/// constant stack however deep the value nests, so a value's depth costs
/// memory, never stack; the **budget** bounds the remaining recursive
/// walks of the crossed value, which still run per level on the calling
/// surface's thread — the crossed value's recursive drop (~1
/// frame/level) and the document builds (`plan_document`/`node_json`,
/// `v2_plan_node`, the plan table's layout: 1–3 frames/level), ~600
/// B/level at the worst walk; and the **typed tree's own drop** is an
/// iterative drain (`impl Drop for PlanNode`) — the tree is built on the
/// engine's big-stack worker but dropped wherever the caller's
/// `QueryResult` drops, so the drain makes that stack-flat at any depth.
/// At the 1 000-level budget every walk stays under ~600 KB, inside the
/// smallest caller-provided stack — an FFI host thread: Java and .NET
/// default to ~1 MiB — with margin. (The earlier 10 000-level budget was
/// sized against the engine's 8 MiB stacks and its own arithmetic was
/// off: the crossing then recursed ~2 frames/level, so the budget
/// admitted ~20 000 frames ≈ 6 MB of walk, more than a 1 MiB caller
/// thread holds.)
pub const MAX_PLAN_WIRE_DEPTH: u32 = 1_000;

/// The depth policy one bridge crossing runs under: the ADR-0462
/// data-value cap, or ADR-0628's plan-wire budget. Both crossings ride
/// the ONE iterative walk ([`value_to_cypher_walk`]) — the policy chooses
/// only what a container past its bound does — so the Value→CypherValue
/// mapping is single-sourced in the walk's one exhaustive match: a new
/// `Value` variant lands in both crossings by construction, never in a
/// second copy that can drift.
#[derive(Clone, Copy)]
pub(crate) enum DepthPolicy {
    /// The data-value crossing: a container past [`crate::MAX_VALUE_DEPTH`]
    /// crosses as `CypherValue::Null` instead of opening — the ADR-0462
    /// safety valve, unreachable behind the checked constructors (the
    /// bridge has no error channel of its own).
    Data,
    /// The plan-wire crossing: a container past [`MAX_PLAN_WIRE_DEPTH`]
    /// fails with the typed budget error (ADR-0628).
    Plan,
}

/// What opening a container `enclosing` levels deep does under a
/// [`DepthPolicy`]: opens, or names the level's refusal — ADR-0462's
/// saturation (cross the container as `Null`) for data values, ADR-0628's
/// typed over-budget error for the plan wire, carrying the refusing level
/// (a lower bound of the value's true depth).
enum Level {
    Deeper,
    Saturates,
    OverBudget(u32),
}

impl DepthPolicy {
    /// Whether a container `enclosing` levels deep may open, and its
    /// refusal when it may not — the one boundary both crossings decide
    /// here: ADR-0462's data cap through [`next_depth`], ADR-0628's plan
    /// budget against [`MAX_PLAN_WIRE_DEPTH`].
    fn admit(self, enclosing: u32) -> Level {
        match self {
            DepthPolicy::Data => match next_depth(enclosing) {
                Some(_) => Level::Deeper,
                None => Level::Saturates,
            },
            DepthPolicy::Plan => {
                if enclosing < MAX_PLAN_WIRE_DEPTH {
                    Level::Deeper
                } else {
                    Level::OverBudget(enclosing + 1)
                }
            }
        }
    }
}

/// ADR-0628's typed plan-budget refusal at container level `at` (a lower
/// bound of the value's true depth) — the one spelling every
/// [`Level::OverBudget`] arm of the walk raises.
fn plan_budget_exceeded(at: u32) -> Error {
    Error::capacity_plan_wire(u64::from(at), u64::from(MAX_PLAN_WIRE_DEPTH))
}

/// The next value the walk converts: a `Value` child of an open frame, or
/// a typed path element (converted directly, without the clone-wrap a
/// trip through `Value` would add — the recursive walk's own precedent).
enum Input<'a> {
    Value(&'a Value),
    Node(&'a Node),
    Rel(&'a Relationship),
}

/// The path element kind a [`Frame::Path`] is currently converting — the
/// routing fact for placing its completed value.
enum PathElement {
    Node,
    Rel,
}

/// One open container of the walk's heap work stack: the children still
/// to convert, the completed children so far, and the policy depth the
/// children convert at (`child_depth` — the container's own level). When
/// the children are exhausted the frame closes into its `CypherValue` and
/// is delivered to its parent — the bottom-up half of the iterative
/// crossing.
enum Frame<'a> {
    List {
        items: std::slice::Iter<'a, Value>,
        child_depth: u32,
        out: Vec<CypherValue>,
    },
    Map {
        entries: std::collections::btree_map::Iter<'a, String, Value>,
        /// The key of the child being converted, stashed when it was
        /// handed out and consumed when its value is placed.
        pending: Option<String>,
        child_depth: u32,
        /// The entries in their final form — `place` inserts, `close`
        /// moves the map out, so the frame never re-collects (the
        /// per-row hot path's one allocation, sized up front).
        out: IndexMap<String, CypherValue>,
    },
    Vector {
        coord: VectorCoordKind,
        values: std::slice::Iter<'a, Value>,
        child_depth: u32,
        out: Vec<CypherValue>,
    },
    Node {
        node: &'a Node,
        entries: std::slice::Iter<'a, (String, Value)>,
        pending: Option<String>,
        child_depth: u32,
        /// The properties in their final form — inserted as each value
        /// places, moved out at close, never re-collected.
        out: IndexMap<String, CypherValue>,
    },
    Rel {
        rel: &'a Relationship,
        entries: std::slice::Iter<'a, (String, Value)>,
        pending: Option<String>,
        child_depth: u32,
        /// The properties in their final form — inserted as each value
        /// places, moved out at close, never re-collected.
        out: IndexMap<String, CypherValue>,
    },
    Path {
        nodes: std::slice::Iter<'a, Node>,
        rels: std::slice::Iter<'a, Relationship>,
        /// Which element kind the child being converted is, stashed when
        /// it was handed out and consumed when its value is placed.
        pending: Option<PathElement>,
        child_depth: u32,
        nodes_out: Vec<CypherValue>,
        rels_out: Vec<CypherValue>,
    },
}

impl<'a> Frame<'a> {
    /// The policy depth this frame's children convert at (its own level).
    fn child_depth(&self) -> u32 {
        match self {
            Frame::List { child_depth, .. }
            | Frame::Map { child_depth, .. }
            | Frame::Vector { child_depth, .. }
            | Frame::Node { child_depth, .. }
            | Frame::Rel { child_depth, .. }
            | Frame::Path { child_depth, .. } => *child_depth,
        }
    }

    /// The frame's next child to convert, or `None` when they are
    /// exhausted. The map-shaped frames and the path frame stash the
    /// placement fact (the entry key, the element kind) for the value's
    /// [`Frame::place`].
    fn next_child(&mut self) -> Option<Input<'a>> {
        match self {
            Frame::List { items, .. } => items.next().map(Input::Value),
            Frame::Vector { values, .. } => values.next().map(Input::Value),
            Frame::Map {
                entries, pending, ..
            } => {
                let (key, value) = entries.next()?;
                *pending = Some(key.clone());
                Some(Input::Value(value))
            }
            Frame::Node {
                entries, pending, ..
            }
            | Frame::Rel {
                entries, pending, ..
            } => {
                let (key, value) = entries.next()?;
                *pending = Some(key.clone());
                Some(Input::Value(value))
            }
            Frame::Path {
                nodes,
                rels,
                pending,
                ..
            } => {
                if let Some(node) = nodes.next() {
                    *pending = Some(PathElement::Node);
                    Some(Input::Node(node))
                } else if let Some(rel) = rels.next() {
                    *pending = Some(PathElement::Rel);
                    Some(Input::Rel(rel))
                } else {
                    None
                }
            }
        }
    }

    /// Place a completed child value into the frame — the map-shaped
    /// frames pair it with the stashed key, the path frame routes it by
    /// the stashed element kind.
    fn place(&mut self, done: CypherValue) {
        match self {
            Frame::List { out, .. } | Frame::Vector { out, .. } => out.push(done),
            Frame::Map { pending, out, .. }
            | Frame::Node { pending, out, .. }
            | Frame::Rel { pending, out, .. } => {
                let key = pending
                    .take()
                    .expect("a placed value follows the child its key was stashed for");
                out.insert(key, done);
            }
            Frame::Path {
                pending,
                nodes_out,
                rels_out,
                ..
            } => match pending
                .take()
                .expect("a placed value follows the element its kind was stashed for")
            {
                PathElement::Node => nodes_out.push(done),
                PathElement::Rel => rels_out.push(done),
            },
        }
    }

    /// Close the frame into its `CypherValue` — the container shapes of
    /// the one mapping, assembled from the completed children.
    fn close(self) -> CypherValue {
        match self {
            Frame::List { out, .. } => CypherValue::List(out),
            Frame::Map { out, .. } => CypherValue::Map(out),
            Frame::Vector { coord, out, .. } => CypherValue::Vector {
                coord,
                dim: out.len(),
                values: out,
            },
            Frame::Node { node, out, .. } => CypherValue::Node {
                id: node.id,
                labels: (*node.labels).clone(),
                properties: out,
            },
            Frame::Rel { rel, out, .. } => CypherValue::Relationship {
                id: rel.id,
                start_id: rel.start,
                end_id: rel.end,
                rel_type: rel.rel_type.clone(),
                properties: out,
            },
            Frame::Path {
                nodes_out,
                rels_out,
                ..
            } => CypherValue::Path {
                nodes: nodes_out,
                relationships: rels_out,
            },
        }
    }
}

/// The one walk behind both crossings (ADR-0462's saturation, ADR-0628's
/// budget), **iterative** over an explicit heap work stack of [`Frame`]s
/// building the `CypherValue` bottom-up: the crossing holds constant
/// caller-stack depth however deep the value nests, so a value's depth
/// costs memory, never stack (the rust round's remedy). The one
/// exhaustive match below — scalars completed in place, containers
/// opening frames, frames closing into their parents — is the whole
/// Value→CypherValue mapping, shared by both policies.
pub(crate) fn value_to_cypher_walk(
    root: &Value,
    policy: DepthPolicy,
) -> crate::Result<CypherValue> {
    /// Deliver a completed value: into its parent frame, or — when the
    /// stack is empty — the walk's finished result.
    fn deliver(stack: &mut Vec<Frame<'_>>, done: CypherValue) -> Option<CypherValue> {
        match stack.last_mut() {
            Some(parent) => {
                parent.place(done);
                None
            }
            None => Some(done),
        }
    }

    /// Admit a container `enclosing` levels deep and open its frame — or
    /// complete the admission's refusal — the one admission spelling
    /// every container arm of the walk shares. `open` builds the admitted
    /// frame; its `child_depth` is the caller's to spell (one deeper than
    /// `enclosing`, except a path's elements, which share the path's
    /// level).
    fn admit_or<'a>(
        stack: &mut Vec<Frame<'a>>,
        policy: DepthPolicy,
        enclosing: u32,
        open: impl FnOnce() -> Frame<'a>,
    ) -> crate::Result<()> {
        match policy.admit(enclosing) {
            Level::Deeper => stack.push(open()),
            // ADR-0462's saturation valve, unreachable behind the checked
            // constructors. Both policies admit the root (enclosing 0),
            // so a refused container always has a parent frame to receive
            // the Null.
            Level::Saturates => stack
                .last_mut()
                .expect("a refused container always has a parent frame")
                .place(CypherValue::Null),
            Level::OverBudget(at) => return Err(plan_budget_exceeded(at)),
        }
        Ok(())
    }

    let mut stack: Vec<Frame<'_>> = Vec::new();
    // The input pending from outside the frames: the root (every later
    // step draws from the top frame's own child iterators).
    let mut current: Option<Input<'_>> = Some(Input::Value(root));
    loop {
        let input = current
            .take()
            .or_else(|| stack.last_mut().and_then(Frame::next_child));
        let Some(input) = input else {
            // No pending input and the top frame's children are exhausted:
            // close it into its parent — or, at the root, finish.
            let frame = stack
                .pop()
                .expect("the walk always has an input or an open frame");
            let closed = frame.close();
            if let Some(finished) = deliver(&mut stack, closed) {
                return Ok(finished);
            }
            continue;
        };
        // The enclosing-container count of the incoming value: the parent
        // frame's child depth (its own level), zero at the root.
        let enclosing = stack.last().map_or(0, Frame::child_depth);
        // The input's outcome: a completed value (every scalar arm — the
        // one mapping), or nothing when a container opened a frame.
        let done: Option<CypherValue> = match input {
            Input::Value(Value::Null) => Some(CypherValue::Null),
            Input::Value(Value::Bool(b)) => Some(CypherValue::Bool(*b)),
            Input::Value(Value::Int(i)) => Some(CypherValue::Int(*i)),
            Input::Value(Value::Float(f)) => Some(CypherValue::Float(*f)),
            Input::Value(Value::String(s)) => Some(CypherValue::Str(String::from(&**s))),
            // CIP-246 documented deviation: the model-level CypherValue
            // (FFI / Python surface) has no UUID variant, so UUIDs cross
            // as their canonical STRING form.
            Input::Value(Value::Uuid(u)) => {
                Some(CypherValue::Str(crate::value::uuid_hyphenated(*u)))
            }
            // ADR-0327 documented deviation: CypherValue has no byte-array
            // variant, so bytes cross as the LIST<INTEGER> of their byte
            // values 0-255 — the representation ADR-0219 gives Python
            // `bytes` parameters. Only the Bolt surface carries real
            // bytes.
            Input::Value(Value::Bytes(b)) => Some(CypherValue::List(
                b.iter().map(|&x| CypherValue::Int(x.into())).collect(),
            )),
            Input::Value(Value::Point(p)) => Some(CypherValue::Point(p.clone())),
            Input::Value(Value::Temporal(t)) => Some(temporal_to_cypher(t)),
            Input::Value(Value::List(xs)) => {
                admit_or(&mut stack, policy, enclosing, || Frame::List {
                    items: xs.iter(),
                    child_depth: enclosing + 1,
                    out: Vec::with_capacity(xs.len()),
                })?;
                None
            }
            Input::Value(Value::Map(m)) => {
                admit_or(&mut stack, policy, enclosing, || Frame::Map {
                    entries: m.iter(),
                    pending: None,
                    child_depth: enclosing + 1,
                    out: IndexMap::with_capacity(m.len()),
                })?;
                None
            }
            Input::Value(Value::Vector { coord, values }) => {
                admit_or(&mut stack, policy, enclosing, || Frame::Vector {
                    coord: *coord,
                    values: values.iter(),
                    child_depth: enclosing + 1,
                    out: Vec::with_capacity(values.len()),
                })?;
                None
            }
            Input::Value(Value::Node(n)) => {
                admit_or(&mut stack, policy, enclosing, || Frame::Node {
                    node: n,
                    entries: n.properties.entries().iter(),
                    pending: None,
                    child_depth: enclosing + 1,
                    out: IndexMap::with_capacity(n.properties.entries().len()),
                })?;
                None
            }
            Input::Value(Value::Relationship(r)) => {
                admit_or(&mut stack, policy, enclosing, || Frame::Rel {
                    rel: r,
                    entries: r.properties.entries().iter(),
                    pending: None,
                    child_depth: enclosing + 1,
                    out: IndexMap::with_capacity(r.properties.entries().len()),
                })?;
                None
            }
            Input::Value(Value::Path(p)) => {
                // A path's embedded entities are not an extra container
                // level (ADR-0462 depth accounting): they share the
                // path's own level, so the frame's children convert at
                // the path's enclosing count — the one frame whose child
                // depth is not one deeper.
                admit_or(&mut stack, policy, enclosing, || Frame::Path {
                    nodes: p.nodes.iter(),
                    rels: p.rels.iter(),
                    pending: None,
                    child_depth: enclosing,
                    nodes_out: Vec::new(),
                    rels_out: Vec::new(),
                })?;
                None
            }
            // A typed path element, drawn from a path frame: it converts
            // at the path's shared level and opens its own entity frame.
            Input::Node(n) => {
                admit_or(&mut stack, policy, enclosing, || Frame::Node {
                    node: n,
                    entries: n.properties.entries().iter(),
                    pending: None,
                    child_depth: enclosing + 1,
                    out: IndexMap::with_capacity(n.properties.entries().len()),
                })?;
                None
            }
            Input::Rel(r) => {
                admit_or(&mut stack, policy, enclosing, || Frame::Rel {
                    rel: r,
                    entries: r.properties.entries().iter(),
                    pending: None,
                    child_depth: enclosing + 1,
                    out: IndexMap::with_capacity(r.properties.entries().len()),
                })?;
                None
            }
        };
        if let Some(done) = done {
            if let Some(finished) = deliver(&mut stack, done) {
                return Ok(finished);
            }
        }
    }
}
