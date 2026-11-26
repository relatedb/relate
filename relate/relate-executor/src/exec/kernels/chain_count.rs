//! The chain kernel's count fold (LABS-1293): `MATCH (a …)-[:R]->()-[:R]->()-[:R]->(b …)
//! RETURN count(*)` (or `count(<pattern var>)`) answered by the unrolled
//! chain with the last hop tallied, never landed. Default match mode only;
//! every decline keeps the unchanged plan.

use relate_core::Result;

use super::super::agg_fold::FoldedReturn;
use super::super::match_folds::FoldCtx;
use super::count_entry::CountEntry;
use super::{chain, chain_stages_fit};

/// The fold entry: `clauses` starts at the `MATCH`, its `RETURN` follows.
pub(crate) fn try_fold_chain_count(ctx: &FoldCtx<'_>) -> Result<Option<FoldedReturn>> {
    let Some(entry) = CountEntry::open(ctx) else {
        return Ok(None);
    };
    let Some(contract) = entry.chain() else {
        return Ok(None);
    };
    // The cost gate prices the hops the walk lands; the tally hop is the
    // one it never lands — `Chain::split_last_hop`'s one spelling of the
    // compile's hop invariant, shared with `chain::run`.
    let (tallied, landed) = contract.split_last_hop();
    if !chain_stages_fit(&entry.stats(), entry.conjuncts(), &contract, landed) {
        return Ok(None);
    }
    let (exec, starts) = entry.scan(&contract)?;
    let count = chain::count(exec, tallied, landed, starts)?;
    Ok(super::count_fold_tail(
        exec.graph,
        u128::from(count),
        entry.item,
    ))
}
