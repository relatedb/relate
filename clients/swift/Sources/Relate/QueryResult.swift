import Foundation

/// A fully materialized statement result. Every field is copied out of the
/// C result before it is destroyed, so a `QueryResult` is a plain value that
/// may cross actors.
public struct QueryResult: Sendable {
    public let columns: [String]
    public let rows: [[CypherValue]]
    /// The auto-commit statement's bookmark (`"relate:<n>"`, the
    /// engine's monotonic commit sequence, ADR-0502 §7): `nil` when the
    /// statement committed nothing — a read, an empty write pipeline, an
    /// EXPLAIN, a statement inside an explicit transaction (its commit's
    /// return is the bookmark) or an error result.
    public let bookmark: String?
    public let queryType: QueryType
    /// The resolved namespace (ADR-0117): the `USE` target, else the session's.
    public let namespace: Namespace
    public let stats: QueryStats
    /// Always present: empty on a plain run, populated by EXPLAIN / PROFILE.
    public let notifications: [Notification]
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

    /// Rows as column-name → value dictionaries. Duplicate column names
    /// are legal Cypher (`RETURN 1 AS a, 2 AS a`); the last column's value
    /// is the one the name resolves to.
    public var records: [[String: CypherValue]] {
        rows.map { row in Dictionary(zip(columns, row), uniquingKeysWith: { _, last in last }) }
    }

    /// The single value of a one-row, one-column result.
    public func scalar() throws -> CypherValue {
        guard rows.count == 1, columns.count == 1 else {
            throw RelateError.decoding(
                "scalar() needs exactly one row and one column, got \(rows.count)×\(columns.count)")
        }
        return rows[0][0]
    }
}

/// The plan-time statement classification (ADR-0162, ADR-0332).
public enum QueryType: String, Sendable, Hashable {
    case read = "r"
    case write = "w"
    case readWrite = "rw"
    case schema = "s"
}

/// The statement's resolved namespace (ADR-0117): one of the three fixed
/// databases. Any other value is an ABI contract violation at decode.
public enum Namespace: String, Sendable, Hashable {
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

/// A notification's severity (ADR-0363): a closed vocabulary. A wire value
/// outside it is an ABI contract violation at decode, never a lenient string.
public enum Severity: String, Sendable, Hashable, Decodable {
    case information = "INFORMATION"
    case warning = "WARNING"

    public init(from decoder: any Decoder) throws {
        self = try decodeWire(Self.self, from: decoder)
    }
}

/// A notification's category (ADR-0363): a closed vocabulary, refused like
/// `Severity` when the wire value is outside it.
public enum Category: String, Sendable, Hashable, Decodable {
    case performance = "PERFORMANCE"
    case unrecognized = "UNRECOGNIZED"
    case deprecation = "DEPRECATION"
    case schema = "SCHEMA"
    case hint = "HINT"

    public init(from decoder: any Decoder) throws {
        self = try decodeWire(Self.self, from: decoder)
    }
}

/// One query notification (ADR-0363) in the engine-native shape. `severity`
/// and `category` are the closed `Severity` and `Category` enums: decoding
/// refuses a missing, null or unknown value as an ABI contract violation
/// naming the key and the (sanitized) offending value.
public struct Notification: Sendable, Hashable, Decodable {
    public let code: String
    public let gqlStatus: String
    public let severity: Severity
    public let category: Category
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
    public let warnings: [Notification]
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
