use super::*;

mod bytes;
mod chunk;
mod fold;

/// The due predicate for one type id, as the tests read it: the selection
/// the production pass makes ([`TypedAdj::due_chunks`]), narrowed to a type.
pub(super) fn due(
    a: &TypedAdj,
    sealed: Option<SealedRegion<'_>>,
    type_id: u32,
    horizon: u64,
) -> bool {
    a.due_chunks(sealed, horizon)
        .iter()
        .any(|candidate| candidate.type_id == type_id)
}

/// The production fold's two steps: due-ness against the node's sealed
/// region, then the fold into it. Returns whether anything was folded.
pub(super) fn fold(a: &mut TypedAdj, sealed: SealedRegion<'_>, horizon: u64) -> bool {
    let due = a.due_chunks(Some(sealed), horizon);
    a.fold_due(sealed, &due, horizon)
}
