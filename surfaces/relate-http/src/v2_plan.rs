//! The Query API v2 plan-node encoding (ADR-0365): the engine's plan tree
//! projected into Neo4j's own v2 node shape — beside [`crate::v2_json`]'s
//! value encoding, so the whole v2 wire translation lives in one family of
//! modules and `query_api` stays routing and envelopes.

use serde_json::{json, Map, Value as Json};

use relate::PlanNode;
use relate_model::wire::{self, plan_keys};
use relate_model::CypherValue;

use crate::v2_json::{v2_map_json_at, Form, ResponseMediaType};

/// The v2 plan node's args key. HTTP-specific: the engine's own wire form
/// and the Bolt wire both use `args` (the Bolt one is the driver's shape),
/// while the Query API v2 — and Neo4j's own response — names it
/// `arguments`. One spelling for this surface.
const V2_ARGUMENTS: &str = "arguments";

/// The v2 plan node's metric and page-cache FIELD names — the HTTP-only
/// vocabulary that must match Neo4j's own response (verified against Neo4j
/// 5.26). One spelling, beside [`V2_ARGUMENTS`], so the shape has a single
/// place to audit.
const V2_DB_HITS: &str = "dbHits";
const V2_RECORDS: &str = "records";
const V2_TIME: &str = "time";
const V2_HAS_PAGE_CACHE_STATS: &str = "hasPageCacheStats";
const V2_PAGE_CACHE_HITS: &str = "pageCacheHits";
const V2_PAGE_CACHE_MISSES: &str = "pageCacheMisses";
const V2_PAGE_CACHE_HIT_RATIO: &str = "pageCacheHitRatio";

/// The plan field of a success envelope: the summary key (`queryPlan` for
/// EXPLAIN, `profiledQueryPlan` for PROFILE — one interrogation of the
/// tree's typed fact) and the v2 plan node.
///
/// Both forms come from the formatter boundary's one plan-wire composition
/// (`relate_formatter::plan_wire_forms`, ADR-0628) — crossed once inside
/// it, never re-derived here. The walk reads the **undecorated** engine
/// wire form (the composition's `tree()`): the decorated root's
/// `EstimatedRows` is a Neo4j-facing float, which the engine-form accessor
/// rejects (ADR-0345). The root's `arguments` are overlaid with the
/// Neo4j-facing set the decoration produces (the identity and
/// `string-representation` args, ADR-0357) — read through the carrier's
/// one narrowing accessor (`root_args`, the single place its map
/// invariant is asserted), resolved once here, never a second lookup
/// inside the walk.
///
/// # Errors
///
/// A plan nesting past `relate::MAX_PLAN_WIRE_DEPTH` container levels —
/// ADR-0628's stack-safe budget — fails with the typed capacity refusal
/// whose text states the scope: the statement itself executed; only its
/// plan document is refused.
pub fn plan_field(tree: &PlanNode, form: Form) -> relate::Result<(&'static str, Json)> {
    let forms = relate_formatter::plan_wire_forms(tree)?;
    let root_args = forms.root_args();
    let mut node = v2_plan_node(forms.tree(), form)?;
    // One producer for `arguments`: the walker built the node's own args,
    // and the root's Neo4j-facing set replaces them here — never a second
    // pass narrowing the walker's output back to a map.
    node.insert(
        V2_ARGUMENTS.into(),
        Json::Object(v2_map_json_at(root_args, form, 0)?),
    );
    let key = if tree.is_profiled() {
        "profiledQueryPlan"
    } else {
        "queryPlan"
    };
    Ok((key, Json::Object(node)))
}

/// Plain-JSON Query API v2 plan field for an embedder's own success envelope
/// (ADR-0373).
///
/// Returns `queryPlan` for an `EXPLAIN` tree and `profiledQueryPlan` for a
/// `PROFILE` tree, with the node in the same shape [`plan_field`] writes when
/// the form is plain JSON (`ResponseMediaType::Json`). The HTTP server's
/// negotiated `{"$type","_value"}` form stays inside that server; embedders that speak plain JSON call this
/// instead of copying the walk. Fails with ADR-0628's typed plan-wire
/// budget refusal for a plan past `relate::MAX_PLAN_WIRE_DEPTH` — the
/// statement itself executed; only its plan document is refused.
pub fn query_plan_field(tree: &PlanNode) -> relate::Result<(&'static str, Json)> {
    // Plan arguments carry scalars, lists and maps, never an entity — an
    // entity here fails loudly (the encoder's contract) rather than taking a
    // fabricated namespace; the server's own entry ([`plan_field`]) passes
    // the statement's.
    plan_field(
        tree,
        Form {
            media_type: ResponseMediaType::Json,
            namespace: None,
        },
    )
}

/// One plan node in the Neo4j Query API v2 shape — verified against Neo4j
/// 5.26's own response: the metrics ride as node FIELDS (`dbHits`,
/// `records`, `time` in fractional ms, the page-cache trio), the args map
/// is named `arguments` (never the Bolt wire's `args` — that is the
/// driver's shape), and `operatorType` carries this engine's suffix
/// (`@relate`; Neo4j writes `@neo4j`).
///
/// Everything reads the canonical typed view ([`wire::plan_node_wire`]),
/// so a malformed node fails loudly there rather than being defaulted here.
/// The walker is a plain per-node recursion: the root's Neo4j-facing args
/// are overlaid once by [`plan_field`], never threaded as a mode.
fn v2_plan_node(value: &CypherValue, form: Form) -> relate::Result<Map<String, Json>> {
    let node = wire::plan_node_wire(value);
    let mut out = Map::new();
    out.insert(
        plan_keys::OPERATOR_TYPE.into(),
        Json::String(format!("{}@relate", node.operator_type)),
    );
    // `arguments` carries the node's own args as-is — the engine's richer
    // form (Neo4j keeps its metrics in `arguments` too, so nothing is
    // stripped) — as a PLAIN object whose leaf values carry the typed
    // encoding, Neo4j's own shape: the client iterates its entries and
    // decodes each value, so a tagged Map here would hand it `$type` /
    // `_value` entries and throw `Unknown type: undefined` (LABS-376).
    out.insert(
        V2_ARGUMENTS.into(),
        Json::Object(v2_map_json_at(node.args, form, 0)?),
    );
    // `identifiers` is a plain array of plain strings, like Neo4j's.
    out.insert(
        plan_keys::IDENTIFIERS.into(),
        Json::Array(
            node.identifiers
                .iter()
                .map(|name| Json::String((*name).to_string()))
                .collect(),
        ),
    );
    let (db_hits, records, time_ns) = match node.metrics {
        Some(m) => (m.db_hits, m.rows, m.time_ns),
        // An EXPLAIN node carries no metrics: the fields read 0, which is
        // the honest value (nothing was executed to measure).
        None => (0, 0, 0),
    };
    out.insert(V2_DB_HITS.into(), json!(db_hits));
    out.insert(V2_RECORDS.into(), json!(records));
    // The one nanoseconds-to-milliseconds conversion, as a float: Neo4j
    // carries fractional ms, and integer division would report 0 for
    // every sub-millisecond operator.
    out.insert(V2_TIME.into(), json!(wire::time_ns_ms(time_ns)));
    out.insert(V2_HAS_PAGE_CACHE_STATS.into(), Json::Bool(false));
    out.insert(V2_PAGE_CACHE_HITS.into(), json!(0));
    out.insert(V2_PAGE_CACHE_MISSES.into(), json!(0));
    out.insert(V2_PAGE_CACHE_HIT_RATIO.into(), json!(0.0));
    let mut children = Vec::with_capacity(node.children.len());
    for child in node.children {
        children.push(Json::Object(v2_plan_node(child, form)?));
    }
    out.insert(plan_keys::CHILDREN.into(), Json::Array(children));
    // The typed warnings ride the node like every other field — built from
    // the typed view ([`wire::PlanNodeWire::warnings`]), the accessor that
    // already validated them, never a second match of the raw wire map (a
    // second strictness would drift from the accessor's).
    if !node.warnings.is_empty() {
        out.insert(
            plan_keys::WARNINGS.into(),
            Json::Array(node.warnings.iter().map(wire::warning_wire_json).collect()),
        );
    }
    Ok(out)
}
