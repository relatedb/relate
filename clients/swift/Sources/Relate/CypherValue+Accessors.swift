// MARK: - Accessors

extension CypherValue {
    /// This value's kind, per the ABI's structural discrimination.
    public var kind: CypherValueKind {
        switch self {
        case .null: .null
        case .bool: .bool
        case .int: .int
        case .float: .float
        case .string: .string
        case .list: .list
        case .map: .map
        case .node: .node
        case .relationship: .relationship
        case .path: .path
        case .point: .point
        case .date: .date
        case .time: .time
        case .localTime: .localTime
        case .dateTime: .dateTime
        case .localDateTime: .localDateTime
        case .duration: .duration
        case .vector: .vector
        }
    }

    public var isNull: Bool { self == .null }
    public var boolValue: Bool? { if case .bool(let v) = self { v } else { nil } }
    public var intValue: Int64? { if case .int(let v) = self { v } else { nil } }
    /// The value as a `Double`: a float, or an integer widened to one.
    public var doubleValue: Double? {
        switch self {
        case .float(let v): v
        case .int(let v): Double(v)
        default: nil
        }
    }
    public var stringValue: String? { if case .string(let v) = self { v } else { nil } }
    public var listValue: [CypherValue]? { if case .list(let v) = self { v } else { nil } }
    public var mapValue: [String: CypherValue]? { if case .map(let v) = self { v } else { nil } }
    public var nodeValue: Node? { if case .node(let v) = self { v } else { nil } }
    public var relationshipValue: Relationship? {
        if case .relationship(let v) = self { v } else { nil }
    }
    public var pathValue: Path? { if case .path(let v) = self { v } else { nil } }
    public var pointValue: Point? { if case .point(let v) = self { v } else { nil } }
    public var dateValue: Date? { if case .date(let v) = self { v } else { nil } }
    public var timeValue: Time? { if case .time(let v) = self { v } else { nil } }
    public var localTimeValue: LocalTime? { if case .localTime(let v) = self { v } else { nil } }
    public var dateTimeValue: DateTime? { if case .dateTime(let v) = self { v } else { nil } }
    public var localDateTimeValue: LocalDateTime? {
        if case .localDateTime(let v) = self { v } else { nil }
    }
    public var durationValue: Duration? { if case .duration(let v) = self { v } else { nil } }
    public var vectorValue: Vector? { if case .vector(let v) = self { v } else { nil } }

    /// Map key or node/relationship property lookup; `nil` for any other value.
    public subscript(key: String) -> CypherValue? { entries?[key] }

    /// What a keyed read sees: a map's entries, or a node's or
    /// relationship's properties; `nil` for any other value.
    var entries: [String: CypherValue]? {
        switch self {
        case .map(let m): m
        case .node(let n): n.properties
        case .relationship(let r): r.properties
        default: nil
        }
    }

    /// What a sequence read sees: a list's items, or a vector as the list
    /// of its coordinates; `nil` for any other value.
    var sequenceItems: [CypherValue]? {
        switch self {
        case .list(let values): values
        case .vector(.floats(_, let values)): values.map(CypherValue.float)
        case .vector(.ints(_, let values)): values.map(CypherValue.int)
        default: nil
        }
    }
}
