//! ADR-0660 + ADR-0463: the inter-clause drain is a retaining container.
//!
//! A generator whose follower is another generator hands its rows on
//! through `RowStream::drain`, the incremental migration's one held
//! handoff. The vector that drain accumulates is charged as it lands, so a
//! nested `UNWIND` refuses under the workspace pool the moment the first
//! expansion crosses it — the same reservation the materializing form
//! makes, and never a pass because the expansion was never billed.

mod common;

use relate::Value;

/// The workspace pool: room for the statement's seed and answer, none for
/// the first generator's expansion.
const BUDGET: u64 = 1 << 20;
/// The first generator's expansion, a few times the pool at roughly fifty
/// bytes a row.
const EXPANSION: i64 = 200_000;

/// The first `UNWIND`'s stream is drained by the second `UNWIND` before
/// that generator parks, so the drain's vector is the one container the
/// statement holds; it must be on the pool.
#[test]
fn a_nested_unwind_refuses_when_the_drained_expansion_crosses_the_pool() {
    let mut c = common::workspace_capped(BUDGET);
    common::refused_workspace_limit(
        &mut c,
        &format!("UNWIND range(1, {EXPANSION}) AS i UNWIND [i] AS j RETURN count(*) AS n"),
    );

    // The same first generator with no follower draining it answers under
    // the pool, so the refusal above is the drain's charge, not the
    // generator's own.
    let counted = common::scalar(
        &mut c,
        &format!("UNWIND range(1, {EXPANSION}) AS i RETURN count(*) AS n"),
    );
    assert_eq!(counted, Value::Int(EXPANSION));
}
