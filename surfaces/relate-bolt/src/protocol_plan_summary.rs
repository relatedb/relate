//! The ending-summary plan policy (ADR-0344/0345, ADR-0353, ADR-0357): the
//! three outcomes of carrying an `EXPLAIN`/`PROFILE` statement's
//! structured plan on the final PULL or DISCARD SUCCESS, decided behind
//! one boundary so the ending pushes exactly what this policy returns.

use crate::packstream::{encode_error, Pack};
use crate::value_codec::ValueCodec;

/// The ending summary's plan entry (ADR-0344/0345, ADR-0353, ADR-0357):
/// `None` on a plain run (no plan stamped); the packed tree under `plan`
/// for EXPLAIN or `profile` for PROFILE when packstream can carry it; the
/// depth-truncation marker `planTruncated` when it cannot — the depth
/// limits are client-reachable (a deeply nested statement: the codec's own
/// 128-level cap, ADR-0628's plan-wire budget), so both degrade to the
/// marker, never a panic on client input. A path-invariant break
/// is an engine contract break, kept loud through the io error — the one
/// depth-vs-fatal partition (`MdbPackError::client_reachable`) the
/// RECORD write reads too. `codec` is the connection's value codec
/// (ADR-0484): the plan value converts through the same one entry the
/// RECORD rows do, so the version-gated forms hold here too.
pub(crate) fn plan_summary_pair(
    plan: Option<&relate::PlanNode>,
    codec: ValueCodec,
) -> std::io::Result<Option<(Pack, Pack)>> {
    // The Neo4j-facing plan value (ADR-0357) — the engine's one boundary,
    // root-shaped like this policy, so every surface carries the same plan
    // metadata.
    let Some(tree) = plan else {
        return Ok(None);
    };
    // The boundary's plan presentation (ADR-0359): the one formatter
    // composition, shared with the CLI and HTTP.
    let key = if tree.is_profiled() {
        "profile"
    } else {
        "plan"
    };
    let value = match relate_formatter::plan_presentation(tree) {
        Ok(value) => value,
        // ADR-0628's plan-wire budget: the same client-reachable depth
        // class the codec's own 128-level limit below belongs to — the
        // marker, never a panic on client input and never a swallowed
        // error.
        Err(relate::Error::Capacity {
            kind: relate::CapacityKind::PlanWire,
            ..
        }) => {
            return Ok(Some((
                Pack::String("planTruncated".into()),
                Pack::Bool(true),
            )));
        }
        // No other refusal exists today; a future one is an engine
        // contract break, kept loud through the io error.
        Err(e) => return Err(encode_error(e.to_string())),
    };
    match Pack::from_mdb(&value, codec) {
        Ok(pack) => Ok(Some((Pack::String(key.into()), pack))),
        // The depth limit is client-reachable (a deeply nested
        // statement), so it degrades to the truncation marker, never a
        // panic on client input.
        Err(e) if e.client_reachable() => Ok(Some((
            Pack::String("planTruncated".into()),
            Pack::Bool(true),
        ))),
        // The plan's args are scalars, so the element-id failure cannot
        // arise here; every other failure is an engine contract break,
        // kept loud through the io error. The partition itself is
        // `MdbPackError::client_reachable`'s, at the error definition —
        // the same one the RECORD write consults.
        Err(e) => Err(encode_error(e.to_string())),
    }
}
