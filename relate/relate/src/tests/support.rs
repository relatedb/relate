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
