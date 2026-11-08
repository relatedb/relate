//! Per-mode notification rendering (ADR-0363 decisions 8/9): the envelope
//! always carries the complete list, json/jsonl carry it inside their plan
//! object, toon renders it beside `plan:`, table renders one
//! `Notifications:` block after the plan table with the plan-warning kinds
//! filtered out, markdown (which never renders the plan table) renders the
//! complete list, and csv/cypher render rows only.

use super::*;
use relate_model::wire::{Notification, NotificationKind};

/// The complete engine-shaped list: an operator-scoped Cartesian product
/// (rendered by the plan table), a statement-scoped unknown label and an
/// operator-scoped missing index — one of each rendering class.
fn notifications_fixture() -> Vec<Notification> {
    vec![
        Notification::new(
            NotificationKind::CartesianProduct,
            "the MATCH patterns share no join anchor",
        )
        .with_operator_id(1),
        Notification::new(NotificationKind::UnknownLabelWarning, "Person"),
        Notification::new(NotificationKind::MissingIndex, "no index on :Person(email)")
            .with_operator_id(2)
            .with_remedy("CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)"),
    ]
}

/// The plan table's header lines for the Cartesian-product fixture, through
/// the canonical header producer — never a hand-copy of its `Query:` echo
/// and `Plan:` marker.
fn explain_header() -> Vec<String> {
    plan_header_lines("MATCH (a:Person), (b:City) RETURN a, b")
}

/// Render an EXPLAIN result (no columns, no rows — ADR-0359) under `mode`
/// carrying the structured plan and its notification list.
fn render_explain(mode: Mode, plan: &PlanFixture, notifications: &[Notification]) -> String {
    // An EXPLAIN result carries no columns and no rows (ADR-0359).
    render_plan_notified(mode, &[], &[], plan, &explain_header(), notifications)
}

#[test]
fn envelope_always_carries_the_complete_notification_list() {
    // ADR-0363 decision 9: the envelope's `notifications` key is always
    // present, carries the native keys with the remedy, and is not filtered
    // — the plan table's rendering policy is a text-mode concern. The list
    // rides its plan view, the only shape the engine produces.
    let plan = plan_fixture();
    let notifications = notifications_fixture();
    let out = render_explain(Mode::Envelope, &plan, &notifications);
    let envelope: serde_json::Value = serde_json::from_str(&out).unwrap();
    let list = envelope[relate_model::wire::notification_keys::NOTIFICATIONS]
        .as_array()
        .unwrap();
    assert_eq!(list.len(), 3, "{out}");
    assert_eq!(
        list[0]["code"],
        "Neo.ClientNotification.Statement.CartesianProduct"
    );
    assert_eq!(list[0]["gql_status"], "03N90");
    assert_eq!(list[0]["severity"], "INFORMATION");
    assert_eq!(list[0]["category"], "PERFORMANCE");
    assert_eq!(list[0]["title"], "Cartesian product");
    assert_eq!(list[0]["operator_id"], 1);
    // The statement-scoped entry has no `operator_id` key; the operator-
    // scoped one has, with the instance remedy.
    assert!(list[1].get("operator_id").is_none(), "{out}");
    assert_eq!(list[1]["severity"], "WARNING");
    assert_eq!(
        list[1]["code"],
        "Neo.ClientNotification.Statement.UnknownLabelWarning"
    );
    assert_eq!(list[2]["operator_id"], 2);
    assert_eq!(
        list[2]["remedy"],
        "CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)"
    );
    for entry in list {
        for key in [
            "code",
            "gql_status",
            "severity",
            "category",
            "title",
            "description",
            "remedy",
        ] {
            assert!(entry.get(key).is_some(), "missing {key}: {out}");
        }
    }
}

#[test]
fn envelope_keeps_the_empty_notification_array_on_a_plain_run() {
    let cols = vec!["a".to_string()];
    let out = render(Mode::Envelope, &cols, &[]);
    let envelope: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        envelope[relate_model::wire::notification_keys::NOTIFICATIONS],
        serde_json::json!([])
    );
}

/// A non-empty list can only exist on its plan view (the engine derives it
/// with the tree), so no mode can be handed a list it silently drops — the
/// plain-run bundle derives the empty list from the absent view, and a list
/// that exists is emitted by every mode that surfaces notifications.
#[test]
fn a_non_empty_list_cannot_ride_a_plain_run() {
    let plain = StatementMeta::default();
    assert!(plain.plan.is_none());
    assert!(plain.notifications().is_empty());

    let cols = vec!["a".to_string()];
    let rows = vec![vec![CypherValue::Int(1)]];
    let plan = plan_fixture();
    let notifications = notifications_fixture();
    for mode in [Mode::Envelope, Mode::Json, Mode::Jsonl, Mode::Toon] {
        let out = render_plan_notified(mode.clone(), &cols, &rows, &plan, &[], &notifications);
        for kind in ["CartesianProduct", "UnknownLabelWarning", "MissingIndex"] {
            assert!(
                out.contains(kind),
                "{mode:?} dropped a notification it was handed: {out}"
            );
        }
    }
}

#[test]
fn table_renders_one_notification_block_without_the_plan_warning_kinds() {
    // ADR-0363 decision 8: after the plan table, one `Notifications:` block
    // carries every kind except the three already rendered by the plan table
    // (CartesianProduct/AllNodesScan/EstimationMiss). The operator prefix is
    // `Id <id>: ` when the record carries `operator_id`.
    let plan = plan_fixture();
    let notifications = notifications_fixture();
    let out = render_explain(Mode::Table, &plan, &notifications);
    let block_start = out
        .find("Notifications:\n")
        .unwrap_or_else(|| panic!("the block follows the plan table: {out}"));
    let block = &out[block_start + "Notifications:\n".len()..];
    let lines: Vec<&str> = block.lines().collect();
    assert_eq!(lines.len(), 2, "one line per kept notification: {out}");
    assert_eq!(
        lines[0],
        "WARNING 01N50 Neo.ClientNotification.Statement.UnknownLabelWarning: Person Remedy: \
         Check the spelling; the label exists after the first write that creates it, and no \
         index or constraint uses the unknown name."
    );
    assert_eq!(
        lines[1],
        "Id 2: INFORMATION 03C91 Relate.Notification.Statement.MissingIndex: no index on \
         :Person(email) Remedy: CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)"
    );
    // The plan table's warning kind is printed exactly once — by the plan
    // table's own warnings rendering, never the notification block.
    assert!(
        !block.contains("Cartesian"),
        "the plan-warning kind is not repeated: {out}"
    );
    assert_eq!(
        out.matches("CartesianProduct").count(),
        0,
        "no plan-warning text in the block: {out}"
    );
}

#[test]
fn table_omits_the_block_when_only_plan_warning_kinds_remain() {
    let plan = plan_fixture();
    let notifications = [
        Notification::new(NotificationKind::CartesianProduct, "no join anchor").with_operator_id(1),
        Notification::new(NotificationKind::AllNodesScan, "every node").with_operator_id(1),
        Notification::new(NotificationKind::EstimationMiss, "10x estimate").with_operator_id(1),
    ];
    let out = render_explain(Mode::Table, &plan, &notifications);
    assert!(
        !out.contains("Notifications:"),
        "the block is absent when the filtered list is empty: {out}"
    );
}

/// The formatter is the one owner of table-mode plan routing
/// (ADR-0359/LABS-373, ADR-0363 decision 8): the data table goes to the
/// data writer, while the plan presentation and its one notification block
/// go to the diagnostic writer — the data stream never sees plan text.
#[test]
fn table_routes_the_plan_and_its_block_to_the_diagnostic_writer() {
    let cols = vec!["a".to_string()];
    let rows = vec![vec![CypherValue::Int(1)]];
    // The LABS-373 data-table-beside-the-plan shape is a PROFILE result's
    // (ADR-0359: an EXPLAIN result carries no data rows) — the profile
    // fixture, whose class the typed node states.
    let plan = profile_fixture();
    let notifications = notifications_fixture();
    let header = explain_header();
    let mut data = Vec::new();
    let mut diagnostics = Vec::new();
    Formatter::new(Mode::Table)
        .format_with(
            &mut data,
            &mut diagnostics,
            &cols,
            &ordered_rows(&cols, &rows),
            StatementMeta {
                column_types: &[],
                row_cap: None,
                plan: Some(notified_view(
                    &plan.value,
                    &header,
                    &notifications,
                    plan.profiled,
                )),
                timings: None,
            },
        )
        .unwrap();
    let data = String::from_utf8(data).unwrap();
    let diagnostics = String::from_utf8(diagnostics).unwrap();
    assert!(
        data.contains("| a |") && data.contains("| 1 |"),
        "the data table stays on the data writer: {data}"
    );
    assert!(
        !data.contains("Plan:") && !data.contains("Notifications:"),
        "the data writer never receives the plan: {data}"
    );
    assert!(
        diagnostics.starts_with("Query: "),
        "the plan presentation heads the diagnostic stream: {diagnostics}"
    );
    assert!(diagnostics.contains("Plan:"), "{diagnostics}");
    assert!(
        diagnostics.contains("Notifications:\n"),
        "the block follows the plan table: {diagnostics}"
    );
    assert!(
        diagnostics.contains("UnknownLabelWarning"),
        "the filtered block rides the diagnostic stream: {diagnostics}"
    );
}

/// Every non-table mode ignores the diagnostic writer: its plan payload
/// (when it carries one) stays inside its own output on the data writer.
#[test]
fn non_table_modes_never_touch_the_diagnostic_writer() {
    let cols = vec!["a".to_string()];
    let rows = vec![vec![CypherValue::Int(1)]];
    for mode in [
        Mode::Csv,
        Mode::Json,
        Mode::Jsonl,
        Mode::Envelope,
        Mode::Markdown,
        Mode::Cypher,
        Mode::Toon,
    ] {
        let mut data = Vec::new();
        let mut diagnostics = Vec::new();
        let column_types = vec![wire::column_type_name(&CypherValue::Int(1)).to_string()];
        Formatter::new(mode.clone())
            .format_with(
                &mut data,
                &mut diagnostics,
                &cols,
                &ordered_rows(&cols, &rows),
                StatementMeta {
                    column_types: &column_types,
                    row_cap: None,
                    plan: None,
                    timings: None,
                },
            )
            .unwrap();
        assert!(!data.is_empty(), "{mode:?} writes its output: {data:?}");
        assert!(
            diagnostics.is_empty(),
            "{mode:?} ignores the diagnostic writer: {diagnostics:?}"
        );
    }
}

#[test]
fn markdown_renders_the_complete_notification_list() {
    let cols = vec!["a".to_string()];
    let rows = vec![vec![CypherValue::Int(1)]];
    let notifications = notifications_fixture();
    let plan = plan_fixture();
    let out = render_plan_notified(Mode::Markdown, &cols, &rows, &plan, &[], &notifications);
    let (table, block) = out
        .split_once("\nNotifications:\n")
        .unwrap_or_else(|| panic!("markdown table then block: {out}"));
    assert!(
        table.starts_with("| a |") && table.ends_with("| 1 |"),
        "the rows render first: {out}"
    );
    assert!(block.contains("WARNING 01N50"), "{out}");
    assert!(block.contains("Id 2: INFORMATION 03C91"), "{out}");
    // Markdown renders no plan table, so the plan-warning kind has no other
    // rendering and must appear in the block.
    assert!(block.contains("CartesianProduct"), "{out}");
}

/// A markdown result whose whole list is a plan-warning kind still renders
/// it: markdown never prints the plan table, so the table's filter would
/// otherwise drop the only copy.
#[test]
fn markdown_renders_a_plan_warning_without_a_plan_table() {
    let cols = vec!["a".to_string()];
    let rows = vec![vec![CypherValue::Int(1)]];
    let plan = plan_fixture();
    let notifications = [Notification::new(
        NotificationKind::CartesianProduct,
        "the MATCH patterns share no join anchor",
    )
    .with_operator_id(1)];
    let out = render_plan_notified(Mode::Markdown, &cols, &rows, &plan, &[], &notifications);
    assert!(
        out.contains("Notifications:\n"),
        "the list is not filtered against an unrendered plan table: {out}"
    );
    assert!(
        out.contains("Neo.ClientNotification.Statement.CartesianProduct"),
        "the only copy of the warning must render: {out}"
    );
    assert!(
        !out.contains("Warnings:"),
        "markdown has no plan table: {out}"
    );
}

#[test]
fn toon_renders_a_notifications_section_beside_plan() {
    let plan = plan_fixture();
    let notifications = notifications_fixture();
    let out = render_explain(Mode::Toon, &plan, &notifications);
    let (plan_section, notification_section) = out
        .split_once("\nnotifications:\n")
        .unwrap_or_else(|| panic!("plan then notifications sections: {out}"));
    assert!(plan_section.starts_with("plan:\n  {"), "{out}");
    let list: serde_json::Value = serde_json::from_str(notification_section.trim()).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 3, "complete list: {out}");
    assert_eq!(
        list[0]["code"],
        "Neo.ClientNotification.Statement.CartesianProduct"
    );
}

#[test]
fn toon_keeps_the_empty_notifications_section_beside_plan() {
    let plan = plan_fixture();
    let out = render_explain(Mode::Toon, &plan, &[]);
    assert!(out.ends_with("notifications:\n  []\n"), "{out}");
}

#[test]
fn json_carries_the_complete_list_in_the_plan_object() {
    // ADR-0363 decision 9: json keeps its plan convention — one own object
    // — so the notification list rides it instead of reshaping row arrays.
    let plan = plan_fixture();
    let notifications = notifications_fixture();
    let out = render_explain(Mode::Json, &plan, &notifications);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["plan"]["operatorType"], "ProduceResults");
    let list = json[relate_model::wire::notification_keys::NOTIFICATIONS]
        .as_array()
        .unwrap();
    assert_eq!(
        list.len(),
        3,
        "complete list, no text-mode filtering: {out}"
    );
    assert_eq!(list[1]["description"], "Person");
    // The EXPLAIN text rows are still absent: the plan object is the output.
    assert!(json.get("rows").is_none(), "{out}");
}

#[test]
fn jsonl_carries_the_complete_list_in_the_plan_object() {
    let plan = plan_fixture();
    let notifications = notifications_fixture();
    let out = render_explain(Mode::Jsonl, &plan, &notifications);
    assert_eq!(out.lines().count(), 1, "one object, one line: {out}");
    let json: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(json["plan"]["operatorType"], "ProduceResults");
    assert_eq!(
        json[relate_model::wire::notification_keys::NOTIFICATIONS]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn json_row_shapes_are_untouched_by_notifications() {
    // A plain result stays a bare row array (json) / one row object per
    // line (jsonl): no notifications wrapper leaks into the row shape.
    let cols = vec!["a".to_string()];
    let rows = vec![vec![CypherValue::Int(1)]];
    let json = render(Mode::Json, &cols, &rows);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, serde_json::json!([{"a": 1}]));
    assert!(!json.contains("notifications"), "{json}");
    let jsonl = render(Mode::Jsonl, &cols, &rows);
    assert_eq!(jsonl, "{\"a\":1}\n");
}

#[test]
fn csv_and_cypher_render_rows_only() {
    // ADR-0363 decision 8: csv and cypher carry no notification rendering;
    // a non-empty list beside a plan changes neither output.
    let cols = vec!["a".to_string()];
    let rows = vec![vec![CypherValue::Int(1)]];
    let plan = plan_fixture();
    let notifications = notifications_fixture();
    for (mode, expected) in [(Mode::Csv, "a\n1\n"), (Mode::Cypher, "CREATE ({a: 1})\n")] {
        let out = render_plan_notified(mode.clone(), &cols, &rows, &plan, &[], &notifications);
        assert_eq!(out, expected, "{mode:?} renders rows only: {out}");
        assert!(!out.contains("Notification"), "{mode:?}: {out}");
        assert!(!out.contains("Remedy"), "{mode:?}: {out}");
    }
}
