//! Replicated-batch ops shared by the end-to-end suites: the sentinel suite
//! writes them into a hand-built WAL, the size-limit suite applies them to an
//! open database.

use relate_executor::RedoOp;

/// A minimal labelled node op.
pub(crate) fn node(id: i64) -> RedoOp {
    RedoOp::create_node(
        id,
        vec!["High".to_string()],
        relate_core::PropertyMap::new(),
    )
}

/// A minimal typed relationship op between two node ids.
pub(crate) fn rel(id: i64, start: i64, end: i64) -> RedoOp {
    RedoOp::create_rel(
        id,
        start,
        end,
        "R".to_string(),
        relate_core::PropertyMap::new(),
    )
}

/// The invalid id shapes a replicated batch must refuse with a storage
/// error, each with its label — the one table both replicated-batch
/// suites iterate (the sentinel suite chains its non-id-shape ops onto
/// it), so a shape added here is covered by both.
pub(crate) fn invalid_id_shapes() -> Vec<(&'static str, RedoOp)> {
    vec![
        ("negative node", node(-1)),
        ("max node", node(i64::MAX)),
        ("negative rel", rel(-1, 0, 1)),
        ("max rel", rel(i64::MAX, 0, 1)),
        ("negative start", rel(1, -1, 1)),
        ("max start", rel(1, i64::MAX, 1)),
        ("negative end", rel(1, 0, -1)),
        ("max end", rel(1, 0, i64::MAX)),
    ]
}
