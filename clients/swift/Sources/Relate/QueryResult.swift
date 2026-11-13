import Foundation

/// A fully materialized statement result. Every field is copied out of the
/// C result before it is destroyed, so a `QueryResult` is a plain value that
/// may cross actors.
///
/// A result is also a collection of ``Record``s (`for record in result`,
/// `result.first`, `result.map { … }`). It is `Equatable` over every field,
/// timings included, so two runs of one statement rarely compare equal —
/// compare `rows` to compare what a statement returned.
public struct QueryResult: Sendable, Equatable {
    /// The column names with their name → position lookup, shared by every
    /// `Record` this result vends.
    let columnIndex: ColumnIndex
    public var columns: [String] { columnIndex.names }
    public let rows: [[CypherValue]]
    /// The auto-commit statement's bookmark (`"relate:<n>"`, the
    /// engine's monotonic commit sequence, ADR-0502 §7): `nil` when the
    /// statement committed nothing — a read, an empty write pipeline, an
    /// EXPLAIN, a statement inside an explicit transaction (its commit's
    /// return is the bookmark) or an error result.
    public let bookmark: String?
    public let queryType: QueryType
    /// The resolved namespace (ADR-0117): the `USE` target, else the session's.
    public let namespace: DatabaseNamespace
    public let stats: QueryStats
    /// Always present: empty on a plain run, populated by EXPLAIN / PROFILE.
    public let notifications: [QueryNotification]
    /// The `relate.plan/v1` document (ADR-0381); `nil` on a plain run.
    public let plan: PlanDocument?
    /// Engine-reported parse + plan + execute wall clock (ADR-0221).
    public let elapsedMs: Double
    /// The phase split of `elapsedMs` (ADR-0491): the front-end parse and
    /// lowering window. `parseMs + planMs + executeMs` never exceeds `elapsedMs`.
    public let parseMs: Double
    /// The phase split of `elapsedMs` (ADR-0491): the binding, logical
    /// planning and admission classification window.
    public let planMs: Double
    /// The phase split of `elapsedMs` (ADR-0491): the dispatch window — or
    /// the EXPLAIN plan render for an EXPLAIN statement.
    public let executeMs: Double

}

// MARK: - Reading rows out of a result

extension QueryResult: RandomAccessCollection {
    public var startIndex: Int { rows.startIndex }
    public var endIndex: Int { rows.endIndex }

    /// The record at a position.
    public subscript(position: Int) -> Record {
        Record(index: columnIndex, values: rows[position])
    }

    /// The result's rows as ``Record``s: each readable by position, by
    /// column name, or typed. Iterating the result yields the same records.
    /// Duplicate column names are legal Cypher (`RETURN 1 AS a, 2 AS a`);
    /// the last column's value is the one the name resolves to.
    ///
    /// For a record as a plain dictionary, use ``Record/dictionary``.
    public var records: [Record] { Array(self) }

    /// The one record of a one-row result. Throws
    /// ``RelateError/unexpectedResult(_:)`` for none or several.
    public func single() throws(RelateError) -> Record {
        guard rows.count == 1 else {
            throw shapeMismatch(expected: "exactly one record", found: "\(rows.count) records")
        }
        return self[0]
    }

    /// The single value of a one-row, one-column result. Throws
    /// ``RelateError/unexpectedResult(_:)`` for any other shape.
    public func scalar() throws(RelateError) -> CypherValue {
        try scalar(as: CypherValue.self)
    }

    /// The single value of a one-row, one-column result, as a Swift type.
    public func scalar<T: CypherValueConvertible>(as type: T.Type) throws(RelateError) -> T {
        let record = try single()
        guard record.count == 1 else {
            throw shapeMismatch(expected: "exactly one column", found: "\(record.count) columns")
        }
        return try record.get(0, as: type)
    }
}

/// The plan-time statement classification (ADR-0162, ADR-0332).
public enum QueryType: String, Sendable, Hashable {
    case read = "r"
    case write = "w"
    case readWrite = "rw"
    case schema = "s"
}

extension QueryType: CustomStringConvertible {
    public var description: String {
        switch self {
        case .read: "read"
        case .write: "write"
        case .readWrite: "read-write"
        case .schema: "schema"
        }
    }
}

/// The statement's resolved namespace (ADR-0117): one of the three fixed
/// databases. Any other value is an ABI contract violation at decode.
public enum DatabaseNamespace: String, Sendable, Hashable {
    case neo4j
    case system
    case systemOntologies = "system_ontologies"
}

/// The statement's mutation counters (ADR-0216).
public struct QueryStats: Sendable, Hashable, Decodable {
    public let nodesCreated: Int
    public let nodesDeleted: Int
    public let relationshipsCreated: Int
    public let relationshipsDeleted: Int
    public let propertiesSet: Int
    public let labelsAdded: Int
    public let labelsRemoved: Int
    public let indexesAdded: Int
    public let indexesRemoved: Int
    public let constraintsAdded: Int
    public let constraintsRemoved: Int
}

extension QueryStats {
    static let zero = QueryStats(
        nodesCreated: 0, nodesDeleted: 0, relationshipsCreated: 0, relationshipsDeleted: 0,
        propertiesSet: 0, labelsAdded: 0, labelsRemoved: 0, indexesAdded: 0, indexesRemoved: 0,
        constraintsAdded: 0, constraintsRemoved: 0)

    static func + (lhs: QueryStats, rhs: QueryStats) -> QueryStats {
        QueryStats(
            nodesCreated: lhs.nodesCreated + rhs.nodesCreated,
            nodesDeleted: lhs.nodesDeleted + rhs.nodesDeleted,
            relationshipsCreated: lhs.relationshipsCreated + rhs.relationshipsCreated,
            relationshipsDeleted: lhs.relationshipsDeleted + rhs.relationshipsDeleted,
            propertiesSet: lhs.propertiesSet + rhs.propertiesSet,
            labelsAdded: lhs.labelsAdded + rhs.labelsAdded,
            labelsRemoved: lhs.labelsRemoved + rhs.labelsRemoved,
            indexesAdded: lhs.indexesAdded + rhs.indexesAdded,
            indexesRemoved: lhs.indexesRemoved + rhs.indexesRemoved,
            constraintsAdded: lhs.constraintsAdded + rhs.constraintsAdded,
            constraintsRemoved: lhs.constraintsRemoved + rhs.constraintsRemoved)
    }
}

/// A notification's severity (ADR-0363): a closed vocabulary. A wire value
/// outside it is an ABI contract violation at decode, never a lenient string.
public enum NotificationSeverity: String, Sendable, Hashable, Decodable {
    case information = "INFORMATION"
    case warning = "WARNING"

    public init(from decoder: any Decoder) throws {
        self = try decodeWire(Self.self, from: decoder)
    }
}

/// A notification's category (ADR-0363): a closed vocabulary, refused like
/// `NotificationSeverity` when the wire value is outside it.
public enum NotificationCategory: String, Sendable, Hashable, Decodable {
    case performance = "PERFORMANCE"
    case unrecognized = "UNRECOGNIZED"
    case deprecation = "DEPRECATION"
    case schema = "SCHEMA"
    case hint = "HINT"

    public init(from decoder: any Decoder) throws {
        self = try decodeWire(Self.self, from: decoder)
    }
}

/// The former spelling of ``NotificationSeverity``.
@available(*, deprecated, renamed: "NotificationSeverity")
public typealias Severity = NotificationSeverity

/// The former spelling of ``NotificationCategory``.
@available(*, deprecated, renamed: "NotificationCategory")
public typealias Category = NotificationCategory

/// One query notification (ADR-0363) in the engine-native shape. `severity`
/// and `category` are the closed `NotificationSeverity` and `NotificationCategory` enums: decoding
/// refuses a missing, null or unknown value as an ABI contract violation
/// naming the key and the (sanitized) offending value.
public struct QueryNotification: Sendable, Hashable, Decodable {
    public let code: String
    public let gqlStatus: String
    public let severity: NotificationSeverity
    public let category: NotificationCategory
    public let title: String
    public let description: String
    public let remedy: String
    /// The raising operator's plan id on operator-scoped kinds.
    public let operatorId: Int64?
}

/// A statement's structured plan, the `relate.plan/v1` document (ADR-0381).
public struct PlanDocument: Sendable, Hashable, Decodable {
    public let schema: String
    public let profiled: Bool
    public let statement: String?
    public let root: PlanNode
}

public struct PlanNode: Sendable, Hashable, Decodable {
    public let `operator`: String
    public let details: String
    public let identifiers: [String]
    public let estimatedRows: Int64?
    /// PROFILE metrics; `nil` on an EXPLAIN tree.
    public let actual: PlanActual?
    public let children: [PlanNode]
    public let warnings: [QueryNotification]
}

public struct PlanActual: Sendable, Hashable, Decodable {
    public let rows: Int64
    public let timeMs: Double
    public let dbHits: Int64
    public let memoryBytes: Int64
    public let hops: Int64
    public let neighborWalks: Int64
    public let nodeReads: Int64
    /// The walk's chosen start element (ADR-0375).
    public let start: PlanStart?
}

public struct PlanStart: Sendable, Hashable, Decodable {
    public let element: Int64
    public let backward: Bool
    public let reason: String
}
