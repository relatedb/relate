//! A `RETURN`/`WITH` projection's classification — the decided shape and
//! the per-clause plan every projection path consults. Split out of
//! `project.rs` so the merge paths, the post-merge tail and the
//! dispatcher's fold slots all read it *downward*: the classification is
//! read by `project` and by `post_merge` — which owns the landing
//! container and the tail — and reads neither of them back.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};

use crate::eval::is_agg_expr;
use crate::overlay::ExecGraph;
use crate::row::Row;
use relate_ir::{Expr, Projection};

use super::aggregate::{GroupSubclauses, WhereMode};
use super::order::{resolve_page_bounds, window_capacity, OrderKeys, PageBounds};
use super::scope::{has_star_item, is_star_item, ProjectionFields};

/// Which merge path owns a `RETURN`/`WITH` projection — the three
/// exclusive shapes, decided once at classification so no consumer
/// re-derives the combination from independent flags:
/// [`ProjectionShape::Plain`] (each row projects alone),
/// [`ProjectionShape::Star`] (a `*` item, nothing aggregating: the
/// visible-bindings merge) and [`ProjectionShape::Aggregate`] (aggregate
/// items, an explicit GROUP BY, or a DISTINCT with an aggregating
/// subclause).
#[derive(Clone, Copy)]
pub(crate) enum ProjectionShape {
    Plain,
    Star,
    Aggregate {
        /// A `*` item among the items of the aggregating shape: the group
        /// keys are every carried variable (ADR-0198 ruling 1), so a feed
        /// cannot fold into it as rows arrive
        /// ([`ProjectionShape::streams`]).
        star: bool,
    },
}

impl ProjectionShape {
    /// Decide the shape from the projection's fields — the one decision
    /// point every consumer reads. The one rejection the executor owns
    /// lives here: a `DISTINCT *` whose subclause aggregates. Validation
    /// admits the shape (DISTINCT makes the clause aggregating regardless
    /// of `*`), but the executor cannot give it HAVING semantics: the
    /// aggregate would evaluate per row through scalar `eval`, whose
    /// error is swallowed (the WHERE form returned zero rows, the ORDER
    /// BY form sorted on null keys), so the combination fails closed.
    pub(crate) fn decide(fields: &ProjectionFields<'_>) -> Result<Self> {
        let aggregating_subclause = fields.subclause_aggregates();
        let star = has_star_item(fields.items);
        let has_agg = fields.items_aggregate()
            || fields.group_by.is_some()
            || (fields.distinct && aggregating_subclause);
        if fields.distinct && star && aggregating_subclause {
            return Err(Error::syntax(
                "InvalidAggregation: a DISTINCT * projection cannot aggregate in its ORDER BY or WHERE subclause",
            ));
        }
        Ok(if has_agg {
            ProjectionShape::Aggregate { star }
        } else if star {
            ProjectionShape::Star
        } else {
            ProjectionShape::Plain
        })
    }

    /// The two facts a shape carries, read from it once: every predicate
    /// below is a reading of these, so no one of them re-derives a
    /// combination another already holds.
    fn facts(self) -> ShapeFacts {
        match self {
            ProjectionShape::Plain => ShapeFacts {
                aggregating: false,
                star: false,
            },
            ProjectionShape::Star => ShapeFacts {
                aggregating: false,
                star: true,
            },
            ProjectionShape::Aggregate { star } => ShapeFacts {
                aggregating: true,
                star,
            },
        }
    }

    /// Whether a feed into this projection folds as rows arrive: an
    /// aggregating shape with no `*` item (a `*` groups by every carried
    /// variable, which needs the rows' shape up front). One decision for
    /// the dispatcher, which chooses the feed, and the projection, which
    /// consumes it.
    pub(crate) fn streams(&self) -> bool {
        let facts = self.facts();
        facts.aggregating && !facts.star
    }

    /// Whether the shape folds an aggregation — the fact the ordered
    /// window's decision reads ([`ProjectionClass::window_keep`]): an
    /// aggregating shape's group table is its own declared breaker, so it
    /// takes no top-k window.
    pub(crate) fn aggregating(&self) -> bool {
        self.facts().aggregating
    }

    /// Whether the shape passes its feed through row by row: a
    /// non-aggregating shape with no `ORDER BY`. Each item is evaluated per
    /// row, a post-projection `WHERE` filters per row, and `SKIP`/`LIMIT`
    /// and `DISTINCT` apply on the stream, so the producer is never
    /// materialized ([`ProjectionClass::streams_pass`] is the clause's
    /// reading of the same fact).
    pub(crate) fn passes_stream(&self, fields: &ProjectionFields<'_>) -> bool {
        !self.aggregating() && fields.order.is_empty()
    }

    /// Whether the projection carries a `*` item — the shape a `*` owns
    /// outright, or the aggregating form that groups by every carried
    /// variable.
    pub(crate) fn star(&self) -> bool {
        self.facts().star
    }
    /// Whether the shape is the plain one: no aggregation and no `*` item,
    /// so each row projects alone and the clause keys its rows by the
    /// declared output columns.
    pub(crate) fn plain(&self) -> bool {
        let facts = self.facts();
        !facts.aggregating && !facts.star
    }
}

/// A shape's two facts — whether it aggregates, and whether it carries a
/// `*` item. The one reading of the shape's vocabulary: the enum's three
/// variants spell them out, and the predicates above never match on the
/// variants themselves.
#[derive(Clone, Copy)]
struct ShapeFacts {
    aggregating: bool,
    star: bool,
}

/// A `RETURN`/`WITH` projection's row-independent classification — the
/// plan every projection path consults, decided once per clause: the
/// decided [`ProjectionShape`], the clause's fields (the one field
/// bundle every consumer reads), ADR-0453's WHERE split, the eager-page
/// decision, and the parked-key plan and output names built from the
/// same fields. `project_rows` and `open_fold` read this one plan, so
/// the streaming decision, the fold and the projection path cannot
/// disagree on the clause's shape — and no site re-destructures the
/// fields or hand-builds a `ProjectionFields` literal. The shape alone
/// ([`ProjectionShape::decide`]) answers the dispatcher's cheap
/// questions without building the per-key plan.
pub(crate) struct ProjectionClass<'a> {
    /// The clause's fields — the one field bundle (a copy of the
    /// [`ProjectionFields`] every consumer reads).
    pub(crate) fields: ProjectionFields<'a>,
    /// The decided shape: which merge path owns the clause.
    pub(crate) shape: ProjectionShape,
    /// ADR-0453's one WHERE split, decided once in the mode the pipeline
    /// reads: an aggregating path whose WHERE carries aggregate calls
    /// takes it as HAVING (evaluated per group inside the aggregation);
    /// every other WHERE form — any WHERE on a non-aggregating path
    /// included — stays the plain post-merge filter.
    pub(crate) where_mode: Option<WhereMode<'a>>,
    /// The eager pre-projection page ([`pages_eagerly`]) on a
    /// non-aggregating shape: `SKIP`/`LIMIT` with no `ORDER BY`,
    /// DISTINCT, or filter.
    pub(crate) eager_paged: bool,
    /// The projection's parked-key plan — the one per-key scope decision
    /// every path below parks its keys with.
    pub(crate) order_keys: OrderKeys<'a>,
    /// The non-`*` items' output names, in item order — the clause's own
    /// schema only for a non-`*` shape. A `*` shape's result columns are
    /// the incoming visible bindings plus these, resolved by the finish
    /// tail; a reader of this field on a star row must read
    /// [`Self::shape`] first.
    pub(crate) out_names: Vec<String>,
    /// The explicit items with the output name each resolves to — the
    /// clause's `*` split and its names, resolved once here rather than
    /// re-asked or re-rendered per row: the row-assembly closures iterate
    /// this and never test [`is_star_item`] or call
    /// [`Projection::output_name`] themselves.
    pub(crate) value_items: Vec<(&'a Projection, String)>,
}

/// Whether a projection takes the eager pre-projection page: `SKIP`/`LIMIT`
/// with no `ORDER BY`, `DISTINCT`, or filter. It states the paging half of
/// the rule only — the aggregating exclusion belongs to the caller that
/// decided the shape ([`ProjectionClass::new`] applies it; the match sink's
/// decline chain has already rejected every aggregating form by the time it
/// asks).
pub(crate) fn pages_eagerly(fields: &ProjectionFields<'_>) -> bool {
    !fields.distinct
        && fields.order.is_empty()
        && fields.where_clause.is_none()
        && (fields.skip.is_some() || fields.limit.is_some())
}

/// The sort's bounds-free precondition: there is an ordering to cut, a
/// `LIMIT` to bound it, and no `DISTINCT` pass (which runs *after* the
/// sort, so a pre-distinct cut would answer fewer rows than the distinct
/// page deserves). The one home of these three facts —
/// [`ProjectionClass::sort_cut`] reads them for the cutoff.
pub(crate) fn may_cut(fields: &ProjectionFields<'_>) -> bool {
    !fields.distinct && !fields.order.is_empty() && fields.limit.is_some()
}

/// Whether a projection's `DISTINCT` is the first-seen pass over its input
/// stream (ADR-0660): the pass keeps only its charged seen-set, so it
/// neither collects the input nor sorts it. A shape that aggregates, or an
/// ordering (which runs on the distinct output), keeps the post-merge pass.
/// The one home of the decision: the projection's own consumption
/// ([`ProjectionClass::streams_distinct`]) reads it.
pub(crate) fn streams_distinct(fields: &ProjectionFields<'_>, shape: ProjectionShape) -> bool {
    fields.distinct && !shape.aggregating() && fields.order.is_empty()
}

impl<'a> ProjectionClass<'a> {
    /// The plan over an already-decided shape — the one construction
    /// site of the parked-key plan and the output names.
    pub(crate) fn new(
        graph: &ExecGraph<'_>,
        fields: &ProjectionFields<'a>,
        shape: ProjectionShape,
    ) -> Self {
        let aggregating = shape.aggregating();
        let where_mode = match fields.where_clause {
            Some(w) if aggregating && is_agg_expr(w) => Some(WhereMode::Having(w)),
            Some(w) => Some(WhereMode::Plain(w)),
            None => None,
        };
        let value_items: Vec<(&'a Projection, String)> = fields
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| !is_star_item(item))
            .map(|(index, item)| (item, item.output_name(index)))
            .collect();
        Self {
            fields: *fields,
            shape,
            where_mode,
            eager_paged: !aggregating && pages_eagerly(fields),
            order_keys: OrderKeys::new(
                graph,
                fields.order,
                fields.items,
                fields.group_by.is_some(),
            ),
            out_names: value_items.iter().map(|(_, name)| name.clone()).collect(),
            value_items,
        }
    }

    /// The projection's per-group subclause bundle over this plan: the
    /// parked-key plan, the WHERE mode (HAVING or the plain filter), and
    /// the import scope — the one derivation both `open_fold` and the
    /// aggregation merge path hand
    /// [`super::aggregate::Aggregation::open`].
    pub(crate) fn group_subclauses<'b>(&'b self, scope: Option<&'b Row>) -> GroupSubclauses<'b> {
        GroupSubclauses {
            order: &self.order_keys,
            where_mode: self.where_mode,
            import_scope: scope,
        }
    }

    /// The clause's resolved paging bounds — resolved **once** per clause,
    /// and the real bounds whatever the shape: a nondeterministic `LIMIT`
    /// bound would otherwise size the ordered window differently from the
    /// page it feeds, and a fabricated empty bound here would be a second
    /// answer to a question the feed already asked. Who *applies* them is
    /// the shape's own fact ([`Self::eager_paged`]): the feed pages on the
    /// eager shape, the finish tail on every other.
    pub(crate) fn page_bounds(
        &self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
    ) -> Result<PageBounds> {
        resolve_page_bounds(graph, params, self.fields.skip, self.fields.limit)
    }

    /// The sort's cutoff — `skip + limit`, the most rows an ordered page can
    /// need ([`window_capacity`]) — or `None` when the sort must keep every
    /// row: [`may_cut`]'s precondition fails (no order keys, no `LIMIT`, a
    /// `DISTINCT` pass) or the sum overflows. The one derivation of the
    /// cutoff; [`Self::window_keep`] masks it for the shape that takes a
    /// streaming window, and the finish tail's sort reads this value rather
    /// than recomputing it.
    pub(crate) fn sort_cut(&self, bounds: PageBounds) -> Option<std::num::NonZeroUsize> {
        if !may_cut(&self.fields) {
            return None;
        }
        window_capacity(bounds.skip, bounds.limit).and_then(std::num::NonZeroUsize::new)
    }

    /// Whether a *generated* feed reaching this projection stays generated,
    /// for exactly three reasons: the aggregation folds it into the group
    /// table, the ordered window folds it into the bounded heap, and the
    /// pass-through shape consumes it row by row ([`Self::streams_pass`],
    /// whose `DISTINCT` first-seen pass — a non-aggregating, unordered
    /// shape — keeps each row only long enough to decide it). Every row is
    /// evaluated as it arrives, so folding the generator changes no answer.
    /// This is the projection's own decision over the bounds it resolved
    /// once: a parked feed that resolves to no window is collected, not
    /// folded. [`prepare_input`][super::project::prepare_input] is its one
    /// reader.
    pub(crate) fn folds_feed(&self, bounds: PageBounds) -> bool {
        self.shape.streams() || self.streams_pass() || self.window_keep(bounds).is_some()
    }

    /// Whether the clause passes its input through as a stream: the shape's
    /// own fact ([`ProjectionShape::passes_stream`]) over this clause's
    /// fields. The landing funnel's page container
    /// ([`land_feed`][super::post_merge::land_feed]) applies the page and
    /// `DISTINCT` on the stream.
    pub(crate) fn streams_pass(&self) -> bool {
        self.shape.passes_stream(&self.fields)
    }

    /// Whether this clause's `DISTINCT` is the first-seen pass on its input
    /// stream rather than the post-merge pass — the class's reading of
    /// [`streams_distinct`].
    pub(crate) fn streams_distinct(&self) -> bool {
        streams_distinct(&self.fields, self.shape)
    }

    /// Whether the clause takes the pass-through page container — the one
    /// place that rule is spelled. The fold funnel's superset
    /// ([`land_feed`][super::post_merge::land_feed]): a named-`DISTINCT`
    /// shape, a plain `WHERE`, or any page. The `MATCH` sink admits the
    /// narrower named-`DISTINCT` form alone (its rows arrive unprojected,
    /// so a plain page is the projection path's).
    pub(crate) fn takes_page_container(&self, bounds: PageBounds) -> bool {
        self.streams_pass()
            && (self.streams_distinct() || self.plain_where().is_some() || bounds.is_paged())
    }

    /// ADR-0453's plain `WHERE` on this clause — the trailing predicate the
    /// page container applies to its answered page, or the finish tail to
    /// the page it cut. The one home of the plain/HAVING split: every
    /// consumer of the fact reads it here, never re-deriving it from
    /// `where_mode`.
    pub(crate) fn plain_where(&self) -> Option<&'a Expr> {
        self.where_mode.and_then(WhereMode::plain)
    }

    /// The bounded window this clause folds its producer into — its sort
    /// cutoff, masked for the aggregating shape: a group table is its own
    /// declared breaker and is already bounded by the group map, so the
    /// clause keeps its full group sort. The one window decision, beside
    /// the classification it reads: the landing funnels ([`super::post_merge::land_feed`],
    /// [`super::post_merge::adopt_rows`]) read it and every
    /// merge path reaches the container through that.
    /// An empty page is no window, and the capacity is a
    /// [`std::num::NonZeroUsize`] — so every window that is built holds at
    /// least one entry and no path carries a zero-capacity mode. The page's
    /// own emptiness is [`PageBounds::answers_nothing`], the same predicate
    /// the merge decides its empty result from; the invariant the capacity
    /// then carries is the type's, not a promise its callers keep.
    pub(crate) fn window_keep(&self, bounds: PageBounds) -> Option<std::num::NonZeroUsize> {
        if self.shape.aggregating() || bounds.answers_nothing() {
            return None;
        }
        self.sort_cut(bounds)
    }
}
