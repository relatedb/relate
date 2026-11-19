//! The boundary's plan-wire seam (ADR-0359, ADR-0628): the one composition
//! every plan-carrying surface reads — the engine's own wire form crossed
//! once (the plan-policy crossing is this module's property, never a use
//! site's decision, ADR-0628), the plan text rendered from that crossing,
//! and the Neo4j-facing decoration built on the text. The table layout the
//! text renders through lives in `plan_table`; this module owns the
//! composition over it. Bolt reads the engine `Value` form through
//! [`plan_presentation`] (its packer is its own, not the model bridge,
//! ADR-0628); every CypherValue consumer — this crate's
//! [`crate::PlanPresentation::of`], the HTTP Query API v2 plan field —
//! reads [`plan_wire_forms`], and a text-only consumer reads
//! [`plan_text_of`].
//!
//! Every entry fails with ADR-0628's typed plan-wire budget refusal —
//! [`relate::Error::Capacity`] in the
//! [`relate::CapacityKind::PlanWire`] class — for a plan nesting past
//! [`relate::MAX_PLAN_WIRE_DEPTH`] container levels: never a silent
//! truncation, never a panic, and the error's text states the scope (the
//! statement itself executed; only its plan document is refused). The
//! gate fires from the TYPED tree (the iterative
//! `PlanNode::ensure_plan_wire_budget` probe) before any recursive
//! producer build runs on the caller's thread — the security round's
//! closure.

use relate_model::wire::plan_keys;
use relate_model::{CypherValue, IndexMap};

use crate::plan_table::plan_text_lines;

/// The boundary's plan presentation for one result (ADR-0359): the engine's
/// Neo4j-facing value with the plan text rendered through the one layout
/// inside it — the composition every plan-carrying surface reads (Bolt
/// through this entry, the CLI and HTTP through [`plan_wire_forms`], a
/// text-only consumer through [`plan_text_of`]), so the rendering and the
/// decoration cannot drift. The summary KEY stays
/// surface-specific (`plan`/`profile` for Bolt, `queryPlan`/
/// `profiledQueryPlan` for HTTP, the formatter's own modes for the CLI),
/// read off `tree.is_profiled()` at the caller. A caller needing only the
/// header lines reads [`crate::plan_header_lines`], not this return.
///
/// Unlike the layout entry, this takes the engine's `PlanNode`: it must
/// call the engine's decorator, and the wire value alone cannot produce the
/// decorated form. ADR-0359's rejected alternative was the *renderer*
/// taking `PlanNode`; the renderer still takes the wire value.
///
/// The engine `Value` return is Bolt's packer entry — its codec is its
/// own, not the model bridge (ADR-0628).
pub fn plan_presentation(tree: &relate::PlanNode) -> relate::Result<relate::Value> {
    Ok(plan_composition(tree)?.decorated)
}

/// The plan-wire forms of one result (ADR-0359, ADR-0628): the same
/// composition as [`plan_presentation`] with both forms crossed to the
/// model value **once, inside** — the engine's own wire form is crossed
/// once, never per consumer: the plan text render, the presentation
/// bundle's `tree` field and the HTTP v2 node walk all read the same
/// crossing.
///
/// The fields are sealed (`pub(crate)`): [`plan_wire_forms`] is the only
/// constructor, so the carrier's invariant — `root_args`' always-a-map
/// wire — holds by compiler enforcement, the same sealing
/// [`crate::PlanPresentation`] gives its bundle, never by review.
pub struct PlanWireForms {
    pub(crate) wire: CypherValue,
    pub(crate) tree: CypherValue,
}

impl PlanWireForms {
    /// The Neo4j-facing plan value (ADR-0357) — the decorated root —
    /// crossed to the model value.
    pub fn wire(&self) -> &CypherValue {
        &self.wire
    }

    /// The engine's own wire form (ADR-0345), crossed once — its
    /// `EstimatedRows` stays an integer, so the decorated `wire` form's
    /// float mirror never reaches the engine-form accessors.
    pub fn tree(&self) -> &CypherValue {
        &self.tree
    }

    /// The decorated root's args map (ADR-0357) — the one place the
    /// carrier's invariant is asserted: the composition always builds the
    /// Neo4j-facing `Value::Map`, so the crossed `wire` form is always a
    /// `CypherValue::Map` carrying an args map; anything else is a
    /// wire-contract break that fails loudly here, never re-asserted at
    /// each use site. Unreachable behind the sealed fields and the one
    /// constructor.
    pub fn root_args(&self) -> &IndexMap<String, CypherValue> {
        let CypherValue::Map(map) = &self.wire else {
            panic!(
                "the decorated plan is a map (ADR-0357), got {:?}",
                self.wire
            )
        };
        match map.get(plan_keys::ARGS) {
            Some(CypherValue::Map(args)) => args,
            other => panic!("the decorated plan carries an args map (ADR-0357), got {other:?}"),
        }
    }
}

/// The one plan-wire composition for every CypherValue consumer (see
/// [`PlanWireForms`]): both forms crossed once, here — a use site reads
/// the result, never re-deriving a crossing of its own.
pub fn plan_wire_forms(tree: &relate::PlanNode) -> relate::Result<PlanWireForms> {
    let composed = plan_composition(tree)?;
    Ok(PlanWireForms {
        wire: relate::plan_value_to_cypher(&composed.decorated)?,
        tree: composed.tree,
    })
}

/// The plan's text lines (ADR-0359): the boundary's text-only entry — the
/// two primitives of the composition on their own, the engine's own wire
/// form crossed once, then the one layout over that crossing. A consumer
/// that wants only the text (the crate's own example) reads this instead
/// of the bundle; a consumer needing the header lines alone reads
/// [`crate::plan_header_lines`].
pub fn plan_text_of(tree: &relate::PlanNode) -> relate::Result<Vec<String>> {
    Ok(plan_text_lines(
        &plan_tree_crossed(tree)?,
        tree.statement_echo(),
    ))
}

/// The composition core both entries share (ADR-0359, ADR-0628): the
/// engine's own wire form crossed once — the one crossing the text render,
/// the presentation bundle's `tree` field and the HTTP v2 node walk read —
/// the plan text rendered from that crossing, and the Neo4j-facing
/// decoration built on the text.
struct PlanComposition {
    /// The engine's own wire form, crossed (ADR-0628's plan-wire crossing,
    /// under its stack-safe budget).
    tree: CypherValue,
    /// The Neo4j-facing decorated value (ADR-0357), engine `Value` form —
    /// Bolt's packer reads this one.
    decorated: relate::Value,
}

fn plan_composition(tree: &relate::PlanNode) -> relate::Result<PlanComposition> {
    // The echo is a plain accessor (empty on a hand-built tree): a public
    // helper never panics on a tree the engine did not stamp.
    let crossed = plan_tree_crossed(tree)?;
    // The same render [`plan_text_of`] runs, over this composition's one
    // crossing — never a second crossing for the text.
    let text = plan_text_lines(&crossed, tree.statement_echo()).join("\n");
    Ok(PlanComposition {
        tree: crossed,
        decorated: tree.to_neo4j_value(&text),
    })
}

/// The composition's first primitive: the engine's own wire form, crossed
/// once (ADR-0628) — the crossing every consumer of the plan's wire forms
/// reads, never a use site's own. The budget fires BEFORE the recursive
/// producer build: the probe walks the typed tree iteratively, so a
/// past-budget plan refuses here without `to_value` ever building the
/// engine `Value` tree on the caller's thread (the security round's
/// closure) — and every entry over the composition (Bolt's
/// [`plan_presentation`], [`plan_wire_forms`], [`plan_text_of`]) inherits
/// the gate through this one primitive.
fn plan_tree_crossed(tree: &relate::PlanNode) -> relate::Result<CypherValue> {
    tree.ensure_plan_wire_budget()?;
    relate::plan_value_to_cypher(&tree.to_value())
}
