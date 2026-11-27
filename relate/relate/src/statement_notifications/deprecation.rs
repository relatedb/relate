//! The deprecated-builtin notification cluster (ADR-0363): the one
//! typed surface, the one lookup-and-push policy, and the remedy
//! renderer — extracted beside `hints`/`redundant_optional` so the
//! parent module stays the dispatcher and this concern owns its whole
//! shape. The `CALL` form and both function call-site shapes (the
//! expression walk's surviving calls, the SEARCH filter's consumed
//! left-hand sides) all route through [`push`], so the lookup, the
//! construction and the push can never drift into two spellings.

use relate_names::SmallName;

use relate_executor::{
    builtin_function_deprecation, builtin_procedure_deprecation, DeprecatedBy, DeprecationForm,
};
use relate_model::wire::{Notification, NotificationKind};

/// One deprecated catalog surface: the typed pairing of the
/// notification kind, the prose noun, the subject spelling and the
/// catalog's own deprecation table — one fact, carried together, so no
/// call site can hand a kind with the wrong noun, the wrong spelling or
/// the wrong lookup table.
#[derive(Clone, Copy)]
pub(super) enum DeprecationSurface {
    Function,
    Procedure,
}

impl DeprecationSurface {
    /// The deprecation notification for one name on this surface:
    /// `None` when the catalog does not carry the name as deprecated.
    /// The one exhaustive surface match — kind, noun, spelling and
    /// table all live here, so a third surface is one arm plus a
    /// decision, never four more literal variant lists.
    fn notification(self, name: &str) -> Option<Notification> {
        let (kind, noun, subject, replacement) = match self {
            DeprecationSurface::Function => (
                NotificationKind::DeprecatedFunction,
                "function",
                format!("`{name}()`"),
                builtin_function_deprecation(name)?,
            ),
            DeprecationSurface::Procedure => (
                NotificationKind::DeprecatedProcedure,
                "procedure",
                format!("`{name}`"),
                builtin_procedure_deprecation(name)?,
            ),
        };
        Some(
            Notification::new(kind, format!("The {noun} {subject} is deprecated."))
                .with_remedy(deprecation_remedy(replacement)),
        )
    }
}

/// The one deprecated-builtin notify policy: look the name up in its
/// surface's catalog table and push one notification when it is marked
/// deprecated.
pub(super) fn push(
    notifications: &mut Vec<Notification>,
    surface: DeprecationSurface,
    name: &SmallName,
) {
    name.with_str(|text| notifications.extend(surface.notification(text)));
}

/// The instance remedy for a catalog replacement: an identifier/call is
/// rendered as code, a clause is named as a clause, and the catalog's
/// display prose never reaches the backticks.
fn deprecation_remedy(replacement: DeprecatedBy) -> String {
    match replacement.form() {
        DeprecationForm::Identifier(identifier) => format!("Use `{identifier}` instead."),
        DeprecationForm::Clause(clause) => format!("Use the `{clause}` clause instead."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A replacement renders by its syntactic form: an identifier/call is
    /// code, a clause is named as a clause, and catalog prose never reaches
    /// the remedy.
    #[test]
    fn deprecation_remedy_renders_identifier_and_clause_forms() {
        assert_eq!(
            deprecation_remedy(DeprecatedBy::identified(
                "elementId",
                "elementId or consider using an application-generated id",
            )),
            "Use `elementId` instead."
        );
        assert_eq!(
            deprecation_remedy(DeprecatedBy::identifier("db.cdc.current")),
            "Use `db.cdc.current` instead."
        );
        assert_eq!(
            deprecation_remedy(DeprecatedBy::clause("SEARCH")),
            "Use the `SEARCH` clause instead."
        );
    }
}
