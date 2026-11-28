//! ADR-0462 value-nesting depth: the fixed cap, the one iterative depth
//! walk and its two front ends, the typed guard, and the checked
//! constructors. Extracted from `value/mod.rs` for the file-size rule (the
//! `ordering.rs`/`json.rs` precedent); the methods stay inherent, so no
//! caller changes.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::{Node, PropertyMap, Relationship, Value};

/// The deepest container nesting a [`Value`] may have (ADR-0462,
/// LABS-606).
///
/// Equal to the parser's static nesting limit (ADR-0257) so every
/// parse-legal literal is accepted, and low enough that the derived
/// recursive `Drop`, clone, and render walks over an at-cap value stay well
/// inside every surface's stack. Every dynamic container construction in
/// the evaluator goes through [`Value::checked_list`]/
/// [`Value::checked_map`]; a `reduce` accumulator can therefore grow one
/// level per iteration without ever exceeding this bound — the level-501
/// construction fails with a typed `ValueDepthExceeded` error instead of
/// building a value that later overflows a recursive walker.
///
/// Fixed constant, no knob: the same house rule as ADR-0257's 4 MiB/500 and
/// ADR-0395's fixed budget (1 000 000 passes since ADR-0670). Lifting it would require a
/// payload-newtype `Drop` for `Value` (never `impl Drop for Value`, which
/// makes the move-out matches E0509), so a recursive drop cannot overflow.
pub const MAX_VALUE_DEPTH: u32 = 500;

/// What the infallible renderers emit for a container nested past
/// [`MAX_VALUE_DEPTH`] instead of recursing (ADR-0462). Unreachable behind
/// the checked constructors: it is the depth safety valve for a caller that
/// bypassed them, never a documented rendering.
pub(crate) const DEPTH_LIMIT_MARKER: &str = "<depth limit exceeded>";

/// The one `ValueDepthExceeded` detail payload (ADR-0462), beside
/// [`MAX_VALUE_DEPTH`]. `what` names the value shape the walker rejected
/// (`"value"`, `"tagged value"`, `"coll.flatten() input"`). Every raiser —
/// [`Value::check_depth`], the tagged decoder and encoder, the collection
/// helpers — spells the message through here so the detail and the limit
/// cannot drift.
pub fn value_depth_exceeded(what: &str) -> String {
    format!(
        "ValueDepthExceeded: {what} nesting exceeds {MAX_VALUE_DEPTH} levels; \
         container nesting compounds per level (security limit)."
    )
}

/// The next container level below `depth`, or `None` once that level would
/// exceed [`MAX_VALUE_DEPTH`]. The canonical ADR-0462 guard for walkers that
/// saturate or throw at the cap (renderers, JSON decoders/encoders, the
/// model bridge, the logical-bytes walk); re-exported so the HTTP Query API
/// encoder spells it the same way. `depth` is the number of containers
/// enclosing the value, so a root is 0.
pub fn next_depth(depth: u32) -> Option<u32> {
    let next = child_level(depth);
    (next <= MAX_VALUE_DEPTH).then_some(next)
}

/// The most worklist steps one depth walk may take before it conservatively
/// refuses (ADR-0462, PR #547 round-5). The walk memoizes container payloads
/// by identity, so a DAG of N distinct payloads costs O(N) steps; this is the
/// belt-and-braces refusal for a sharing pattern the memo cannot collapse —
/// the walk gives up as over-cap instead of hanging. Symmetric with the fixed
/// pass budgets elsewhere (ADR-0395).
const MAX_DEPTH_WALK_STEPS: u64 = 10_000_000;

/// Identity of one container payload during a depth walk: its `Arc`
/// allocation address. Distinct live allocations never share an address, and
/// the walk's root keeps every visited payload alive for the walk's duration,
/// so a key can never alias a freed allocation. The variant kind is
/// redundant: the address already identifies the container, and the memo
/// stores a depth derived from that payload's own contents.
pub(super) type PayloadKey = usize;

/// The one derivation of a payload's identity from its `Arc` (ADR-0462's
/// depth memo and ADR-0691's expansion memo both key through it).
pub(super) fn payload_key<T: ?Sized>(payload: &Arc<T>) -> PayloadKey {
    Arc::as_ptr(payload) as *const () as usize
}

/// One container's lazily enumerated children. Every variant is O(1) to
/// build, so a memo hit drops it without touching the children — the walk
/// stays O(distinct payloads), never O(expanded edges).
enum Children<'a> {
    Values(std::slice::Iter<'a, Value>),
    MapValues(std::collections::btree_map::Values<'a, String, Value>),
    PropValues(PropValues<'a>),
    Path {
        nodes: std::slice::Iter<'a, Node>,
        rels: std::slice::Iter<'a, Relationship>,
        current: Option<PropValues<'a>>,
    },
}

/// `PropertyMap` entry values: the depth walk only needs the value half.
type PropValues<'a> =
    std::iter::Map<std::slice::Iter<'a, (String, Value)>, fn(&'a (String, Value)) -> &'a Value>;

fn entry_value((_, value): &(String, Value)) -> &Value {
    value
}

fn prop_values(props: &PropertyMap) -> PropValues<'_> {
    props.entries().iter().map(entry_value)
}

impl<'a> Iterator for Children<'a> {
    type Item = &'a Value;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Children::Values(items) => items.next(),
            Children::MapValues(values) => values.next(),
            Children::PropValues(values) => values.next(),
            Children::Path {
                nodes,
                rels,
                current,
            } => loop {
                if let Some(values) = current.as_mut() {
                    if let Some(value) = values.next() {
                        return Some(value);
                    }
                    *current = None;
                }
                if let Some(node) = nodes.next() {
                    *current = Some(prop_values(&node.properties));
                    continue;
                }
                if let Some(rel) = rels.next() {
                    *current = Some(prop_values(&rel.properties));
                    continue;
                }
                return None;
            },
        }
    }
}

/// The one owner of the container vocabulary (ADR-0462): a container value's
/// memo identity and its children together, from one enumeration — so the
/// memo key, the child count and the child iteration cannot drift apart. The
/// scalar variants are spelled out on purpose: a future `Value` variant is a
/// non-exhaustive-match compile error at this one classification site.
fn container(value: &Value) -> Option<(PayloadKey, Children<'_>)> {
    match value {
        Value::List(xs) => Some((payload_key(xs), Children::Values(xs.iter()))),
        Value::Map(map) => Some((payload_key(map), Children::MapValues(map.values()))),
        Value::Node(node) => Some((
            payload_key(node),
            Children::PropValues(prop_values(&node.properties)),
        )),
        Value::Relationship(rel) => Some((
            payload_key(rel),
            Children::PropValues(prop_values(&rel.properties)),
        )),
        Value::Path(path) => Some((
            payload_key(path),
            Children::Path {
                nodes: path.nodes.iter(),
                rels: path.rels.iter(),
                current: None,
            },
        )),
        // A packed Vector (ADR-0706) has no child Values — its
        // coordinates are one homogeneous numeric slice — so it walks as
        // a scalar, not a container.
        Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::String(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Uuid(_)
        | Value::Vector { .. }
        | Value::Bytes(_) => None,
    }
}

/// One frame of the iterative depth walk.
enum Frame<'a> {
    /// Visit a value: a scalar completes with depth 0, a container with its
    /// memoized relative depth, or — on its first visit — schedules its
    /// children and the matching [`Frame::Exit`].
    Enter(&'a Value),
    /// A container's children have each pushed exactly one result on top of
    /// `results`; fold those results (from `base`) into the container's
    /// relative depth and memoize it.
    Exit { key: PayloadKey, base: usize },
}

/// The one owner of the level arithmetic (ADR-0462): the level one container
/// deeper than `depth`, saturating at `u32::MAX` — a value that deep is
/// over the cap in every caller. Every walker and guard routes through here
/// so the three previous spellings cannot drift.
fn child_level(depth: u32) -> u32 {
    depth.saturating_add(1)
}

/// Whether a walker at `depth` (containers enclosing the value) may descend
/// into the value's contents — the boolean spelling of [`next_depth`] for
/// the comparison walkers, whose guards read naturally as a match-arm
/// condition. [`next_depth`] owns the boundary; this only answers its
/// `Some`/`None`.
pub(crate) fn descends(depth: u32) -> bool {
    next_depth(depth).is_some()
}

impl Value {
    /// Whether every container in this value nests at most `limit` levels
    /// deep (ADR-0462). Iterative over an explicit worklist — never
    /// recursive — so an over-deep value is rejected without first growing
    /// the stack to its depth.
    ///
    /// A scalar has depth 0; a container's own level is one more than its
    /// parent's, so `[]` is depth 1 and `[[]]` depth 2. Node, relationship
    /// and path values count as containers, and their property values nest
    /// inside them.
    pub fn depth_within(&self, limit: u32) -> bool {
        // One iterative walker, two front ends (ADR-0462): the walk is on
        // capped values, so the exact depth is cheap to compute and there is
        // no separate early-exit spelling to drift.
        self.depth() <= limit
    }

    /// The typed depth guard: `Ok` when this value nests at most
    /// [`MAX_VALUE_DEPTH`] levels (ADR-0462), else a `ValueDepthExceeded`
    /// execution error. The `Kind:` prefix is the token the error channel
    /// classifies (`ErrorKind::ValueDepthExceeded`).
    pub fn check_depth(&self) -> crate::Result<()> {
        Self::check_known_depth(self.depth())
    }

    /// This value's exact container depth (scalar 0, `[]` 1, `[[]]` 2),
    /// walked iteratively once. For a value whose depth is already known,
    /// [`Value::checked_list_with_depth`] avoids this walk.
    ///
    /// The walk memoizes container payloads by identity: a shared subtree is
    /// measured once, so the cost is linear in **distinct payloads**, never in
    /// the expanded node count of a fan-out DAG (a `[acc, acc]` chain would
    /// otherwise expand exponentially). A walk that exceeds
    /// [`MAX_DEPTH_WALK_STEPS`] conservatively answers [`u32::MAX`] (over any
    /// cap), so it can refuse but never hang.
    pub fn depth(&self) -> u32 {
        self.depth_capped().unwrap_or(u32::MAX)
    }

    /// [`Value::depth`] with the ADR-0462 step bound: `None` when the walk
    /// exceeds [`MAX_DEPTH_WALK_STEPS`], which every caller treats as
    /// over-cap.
    fn depth_capped(&self) -> Option<u32> {
        let mut memo: HashMap<PayloadKey, u32> = HashMap::new();
        let mut results: Vec<u32> = Vec::new();
        let mut stack: Vec<Frame<'_>> = vec![Frame::Enter(self)];
        let mut steps: u64 = 0;
        while let Some(frame) = stack.pop() {
            steps += 1;
            if steps > MAX_DEPTH_WALK_STEPS {
                return None;
            }
            match frame {
                Frame::Enter(value) => {
                    let Some((key, children)) = container(value) else {
                        results.push(0);
                        continue;
                    };
                    if let Some(rel) = memo.get(&key) {
                        // Memo hit: the lazily built `children` is dropped
                        // unused, so a shared payload costs O(1).
                        results.push(*rel);
                        continue;
                    }
                    // Record where this container's children's results begin,
                    // then push the children on top of the Exit frame.
                    let base = results.len();
                    stack.push(Frame::Exit { key, base });
                    stack.extend(children.map(Frame::Enter));
                }
                Frame::Exit { key, base } => {
                    // Every child of this container pushed exactly one result
                    // after `base` was recorded; fold them into `1 + max`.
                    let max = results[base..].iter().copied().max().unwrap_or(0);
                    results.truncate(base);
                    let rel = child_level(max);
                    memo.insert(key, rel);
                    results.push(rel);
                }
            }
        }
        results.pop()
    }

    /// The depth check for a caller that already knows the result's depth —
    /// the O(1) path behind [`Value::checked_list_with_depth`]. `depth` is
    /// the candidate value's own container level.
    fn check_known_depth(depth: u32) -> crate::Result<()> {
        if depth <= MAX_VALUE_DEPTH {
            Ok(())
        } else {
            Err(crate::Error::execution(value_depth_exceeded("value")))
        }
    }

    /// Build a list value, rejecting one that would nest deeper than
    /// [`MAX_VALUE_DEPTH`] (ADR-0462). The one constructor for a list the
    /// evaluator builds from dynamic expression results.
    pub fn checked_list(values: Vec<Value>) -> crate::Result<Value> {
        let value = Value::List(values.into());
        value.check_depth()?;
        Ok(value)
    }

    /// **Evaluator-internal O(1) seam** (ADR-0462): build a list when the
    /// caller already knows `child_depth` — the exact maximum container depth
    /// of `values` — instead of walking the subtree. The result's own level
    /// is `child_depth + 1`.
    ///
    /// Invariant: only the evaluator's depth-tracking loops (the
    /// `reduce`/`allReduce` wrap fast path) may call this, passing the depth
    /// they track per iteration. Every other caller must use
    /// [`Value::checked_list`]. Release builds trust the caller and stay
    /// O(1); debug builds assert every element is within the claimed depth.
    #[doc(hidden)]
    pub fn checked_list_with_depth(values: Vec<Value>, child_depth: u32) -> crate::Result<Value> {
        debug_assert!(
            values.iter().all(|v| v.depth_within(child_depth)),
            "checked_list_with_depth: an element is deeper than the claimed child_depth"
        );
        Self::check_known_depth(child_level(child_depth))?;
        Ok(Value::List(values.into()))
    }

    /// Build a map value, rejecting one that would nest deeper than
    /// [`MAX_VALUE_DEPTH`] (ADR-0462). The one constructor for a map the
    /// evaluator builds from dynamic expression results.
    pub fn checked_map(values: BTreeMap<String, Value>) -> crate::Result<Value> {
        let value = Value::Map(values.into());
        value.check_depth()?;
        Ok(value)
    }
}
