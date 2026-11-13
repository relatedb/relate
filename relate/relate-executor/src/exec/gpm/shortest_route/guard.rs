//! The route's one guard: the patterns ADR-0186's Decision sends to BFS.

use crate::UsedSet;
use std::collections::{BTreeMap, HashSet};

use relate_core::Value;
use relate_ir::{
    Expr, MatchMode, NodePattern, PathMode, PathPattern, PathSelector, PatternElement, RelPattern,
};

use crate::eval::expr_vars;
use crate::row::Row;
use crate::shortest::followable;

use super::super::selector::resolve_gpm_count;
use super::super::{ElementFilter, GpmFilters};
use crate::pattern_walk::RelFilter;
use crate::shortest::CommonEndNodes;

/// Which shortest-path spelling a routed pattern is: the one fact the
/// element decode cannot read off the lowered pattern. The function and
/// expression forms reject a same-node pair, the keyword selectors keep
/// their closed trails (51N23), and only the expression form stops a fan
/// at its first productive layer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Spelling {
    KeywordSelector,
    Function,
    Expression,
}

impl Spelling {
    /// The `MATCH`-clause spelling: the function forms carry the flag, a
    /// keyword selector does not. The expression form is stamped by its
    /// own entry ([`super::fold_shortest_route`]).
    fn of(path: &PathPattern) -> Self {
        if path.is_shortest_path_function {
            Spelling::Function
        } else {
            Spelling::KeywordSelector
        }
    }

    /// 51N23's runtime policy.
    pub(super) fn common_end_nodes(self) -> CommonEndNodes {
        match self {
            Spelling::KeywordSelector => CommonEndNodes::Allowed,
            Spelling::Function | Spelling::Expression => CommonEndNodes::Forbidden,
        }
    }

    /// The expression form keeps only the global shortest: an origin's
    /// deeper layers are longer than its own shortest, so they cannot win.
    pub(super) fn first_layer_only(self) -> bool {
        self == Spelling::Expression
    }
}

/// The three elements of a routed pattern and its selector kind.
pub(super) struct Shape<'p> {
    pub start: &'p NodePattern,
    pub rel: &'p RelPattern,
    /// The rel's resolved type filter (LABS-801) — the pattern-level
    /// table's entry for this exact element, found in the same pass that
    /// decoded the shape, never re-derived or re-sniffed.
    pub filter: &'p RelFilter,
    pub end: &'p NodePattern,
    /// The var-length bounds: the rel's own `*m..n` (legacy element shape)
    /// or the quantifier's `{m,n}` (the quantified element every var-length
    /// spelling lowers to, LABS-661) — the inner rel is a plain one-hop.
    pub min: u32,
    pub max: Option<u32>,
    /// All-path selector (every shortest trail) rather than single-path.
    pub all: bool,
    /// The shortest-path spelling this pattern was written with.
    pub spelling: Spelling,
}

/// The one guard: the selectors, match and path modes, and pattern shape of
/// ADR-0186's Decision, an empty incoming used-relationship set, and row
/// bindings the search can honor. `None` keeps the pattern on GPM.
///
/// A path-level `WHERE` no longer sends the pattern to GPM: the route
/// applies it *during* the search (see [`super::filters::PathFilter`]), so a
/// shortest path that violates the predicate no longer hides the longer
/// shortest path that satisfies it.
pub(super) fn route_shape<'p>(
    path: &'p PathPattern,
    filters: &'p GpmFilters,
    match_mode: MatchMode,
    used: &UsedSet,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Option<Shape<'p>> {
    if match_mode != MatchMode::DifferentRelationships
        || !used.is_empty()
        || !matches!(path.mode, None | Some(PathMode::Walk | PathMode::Trail))
    {
        return None;
    }
    let all = match path.selector.as_ref()? {
        PathSelector::AnyShortest => false,
        PathSelector::AllShortest => true,
        // The count may arrive as a parameter: the plan cache's
        // auto-parameterization turns a literal `SHORTEST 1` into one, so
        // it resolves through the canonical count helper. A count that
        // does not resolve to 1 (or does not resolve at all) stays on GPM,
        // which owns the selector's count errors.
        PathSelector::Shortest {
            count: Some(count),
            group,
        } if resolve_gpm_count(Some(count), 1, params).is_ok_and(|n| n == 1) => *group,
        PathSelector::Shortest { .. } | PathSelector::Any(_) | PathSelector::All => return None,
    };
    let hop = routed_hop(path, filters, all)?;
    if hop.min > 1
        || hop.rel.where_clause.is_some()
        || !followable(hop.start, hop.rel, hop.end)
        || hop
            .rel
            .variable
            .as_ref()
            .is_some_and(|v| v.with_str(|text| row.contains_key(text)))
    {
        return None;
    }
    // GPM checks the end node with the relationship variable already bound;
    // an endpoint map that reads it (or the path variable) stays on GPM.
    // Text render: the late-binding list compares against the String
    // variable sets of the filter walk.
    let late: Vec<String> = hop
        .rel
        .variable
        .as_ref()
        .map(|v| v.to_string())
        .into_iter()
        .chain(path.path_names().map(|name| name.to_string()))
        .collect();
    let reads_late = |expr: &Expr| {
        let mut vars = HashSet::new();
        expr_vars(expr, &mut vars);
        late.iter().any(|v| vars.contains(v.as_str()))
    };
    // The end's inline `WHERE` is admitted only when it reads the
    // relationship or path variable (LABS-1590): it is then a search
    // predicate, decided on each bound trail like the pattern's own inline
    // `WHERE` (`PathFilter`), since enumerating every trail before the
    // selection is exponential in the hop count. An endpoint-only `WHERE`
    // stays on GPM, and so does any start `WHERE`.
    let endpoint_ok = |np: &NodePattern, where_ok: bool| {
        let mut vars = HashSet::new();
        if let Some(props) = &np.properties {
            expr_vars(props, &mut vars);
        }
        np.where_clause
            .as_ref()
            .is_none_or(|w| where_ok && reads_late(w))
            && np
                .variable
                .as_ref()
                .is_none_or(|v| matches!(row.get_name(v), None | Some(Value::Node(_))))
            && !late.iter().any(|v| vars.contains(v.as_str()))
    };
    (endpoint_ok(hop.start, false) && endpoint_ok(hop.end, true)).then_some(hop)
}

/// The routed var-length hop of a three-element pattern decoded into the
/// route's one [`Shape`] (F-1-10): the outer nodes, the traversed rel, its
/// resolved filter (LABS-801), and the effective bounds — one decoded
/// unit, never a positional tuple the caller reassembles field-for-field.
/// The selector's `all` fact arrives decided — the one fact the element
/// decode cannot read off the pattern. Every var-length spelling lowers to
/// the quantified element (LABS-661) — `-[r:T*m..n]-` arrives as
/// `[Node, Quantified([anon, one-hop rel, anon]), Node]` with the bounds on
/// the quantifier — so that shape decodes first (the shared
/// `implicit_var_length_rel` decoder); the plain var-length `Relationship`
/// element (hand-built or planner-IR-diagnostic plans) remains the legacy
/// arm.
fn routed_hop<'p>(path: &'p PathPattern, filters: &'p GpmFilters, all: bool) -> Option<Shape<'p>> {
    let elements = path.elements.as_slice();
    if elements.len() != 3 {
        return None;
    }
    let (PatternElement::Node(start), PatternElement::Node(end)) = (&elements[0], &elements[2])
    else {
        return None;
    };
    // The rel element's resolved filter sits at its own index in the
    // pattern-level table (LABS-801) — the shape decode and the filter
    // lookup read one position, so they cannot disagree.
    match (&elements[1], &filters.filters[1]) {
        (PatternElement::Quantified(qpp), ElementFilter::Group(inner)) => {
            let rel = qpp.implicit_var_length_rel()?;
            let ElementFilter::Rel(filter) = &inner[1] else {
                return None;
            };
            Some(Shape {
                start,
                rel,
                filter,
                end,
                min: qpp.min,
                max: qpp.max,
                all,
                spelling: Spelling::of(path),
            })
        }
        (PatternElement::Relationship(rel), ElementFilter::Rel(filter)) => {
            if !rel.var_length {
                return None;
            }
            Some(Shape {
                start,
                rel,
                filter,
                end,
                min: rel.min_hops,
                max: rel.max_hops,
                all,
                spelling: Spelling::of(path),
            })
        }
        _ => None,
    }
}
