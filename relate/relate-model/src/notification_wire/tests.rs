use super::*;
use serde_json::json;

/// Every kind, in table order, paired with the ADR-0363 table row it must
/// reproduce: code, GQLSTATUS, severity, category, title, remedy. A drift
/// in any cell fails here. Each row is read through the kind's own public
/// accessors, so the pin exercises `row()`'s variant→row mapping — a
/// mis-indexed or duplicated arm fails it.
#[test]
fn vocabulary_matches_the_adr_table_row_for_row() {
    // The one enumeration this pin walks. `row()`'s exhaustive match
    // forces every new variant to claim a row; this list forces that row
    // to carry an authored expectation below (a kind missing here also
    // misses its expectation, so the lengths differ and the pin fails).
    const ALL_KINDS: [NotificationKind; 27] = [
        NotificationKind::CartesianProduct,
        NotificationKind::UnboundedVariableLengthPattern,
        NotificationKind::AllNodesScan,
        NotificationKind::MissingIndex,
        NotificationKind::EagerAggregation,
        NotificationKind::EstimationMiss,
        NotificationKind::UnknownLabelWarning,
        NotificationKind::UnknownRelationshipTypeWarning,
        NotificationKind::UnknownPropertyKeyWarning,
        NotificationKind::DeprecatedFunction,
        NotificationKind::DeprecatedProcedure,
        NotificationKind::CallSubqueryWithoutScope,
        NotificationKind::MergeSelfReference,
        NotificationKind::AmbiguousSubclauseReference,
        NotificationKind::ComplexSubclauseReference,
        NotificationKind::CreateSelfReference,
        NotificationKind::SubqueryVariableShadowing,
        NotificationKind::DynamicProperty,
        NotificationKind::UnsatisfiableRelationshipTypeExpression,
        NotificationKind::RepeatedRelationshipVariable,
        NotificationKind::SearchIdentifierShadowingVariable,
        NotificationKind::ExhaustiveShortestPath,
        NotificationKind::SchemaIndexOrConstraintAlreadyExists,
        NotificationKind::SchemaIndexOrConstraintDoesNotExist,
        NotificationKind::SchemaVectorIndexDimensionsNotSpecified,
        NotificationKind::HintedIndexNotFound,
        NotificationKind::JoinHintUnfulfillable,
    ];
    let rows: Vec<(&str, &str, &str, &str, &str, &str)> = ALL_KINDS
        .iter()
        .map(|kind| {
            (
                kind.code(),
                kind.gql_status(),
                kind.severity().as_str(),
                kind.category().as_str(),
                kind.title(),
                kind.remedy(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "Neo.ClientNotification.Statement.CartesianProduct",
                "03N90",
                "INFORMATION",
                "PERFORMANCE",
                "Cartesian product",
                "Give the patterns a join anchor: reuse a node variable across them, or add a \
                 pattern that relates their endpoints; otherwise run them as separate \
                 queries.",
            ),
            (
                "Neo.ClientNotification.Statement.UnboundedVariableLengthPattern",
                "03N91",
                "INFORMATION",
                "PERFORMANCE",
                "Unbounded variable-length pattern",
                "Bound the expansion (`[:R*1..5]`) where arbitrary length is not required, or \
                 add a selective endpoint/label predicate and a `LIMIT`.",
            ),
            (
                "Relate.Notification.Statement.AllNodesScan",
                "03C90",
                "INFORMATION",
                "PERFORMANCE",
                "All nodes scan",
                "Name the label the scan intends (`MATCH (n:<Label>)`); on the index's label \
                 the scan becomes an index-backed lookup.",
            ),
            (
                "Relate.Notification.Statement.MissingIndex",
                "03C91",
                "INFORMATION",
                "PERFORMANCE",
                "Missing index",
                "`CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)` — the \
                 exact runnable statement, with the name derived deterministically from label \
                 and property.",
            ),
            (
                "Relate.Notification.Statement.EagerAggregation",
                "03C92",
                "INFORMATION",
                "PERFORMANCE",
                "Eager aggregation",
                "Reduce the rows reaching the aggregation with a selective label/property \
                 predicate or a `LIMIT` before it; profile again to confirm the buffered input \
                 shrank.",
            ),
            (
                "Relate.Notification.Statement.EstimationMiss",
                "03C93",
                "INFORMATION",
                "PERFORMANCE",
                "Estimation miss",
                "Rework the operator's input so the estimate can see the filter (add the \
                 selective predicate at the scan, or reorder the join); the estimate is a \
                 planner assumption, not a runtime fact.",
            ),
            (
                "Neo.ClientNotification.Statement.UnknownLabelWarning",
                "01N50",
                "WARNING",
                "UNRECOGNIZED",
                "Unknown label",
                "Check the spelling; the label exists after the first write that creates it, \
                 and no index or constraint uses the unknown name.",
            ),
            (
                "Neo.ClientNotification.Statement.UnknownRelationshipTypeWarning",
                "01N51",
                "WARNING",
                "UNRECOGNIZED",
                "Unknown relationship type",
                "Check the spelling; the type exists after the first write that creates it.",
            ),
            (
                "Neo.ClientNotification.Statement.UnknownPropertyKeyWarning",
                "01N52",
                "WARNING",
                "UNRECOGNIZED",
                "Unknown property key",
                "Check the spelling; the key exists after the first write that sets it.",
            ),
            (
                "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                "01N01",
                "WARNING",
                "DEPRECATION",
                "Deprecated function",
                "Replace the call with the catalog's `deprecated_by` (`id(n)` → \
                 `elementId(n)`).",
            ),
            (
                "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                "01N01",
                "WARNING",
                "DEPRECATION",
                "Deprecated procedure",
                "Use the registry's `deprecated_by` replacement instead; the replacement is \
                 the remedy.",
            ),
            (
                "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                "01N00",
                "WARNING",
                "DEPRECATION",
                "CALL subquery without scope",
                "Add the scope clause the body imports: `CALL (n) { … }`, or `CALL () { … }` when \
                 it imports nothing.",
            ),
            (
                "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                "01N00",
                "WARNING",
                "DEPRECATION",
                "Merge self-reference",
                "Compute the property into a separate variable before the MERGE, or set it with \
                 ON CREATE SET after the merge.",
            ),
            (
                "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                "01N01",
                "WARNING",
                "DEPRECATION",
                "Deprecated ambiguous subclause reference",
                "Reference the projection item by an alias, or use the `GROUP BY` clause.",
            ),
            (
                "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                "01N01",
                "WARNING",
                "DEPRECATION",
                "Deprecated complex subclause reference",
                "Reference the projection item expression by an alias.",
            ),
            (
                "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                "01N00",
                "WARNING",
                "DEPRECATION",
                "Create self-reference",
                "Create the entity in a separate `CREATE` clause before referencing it, or set \
                 the property afterwards.",
            ),
            (
                "Neo.ClientNotification.Statement.SubqueryVariableShadowing",
                "03N60",
                "INFORMATION",
                "UNRECOGNIZED",
                "Subquery variable shadowing",
                "Import the outer variable with `CALL (name) { … }`, or rename the inner one.",
            ),
            (
                "Neo.ClientNotification.Statement.DynamicProperty",
                "03N95",
                "INFORMATION",
                "PERFORMANCE",
                "Queries using dynamic properties will use neither index seeks nor \
                 index scans for those properties",
                "Use a static property so index lookups stay possible.",
            ),
            (
                "Neo.ClientNotification.Statement.UnsatisfiableRelationshipTypeExpression",
                "01N61",
                "WARNING",
                "UNRECOGNIZED",
                "Unsatisfiable relationship type expression",
                "Request one type or a disjunction of types; a relationship has exactly one type.",
            ),
            (
                "Neo.ClientNotification.Statement.RepeatedRelationshipVariable",
                "01N63",
                "WARNING",
                "UNRECOGNIZED",
                "Repeated relationship pattern variable",
                "Use a distinct variable per relationship position; a relationship cannot be \
                 traversed twice in one pattern.",
            ),
            (
                "Neo.ClientNotification.Statement.IdentifierShadowingVariable",
                "03N63",
                "INFORMATION",
                "UNRECOGNIZED",
                "Identifier shadowing variable",
                "Rename the index or the variable; the index name is taken literally.",
            ),
            (
                "Neo.ClientNotification.Statement.ExhaustiveShortestPath",
                "03N92",
                "INFORMATION",
                "PERFORMANCE",
                "Exhaustive shortest path",
                "Separate the existential predicate from the `MATCH` with `WITH` where possible.",
            ),
            (
                "Neo.ClientNotification.Schema.IndexOrConstraintAlreadyExists",
                "00NA0",
                "INFORMATION",
                "SCHEMA",
                "Index or constraint already exists",
                "Drop the existing index or constraint first, or use a different name or schema.",
            ),
            (
                "Neo.ClientNotification.Schema.IndexOrConstraintDoesNotExist",
                "00NA1",
                "INFORMATION",
                "SCHEMA",
                "Index or constraint does not exist",
                "Check the name; `IF EXISTS` makes a missing index or constraint a no-op.",
            ),
            (
                "Neo.ClientNotification.Schema.VectorIndexDimensionsNotSpecified",
                "00NA2",
                "INFORMATION",
                "SCHEMA",
                "Vector index dimensions not specified",
                "Set `vector.dimensions` in `OPTIONS { indexConfig: { … } }` when creating the index.",
            ),
            (
                "Neo.ClientNotification.Schema.HintedIndexNotFound",
                "01N31",
                "WARNING",
                "HINT",
                "The request (directly or indirectly) referred to an index that does not exist.",
                "Create the index, or drop the hint.",
            ),
            (
                "Neo.ClientNotification.Statement.JoinHintUnfulfillableWarning",
                "01N30",
                "WARNING",
                "HINT",
                "The database was unable to plan a hinted join.",
                "Try a different join key or restructure the query.",
            ),
        ]
    );
}

/// The Neo4j-shaped entry order is Bolt's five keys — `code`, `title`,
/// `description`, `severity`, `category` — and the list carries exactly
/// those five, read from the one native key table.
#[test]
fn neo4j_entries_use_the_five_native_keys_in_bolt_order() {
    let notification =
        Notification::new(NotificationKind::MissingIndex, "no index on :Person(email)");
    assert_eq!(
        notification_to_neo4j_entries(&notification)
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>(),
        [
            notification_keys::CODE,
            notification_keys::TITLE,
            notification_keys::DESCRIPTION,
            notification_keys::SEVERITY,
            notification_keys::CATEGORY,
        ]
    );
    assert_eq!(
        [
            notification_keys::CODE,
            notification_keys::TITLE,
            notification_keys::DESCRIPTION,
            notification_keys::SEVERITY,
            notification_keys::CATEGORY,
        ],
        ["code", "title", "description", "severity", "category"]
    );
}

/// The typed accessor emits the five ordered pairs — code, title,
/// description, severity, category — with the effective remedy folded
/// into `description` and no native entry. The JSON projection carries
/// the same values.
#[test]
fn neo4j_entries_are_ordered_and_fold_the_remedy() {
    let notification =
        Notification::new(NotificationKind::MissingIndex, "no index on :Person(email)")
            .with_remedy("CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)");
    let entries = notification_to_neo4j_entries(&notification);
    assert_eq!(
        entries,
        vec![
            (
                "code",
                "Relate.Notification.Statement.MissingIndex".to_string(),
            ),
            ("title", "Missing index".to_string()),
            (
                "description",
                "no index on :Person(email) (remedy: CREATE INDEX \
                 relate_Person__email FOR (n:Person) ON (n.email))"
                    .to_string(),
            ),
            ("severity", "INFORMATION".to_string()),
            ("category", "PERFORMANCE".to_string()),
        ]
    );
    let value = notification_to_neo4j_json(&notification);
    for (key, entry_value) in &entries {
        assert_eq!(
            value[*key],
            Value::String(entry_value.clone()),
            "the JSON projection carries the `{key}` entry"
        );
    }
}

/// The native form has exactly the ADR keys — `remedy` included, and
/// `operator_id` only on an operator-scoped record.
#[test]
fn native_wire_form_carries_the_adr_keys() {
    let operator = Notification::new(
        NotificationKind::AllNodesScan,
        "the scan matches every node; consider the property index (:Person(name))",
    )
    .with_operator_id(3);
    assert_eq!(
        notification_to_wire_json(&operator),
        json!({
            "code": "Relate.Notification.Statement.AllNodesScan",
            "gql_status": "03C90",
            "severity": "INFORMATION",
            "category": "PERFORMANCE",
            "title": "All nodes scan",
            "description": "the scan matches every node; consider the property index (:Person(name))",
            "remedy": "Name the label the scan intends (`MATCH (n:<Label>)`); on the index's \
                       label the scan becomes an index-backed lookup.",
            "operator_id": 3,
        })
    );
    // A statement-scoped record carries no `operator_id` key at all.
    let statement = Notification::new(NotificationKind::UnknownLabelWarning, "Person");
    let obj = notification_to_wire_json(&statement);
    assert_eq!(
        obj["code"],
        "Neo.ClientNotification.Statement.UnknownLabelWarning"
    );
    assert_eq!(obj["severity"], "WARNING");
    assert_eq!(obj["category"], "UNRECOGNIZED");
    assert!(
        obj.get("operator_id").is_none(),
        "a statement-scoped notification has no operator association: {obj}"
    );
    assert!(obj.get("remedy").is_some(), "the native form keeps remedy");
}

/// The Neo4j-shaped form has exactly Bolt's five keys, folds the
/// effective remedy into `description`, and emits neither `remedy` nor
/// `position`.
#[test]
fn neo4j_wire_form_folds_the_remedy_and_has_no_position() {
    let notification =
        Notification::new(NotificationKind::MissingIndex, "no index on :Person(email)")
            .with_remedy("CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)");
    let value = notification_to_neo4j_json(&notification);
    assert_eq!(
        value,
        json!({
            "code": "Relate.Notification.Statement.MissingIndex",
            "title": "Missing index",
            "description": "no index on :Person(email) (remedy: CREATE INDEX \
                            relate_Person__email FOR (n:Person) ON (n.email))",
            "severity": "INFORMATION",
            "category": "PERFORMANCE",
        })
    );
    let obj = value
        .as_object()
        .expect("the notification is a JSON object");
    assert_eq!(
        obj.len(),
        5,
        "exactly the Bolt/HTTP keys: {}",
        obj.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    assert!(!obj.contains_key("remedy"), "the remedy is folded");
    assert!(
        !obj.contains_key("position"),
        "no position key ever appears"
    );
    assert!(!obj.contains_key("operator_id"), "no native key leaks");
}

/// An instance remedy overrides the kind's canonical text; without one
/// the kind supplies it.
#[test]
fn instance_remedy_overrides_the_kind_canonical_remedy() {
    let canonical = Notification::new(NotificationKind::DeprecatedFunction, "id()");
    assert_eq!(
        canonical.remedy(),
        NotificationKind::DeprecatedFunction.remedy()
    );
    let specific = canonical.with_remedy("elementId()");
    assert_eq!(specific.remedy(), "elementId()");
}

/// The list serializer emits one object per record, preserving order.
#[test]
fn notification_list_serializes_in_order() {
    let list = [
        Notification::new(NotificationKind::CartesianProduct, "first"),
        Notification::new(NotificationKind::EstimationMiss, "second"),
    ];
    let value = notifications_to_wire_json(&list);
    assert_eq!(value[0]["description"], "first");
    assert_eq!(value[1]["description"], "second");
    assert_eq!(
        value[1]["code"],
        "Relate.Notification.Statement.EstimationMiss"
    );
}
