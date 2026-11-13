import CRelate

// MARK: - Result materialization (ADR-0502 §1)

extension QueryResult {
    /// Reads one good result into a `QueryResult` before it is destroyed:
    /// cells through the typed walker, the metadata documents from their
    /// JSON. The boundary decode lives here, not in the model file,
    /// mirroring `CypherValue+Walker.swift`.
    static func materialize(_ result: OpaquePointer) throws(RelateError) -> QueryResult {
        let columnCount = Int(relate_column_count(result))
        let rowCount = Int(relate_row_count(result))
        var columns: [String] = []
        columns.reserveCapacity(columnCount)
        for col in 0..<columnCount {
            columns.append(try required(relate_column_name(result, Int64(col)), "column name \(col)"))
        }
        var rows: [[CypherValue]] = []
        rows.reserveCapacity(rowCount)
        for row in 0..<rowCount {
            var values: [CypherValue] = []
            values.reserveCapacity(columnCount)
            for col in 0..<columnCount {
                values.append(
                    try CypherValue.decode(relate_value_at(result, Int64(row), Int64(col))))
            }
            rows.append(values)
        }
        let queryTypeName = try required(relate_result_query_type(result), "query type")
        let queryType = try QueryType.decode(queryTypeName)
        let namespaceName = try required(relate_result_namespace(result), "namespace")
        let namespace = try DatabaseNamespace.decode(namespaceName)
        var plan: PlanDocument? = nil
        if let json = relate_result_plan_json(result) {
            plan = try decodeMetadata(
                PlanDocument.self, try required(json, "the plan document"), what: "plan")
        }
        var bookmark: String? = nil
        if let text = relate_result_bookmark(result) {
            bookmark = try required(text, "the result's bookmark")
        }
        return QueryResult(
            columns: columns,
            rows: rows,
            bookmark: bookmark,
            queryType: queryType,
            namespace: namespace,
            stats: try decodeMetadata(
                QueryStats.self, try required(relate_result_stats(result), "stats"), what: "stats"),
            notifications: try decodeMetadata(
                [QueryNotification].self,
                try required(relate_result_notifications(result), "notifications"),
                what: "notifications"),
            plan: plan,
            elapsedMs: relate_result_elapsed_ms(result),
            parseMs: relate_result_parse_ms(result),
            planMs: relate_result_plan_ms(result),
            executeMs: relate_result_execute_ms(result))
    }
}

// The closed metadata vocabularies (query type and namespace here; severity and
// category decode through `decodeWire`): the C ABI's contract is the listed
// values, so anything else is a contract violation, never a lenient string.
extension QueryType {
    /// Types the `relate_result_query_type` accessor's string (ADR-0162).
    static func decode(_ raw: String) throws(RelateError) -> QueryType {
        try closedWire(QueryType.self, raw, what: "query type")
    }
}

extension DatabaseNamespace {
    /// Types the `relate_result_namespace` accessor's string (ADR-0117).
    static func decode(_ raw: String) throws(RelateError) -> DatabaseNamespace {
        try closedWire(DatabaseNamespace.self, raw, what: "namespace")
    }
}
