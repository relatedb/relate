//! The `tx.setMetaData` diverted executor (ADR-0610): the void call's
//! merge side effect. While the `value-provenance` feature is off the
//! call is exactly the compat no-op the corpus pins; while it is on each
//! bound map merges into the connection's per-transaction metadata cell,
//! to be serialized into the commit's record at the next WAL boundary.
//! A child of `exec`, beside the dispatch module — the dispatch site
//! (`exec::procedures`) classifies the name once, binds it through the
//! generic policy, and hands the arm here.

use relate_core::{Error, Result, Value};

use crate::ctx::ExecCtx;
use crate::row::Row;

use super::procedures::{pad_or_each, CallBinding, CallSpec};

/// `tx.setMetaData` (CIP-230, ADR-0610): a void diverted call. While the
/// `value-provenance` feature is off it is exactly the compat no-op — the
/// arguments are bound and type-checked by the same
/// [`CallBinding::bind_row`] policy the generic path applies (a non-map
/// argument is the same `InvalidArgumentType` rejection), and nothing is
/// stored. While the feature is on, each bound map merges into the
/// connection's per-transaction metadata cell (the view's
/// [`ExecGraph::merge_tx_metadata`] — later keys win, the merged map
/// bounded by the per-object envelope, and the channel hard-sets the
/// provenance record type) to be serialized into the commit's
/// record at the next WAL boundary.
pub(super) fn exec_tx_set_metadata_call(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    call: CallSpec<'_>,
    binding: CallBinding<'_>,
) -> Result<(Vec<Row>, Option<Row>)> {
    let optional = call.optional;
    // The feature gate, read once per call: the off form is the compat
    // no-op — every row's arguments are still bound and type-checked.
    let enabled = ctx
        .graph
        .preview()
        .enabled(relate_core::PreviewFeature::ValueProvenance);
    // The void-procedure policy's one spelling (the shared helper): the
    // bind-and-merge runs once per input row, the pad is the unit row's
    // own — completed by the clause arm like every void call's.
    let pad = pad_or_each(&rows, optional, |row| {
        let args = binding.bind_row(row)?;
        if !enabled {
            return Ok(());
        }
        match args.get("metadata") {
            Some(Value::Map(map)) => ctx.graph.merge_tx_metadata(map.as_ref().clone())?,
            // The binding's type check admits null for every declared type;
            // a null map sets nothing, the same no-op the off form is.
            Some(Value::Null) | None => {}
            // The MAP type check above rejected every other shape, so this
            // arm is the checked invariant, not a reachable fallback.
            Some(_) => {
                return Err(Error::internal(format!(
                    "{} argument `metadata` escaped its MAP type check",
                    call.name
                )))
            }
        }
        Ok(())
    })?;
    Ok((rows, pad))
}
