//! The `Notifications:` text block of the table and markdown renderers
//! (ADR-0363 decision 8): one line per notification. The table block skips
//! the plan table's already-rendered warning kinds; markdown has no plan
//! table, so it renders the complete list.

use std::io::Write;

use relate_model::wire::Notification;

use crate::escape::escape_control;

/// Render the one `Notifications:` block (ADR-0363 decision 8) after the
/// table: one line per notification the plan table does not render, in the
/// engine's list order,
/// `<SEVERITY> <gql_status> <code>: <description> Remedy: <remedy>`, with
/// `Id <id>: ` prefixed on an operator-scoped entry. The block is absent
/// when the filtered list is empty. The table policy never escapes pipes —
/// that boundary is markdown's alone ([`format_complete_notifications`]) —
/// and every line takes the same control-character policy as every cell.
pub(crate) fn format_notifications(
    w: &mut dyn Write,
    notifications: &[Notification],
) -> std::io::Result<()> {
    write_block(
        w,
        notifications
            .iter()
            // The plan table's `Warnings` column plus footer is the one
            // text rendering of the plan-warning kinds (ADR-0344/0345);
            // the model's exhaustive membership decides the partition.
            .filter(|notification| !notification.kind().is_plan_warning()),
        false,
    )
}

/// Render the complete notification list after the table. Markdown never
/// renders the plan table (it has no plan presentation), so the
/// plan-warning kinds have no other rendering there and the block must
/// carry them too — the list is never filtered against a table that is not
/// printed. Markdown is the one policy that escapes pipes.
pub(crate) fn format_complete_notifications(
    w: &mut dyn Write,
    notifications: &[Notification],
) -> std::io::Result<()> {
    write_block(w, notifications.iter(), true)
}

/// The one block writer behind both policies: the heading only when at
/// least one notification survives the caller's filter, then one escaped
/// line each.
fn write_block<'a>(
    w: &mut dyn Write,
    notifications: impl Iterator<Item = &'a Notification>,
    escape_pipe: bool,
) -> std::io::Result<()> {
    let mut notifications = notifications.peekable();
    if notifications.peek().is_none() {
        return Ok(());
    }
    writeln!(w, "Notifications:")?;
    for notification in notifications {
        writeln!(
            w,
            "{}",
            escape_control(&notification_line(notification), escape_pipe)
        )?;
    }
    Ok(())
}

/// One notification's text line: the ADR-0363 decision-8 shape, with the
/// operator association first when the record carries one.
fn notification_line(notification: &Notification) -> String {
    let operator = match notification.operator_id() {
        Some(id) => format!("Id {id}: "),
        None => String::new(),
    };
    format!(
        "{operator}{} {} {}: {} Remedy: {}",
        notification.severity(),
        notification.gql_status(),
        notification.code(),
        notification.description(),
        notification.remedy(),
    )
}

#[cfg(test)]
mod tests {
    use relate::PlanWarning;
    use relate_model::wire::NotificationKind;

    use super::*;

    /// The plan table's warning kinds never reach the text block; every
    /// other kind does — the ADR-0363 decision-8 partition by kind.
    #[test]
    fn the_partition_excludes_exactly_the_plan_warning_kinds() {
        assert!(NotificationKind::CartesianProduct.is_plan_warning());
        assert!(NotificationKind::AllNodesScan.is_plan_warning());
        assert!(NotificationKind::EstimationMiss.is_plan_warning());
        for kind in [
            NotificationKind::UnboundedVariableLengthPattern,
            NotificationKind::MissingIndex,
            NotificationKind::EagerAggregation,
            NotificationKind::UnknownLabelWarning,
            NotificationKind::UnknownRelationshipTypeWarning,
            NotificationKind::UnknownPropertyKeyWarning,
            NotificationKind::DeprecatedFunction,
            NotificationKind::DeprecatedProcedure,
            NotificationKind::CallSubqueryWithoutScope,
        ] {
            assert!(
                !kind.is_plan_warning(),
                "{kind:?} belongs in the Notifications block"
            );
        }
    }

    /// The partition follows the engine's projection: every `PlanWarning`
    /// variant projects to a kind the plan table renders. The match below
    /// is exhaustive on purpose, so a new variant cannot compile here until
    /// it joins the list and picks its side.
    #[test]
    fn the_partition_covers_every_plan_warning_variant() {
        for warning in [
            PlanWarning::EstimationMiss,
            PlanWarning::CartesianProduct,
            PlanWarning::AllNodesScan {
                index: ":Person(name)".to_string(),
            },
            PlanWarning::LabelDisjunctionScan {
                indexes: ":Model(id), :Source(id)".to_string(),
            },
        ] {
            match &warning {
                PlanWarning::EstimationMiss
                | PlanWarning::CartesianProduct
                | PlanWarning::AllNodesScan { .. }
                | PlanWarning::LabelDisjunctionScan { .. } => {}
            }
            let kind = warning.notification_kind();
            assert!(
                kind.is_plan_warning(),
                "{warning:?} projects to {kind:?}, which the plan table renders"
            );
        }
    }

    /// The line shape is the ADR's: severity, GQLSTATUS, code, description,
    /// then `Remedy:`, with the operator prefix only when present.
    #[test]
    fn the_line_shape_matches_the_adr() {
        let statement = Notification::new(NotificationKind::UnknownLabelWarning, "Person");
        assert_eq!(
            notification_line(&statement),
            "WARNING 01N50 Neo.ClientNotification.Statement.UnknownLabelWarning: Person \
             Remedy: Check the spelling; the label exists after the first write that creates it, \
             and no index or constraint uses the unknown name."
        );
        let operator =
            Notification::new(NotificationKind::MissingIndex, "no index on :Person(email)")
                .with_operator_id(4)
                .with_remedy("CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)");
        assert_eq!(
            notification_line(&operator),
            "Id 4: INFORMATION 03C91 Relate.Notification.Statement.MissingIndex: no index on \
             :Person(email) Remedy: CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)"
        );
    }
}
