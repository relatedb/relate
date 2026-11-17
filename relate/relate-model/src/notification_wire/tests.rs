use super::*;
use serde_json::json;

/// One vocabulary row as the pin names it: every cell is a named field,
/// and severity and category are typed, so a transposed cell fails to
/// compile or to match instead of passing as a positional string.
#[derive(Debug, PartialEq, Eq)]
struct Cells {
    code: &'static str,
    gql_status: &'static str,
    severity: NotificationSeverity,
    category: NotificationCategory,
    title: &'static str,
    remedy: &'static str,
}

impl Cells {
    /// The cells a kind reports through its own public accessors, so the
    /// pin exercises `row()`'s variant-to-row mapping.
    fn of(kind: NotificationKind) -> Cells {
        Cells {
            code: kind.code(),
            gql_status: kind.gql_status(),
            severity: kind.severity(),
            category: kind.category(),
            title: kind.title(),
            remedy: kind.remedy(),
        }
    }
}

/// Every kind paired with the ADR-0363 table row it must reproduce. A drift
/// in any cell fails here. The kinds come from the vocabulary macro's own
/// `ALL` listing, so a kind added to the table without an authored
/// expectation below fails the coverage assertion.
#[test]
fn vocabulary_matches_the_adr_table_row_for_row() {
    let expected: Vec<(NotificationKind, Cells)> = vec![
        (
            NotificationKind::CartesianProduct,
            Cells {
                code: "Neo.ClientNotification.Statement.CartesianProduct",
                gql_status: "03N90",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Cartesian product",
                remedy: "Give the patterns a join anchor: reuse a node variable across them, or add a \
                 pattern that relates their endpoints; otherwise run them as separate \
                 queries.",
            },
        ),
        (
            NotificationKind::UnboundedVariableLengthPattern,
            Cells {
                code: "Neo.ClientNotification.Statement.UnboundedVariableLengthPattern",
                gql_status: "03N91",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Unbounded variable-length pattern",
                remedy: "Bound the expansion (`[:R*1..5]`) where arbitrary length is not required, or \
                 add a selective endpoint/label predicate and a `LIMIT`.",
            },
        ),
        (
            NotificationKind::AllNodesScan,
            Cells {
                code: "Relate.Notification.Statement.AllNodesScan",
                gql_status: "03C90",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "All nodes scan",
                remedy: "Name the label the scan intends (`MATCH (n:<Label>)`); on the index's label \
                 the scan becomes an index-backed lookup.",
            },
        ),
        (
            NotificationKind::MissingIndex,
            Cells {
                code: "Relate.Notification.Statement.MissingIndex",
                gql_status: "03C91",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Missing index",
                remedy: "`CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)` — the \
                 exact runnable statement, with the name derived deterministically from label \
                 and property.",
            },
        ),
        (
            NotificationKind::EagerAggregation,
            Cells {
                code: "Relate.Notification.Statement.EagerAggregation",
                gql_status: "03C92",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Eager aggregation",
                remedy: "Reduce the rows reaching the aggregation with a selective label/property \
                 predicate or a `LIMIT` before it; profile again to confirm the buffered input \
                 shrank.",
            },
        ),
        (
            NotificationKind::EstimationMiss,
            Cells {
                code: "Relate.Notification.Statement.EstimationMiss",
                gql_status: "03C93",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Estimation miss",
                remedy: "Rework the operator's input so the estimate can see the filter (add the \
                 selective predicate at the scan, or reorder the join); the estimate is a \
                 planner assumption, not a runtime fact.",
            },
        ),
        (
            NotificationKind::LabelDisjunctionScan,
            Cells {
                code: "Relate.Notification.Statement.LabelDisjunctionScan",
                gql_status: "03C94",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Label disjunction scan",
                remedy: "Create a range index on each disjunct label's filtered property: the \
                 disjunction then serves as per-label keyed lookups instead of per-label \
                 scans.",
            },
        ),
        (
            NotificationKind::UnknownLabelWarning,
            Cells {
                code: "Neo.ClientNotification.Statement.UnknownLabelWarning",
                gql_status: "01N50",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Unrecognized,
                title: "Unknown label",
                remedy: "Check the spelling; the label exists after the first write that creates it, \
                 and no index or constraint uses the unknown name.",
            },
        ),
        (
            NotificationKind::UnknownRelationshipTypeWarning,
            Cells {
                code: "Neo.ClientNotification.Statement.UnknownRelationshipTypeWarning",
                gql_status: "01N51",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Unrecognized,
                title: "Unknown relationship type",
                remedy: "Check the spelling; the type exists after the first write that creates it.",
            },
        ),
        (
            NotificationKind::UnknownPropertyKeyWarning,
            Cells {
                code: "Neo.ClientNotification.Statement.UnknownPropertyKeyWarning",
                gql_status: "01N52",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Unrecognized,
                title: "Unknown property key",
                remedy: "Check the spelling; the key exists after the first write that sets it.",
            },
        ),
        (
            NotificationKind::DeprecatedFunction,
            Cells {
                code: "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                gql_status: "01N01",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Deprecation,
                title: "Deprecated function",
                remedy: "Replace the call with the catalog's `deprecated_by` (`id(n)` → \
                 `elementId(n)`).",
            },
        ),
        (
            NotificationKind::DeprecatedProcedure,
            Cells {
                code: "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                gql_status: "01N01",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Deprecation,
                title: "Deprecated procedure",
                remedy: "Use the registry's `deprecated_by` replacement instead; the replacement is \
                 the remedy.",
            },
        ),
        (
            NotificationKind::CallSubqueryWithoutScope,
            Cells {
                code: "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                gql_status: "01N00",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Deprecation,
                title: "CALL subquery without scope",
                remedy: "Add the scope clause the body imports: `CALL (n) { … }`, or `CALL () { … }` when \
                 it imports nothing.",
            },
        ),
        (
            NotificationKind::MergeSelfReference,
            Cells {
                code: "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                gql_status: "01N00",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Deprecation,
                title: "Merge self-reference",
                remedy: "Compute the property into a separate variable before the MERGE, or set it with \
                 ON CREATE SET after the merge.",
            },
        ),
        (
            NotificationKind::AmbiguousSubclauseReference,
            Cells {
                code: "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                gql_status: "01N01",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Deprecation,
                title: "Deprecated ambiguous subclause reference",
                remedy: "Reference the projection item by an alias, or use the `GROUP BY` clause.",
            },
        ),
        (
            NotificationKind::ComplexSubclauseReference,
            Cells {
                code: "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                gql_status: "01N01",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Deprecation,
                title: "Deprecated complex subclause reference",
                remedy: "Reference the projection item expression by an alias.",
            },
        ),
        (
            NotificationKind::CreateSelfReference,
            Cells {
                code: "Neo.ClientNotification.Statement.FeatureDeprecationWarning",
                gql_status: "01N00",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Deprecation,
                title: "Create self-reference",
                remedy: "Create the entity in a separate `CREATE` clause before referencing it, or set \
                 the property afterwards.",
            },
        ),
        (
            NotificationKind::SubqueryVariableShadowing,
            Cells {
                code: "Neo.ClientNotification.Statement.SubqueryVariableShadowing",
                gql_status: "03N60",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Unrecognized,
                title: "Subquery variable shadowing",
                remedy: "Import the outer variable with `CALL (name) { … }`, or rename the inner one.",
            },
        ),
        (
            NotificationKind::DynamicProperty,
            Cells {
                code: "Neo.ClientNotification.Statement.DynamicProperty",
                gql_status: "03N95",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Queries using dynamic properties will use neither index seeks nor \
                 index scans for those properties",
                remedy: "Use a static property so index lookups stay possible.",
            },
        ),
        (
            NotificationKind::UnsatisfiableRelationshipTypeExpression,
            Cells {
                code: "Neo.ClientNotification.Statement.UnsatisfiableRelationshipTypeExpression",
                gql_status: "01N61",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Unrecognized,
                title: "Unsatisfiable relationship type expression",
                remedy: "Request one type or a disjunction of types; a relationship has exactly one type.",
            },
        ),
        (
            NotificationKind::RepeatedRelationshipVariable,
            Cells {
                code: "Neo.ClientNotification.Statement.RepeatedRelationshipVariable",
                gql_status: "01N63",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Unrecognized,
                title: "Repeated relationship pattern variable",
                remedy: "Use a distinct variable per relationship position; a relationship cannot be \
                 traversed twice in one pattern.",
            },
        ),
        (
            NotificationKind::SearchIdentifierShadowingVariable,
            Cells {
                code: "Neo.ClientNotification.Statement.IdentifierShadowingVariable",
                gql_status: "03N63",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Unrecognized,
                title: "Identifier shadowing variable",
                remedy: "Rename the index or the variable; the index name is taken literally.",
            },
        ),
        (
            NotificationKind::ExhaustiveShortestPath,
            Cells {
                code: "Neo.ClientNotification.Statement.ExhaustiveShortestPath",
                gql_status: "03N92",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Exhaustive shortest path",
                remedy: "Separate the existential predicate from the `MATCH` with `WITH` where possible.",
            },
        ),
        (
            NotificationKind::SchemaIndexOrConstraintAlreadyExists,
            Cells {
                code: "Neo.ClientNotification.Schema.IndexOrConstraintAlreadyExists",
                gql_status: "00NA0",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Schema,
                title: "Index or constraint already exists",
                remedy: "Drop the existing index or constraint first, or use a different name or schema.",
            },
        ),
        (
            NotificationKind::SchemaIndexOrConstraintDoesNotExist,
            Cells {
                code: "Neo.ClientNotification.Schema.IndexOrConstraintDoesNotExist",
                gql_status: "00NA1",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Schema,
                title: "Index or constraint does not exist",
                remedy: "Check the name; `IF EXISTS` makes a missing index or constraint a no-op.",
            },
        ),
        (
            NotificationKind::SchemaVectorIndexDimensionsNotSpecified,
            Cells {
                code: "Neo.ClientNotification.Schema.VectorIndexDimensionsNotSpecified",
                gql_status: "00NA2",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Schema,
                title: "Vector index dimensions not specified",
                remedy: "Set `vector.dimensions` in `OPTIONS { indexConfig: { … } }` when creating the index.",
            },
        ),
        (
            NotificationKind::HintedIndexNotFound,
            Cells {
                code: "Neo.ClientNotification.Schema.HintedIndexNotFound",
                gql_status: "01N31",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Hint,
                title: "The request (directly or indirectly) referred to an index that does not exist.",
                remedy: "Create the index, or drop the hint.",
            },
        ),
        (
            NotificationKind::JoinHintUnfulfillable,
            Cells {
                code: "Neo.ClientNotification.Statement.JoinHintUnfulfillableWarning",
                gql_status: "01N30",
                severity: NotificationSeverity::Warning,
                category: NotificationCategory::Hint,
                title: "The database was unable to plan a hinted join.",
                remedy: "Try a different join key or restructure the query.",
            },
        ),
        (
            NotificationKind::RedundantOptionalProcedure,
            Cells {
                code: "Neo.ClientNotification.Statement.RedundantOptionalProcedure",
                gql_status: "03N61",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Redundant optional procedure",
                remedy: "Drop `OPTIONAL`: the procedure declares no return columns, so the call yields the same single row either way.",
            },
        ),
        (
            NotificationKind::RedundantOptionalSubquery,
            Cells {
                code: "Neo.ClientNotification.Statement.RedundantOptionalSubquery",
                gql_status: "03N62",
                severity: NotificationSeverity::Information,
                category: NotificationCategory::Performance,
                title: "Redundant optional subquery",
                remedy: "Drop `OPTIONAL`: the subquery projects no columns, so the call yields the same single row either way.",
            },
        ),
    ];
    for (kind, cells) in &expected {
        assert_eq!(&Cells::of(*kind), cells, "{kind:?}");
    }
    // Set equality both ways: every kind pinned, none pinned twice.
    let pinned: std::collections::HashSet<NotificationKind> =
        expected.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        pinned.len(),
        expected.len(),
        "a kind is pinned more than once"
    );
    let all: std::collections::HashSet<NotificationKind> =
        NotificationKind::ALL.iter().copied().collect();
    assert_eq!(pinned, all, "the pin must cover exactly the vocabulary");
}

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
        "the scan matches every node; add a label to narrow it",
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
            "description": "the scan matches every node; add a label to narrow it",
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
