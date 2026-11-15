//! The redundant-`OPTIONAL` notifications (ADR-0363): the one home of the
//! family. `03N61` fires for `OPTIONAL CALL` over a void procedure and
//! `03N62` for `OPTIONAL CALL { … }` over a unit subquery — in both the
//! `OPTIONAL` adds nothing because the call yields the same single row
//! either way. Callers have already established that the clause is
//! `OPTIONAL`; each entry decides only whether the call projects columns.

use relate_executor::{is_void_procedure, Procedure};
use relate_ir::Query;
use relate_model::wire::{Notification, NotificationKind};
use relate_names::SmallName;

/// `OPTIONAL CALL <void procedure>` (`03N61`). Voidness is the shared
/// [`is_void_procedure`] predicate the `CALL` contract already reads, not a
/// second spelling of it.
pub(super) fn observe_call(
    notifications: &mut Vec<Notification>,
    registered: Option<&Procedure>,
    name: &SmallName,
) {
    if name.with_str(|text| is_void_procedure(registered, text)) {
        notifications.push(Notification::new(
            NotificationKind::RedundantOptionalProcedure,
            format!("The use of `OPTIONAL` is redundant as `CALL {name}()` is a void procedure."),
        ));
    }
}

/// `OPTIONAL CALL { … }` whose body projects no columns (`03N62`).
pub(super) fn observe_subquery(notifications: &mut Vec<Notification>, query: &Query) {
    if !query.projects_columns() {
        notifications.push(Notification::new(
            NotificationKind::RedundantOptionalSubquery,
            "The use of `OPTIONAL` is redundant as `CALL` is a unit subquery.",
        ));
    }
}
