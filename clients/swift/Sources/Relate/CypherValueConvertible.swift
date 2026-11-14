// MARK: - Swift values as Cypher values, both ways

/// A Swift type that converts to and from a ``CypherValue``: what a
/// parameter dictionary, an interpolated ``Statement`` and a ``Record``
/// accessor accept in place of a hand-built enum case.
///
/// Conversion from a `CypherValue` is strict. An integer widens to a
/// floating-point type; nothing else converts — a string is never parsed
/// into a number, a null is never a default. A mismatch throws
/// ``RelateError/unexpectedResult(_:)`` naming the expected type and the
/// kind found.
public protocol CypherValueConvertible {
    /// This value as a Cypher value. Throws
    /// ``RelateError/invalidParameter(_:)`` for a value Cypher cannot hold
    /// (an unsigned integer above `Int64.max`, a non-finite `Date`, a
    /// coordinate off the globe).
    func cypherValue() throws(RelateError) -> CypherValue

    /// The Swift value a Cypher value holds.
    init(cypherValue: CypherValue) throws(RelateError)
}

/// The one mismatch a conversion reports: `value` is not a `type`.
func mismatch<T>(_ type: T.Type, _ value: CypherValue, reason: String? = nil) -> RelateError {
    .unexpectedResult(
        ResultMismatch(path: [], expected: "\(type)", found: value.kind.name, reason: reason))
}

/// The payload `accessor` reads out of `value`, or the mismatch for `type`
/// when the value is of another kind. Every conversion reads its payload
/// through here, so which case holds which payload is stated only by the
/// accessors (CypherValue+Accessors.swift).
func payload<T, Payload>(
    _ accessor: KeyPath<CypherValue, Payload?>, of value: CypherValue, as type: T.Type
) throws(RelateError) -> Payload {
    guard let payload = value[keyPath: accessor] else { throw mismatch(type, value) }
    return payload
}

/// The one mismatch a result of the wrong shape reports: a missing column,
/// a row count other than the one asked for, a value a `Decodable` type
/// does not decode from. Text that may quote a result value goes in
/// `reason`, which the message sanitizes.
func shapeMismatch(
    path: [String] = [], expected: String, found: String, reason: String? = nil
) -> RelateError {
    .unexpectedResult(ResultMismatch(path: path, expected: expected, found: found, reason: reason))
}

/// The one nested conversion: `value` as a `T`, with `component` (a column
/// name, a map key, a list position) prefixed to the path of a mismatch.
func convert<T: CypherValueConvertible>(
    _ value: CypherValue, under component: String, as type: T.Type = T.self
) throws(RelateError) -> T {
    do throws(RelateError) {
        return try T(cypherValue: value)
    } catch {
        throw error.nested(under: component)
    }
}

extension CypherValue: CypherValueConvertible {
    public func cypherValue() -> CypherValue { self }
    public init(cypherValue: CypherValue) { self = cypherValue }
}

extension Bool: CypherValueConvertible {
    public func cypherValue() -> CypherValue { .bool(self) }
    public init(cypherValue: CypherValue) throws(RelateError) {
        self = try payload(\.boolValue, of: cypherValue, as: Bool.self)
    }
}

extension String: CypherValueConvertible {
    public func cypherValue() -> CypherValue { .string(self) }
    public init(cypherValue: CypherValue) throws(RelateError) {
        self = try payload(\.stringValue, of: cypherValue, as: String.self)
    }
}

extension Substring: CypherValueConvertible {
    public func cypherValue() -> CypherValue { .string(String(self)) }
    public init(cypherValue: CypherValue) throws(RelateError) {
        self = Substring(try String(cypherValue: cypherValue))
    }
}

/// The integer conversions, stated once for every fixed-width type: out
/// through `Int64` (refusing what does not fit), in from `.int` (refusing
/// what the Swift type cannot hold).
extension CypherValueConvertible where Self: FixedWidthInteger {
    public func cypherValue() throws(RelateError) -> CypherValue {
        guard let value = Int64(exactly: self) else {
            throw RelateError.invalidParameter(
                "\(Self.self) value \(self) does not fit a Cypher INTEGER (64-bit signed)")
        }
        return .int(value)
    }

    public init(cypherValue: CypherValue) throws(RelateError) {
        let value = try payload(\.intValue, of: cypherValue, as: Self.self)
        guard let narrowed = Self(exactly: value) else {
            throw mismatch(Self.self, cypherValue, reason: "\(value) is out of range")
        }
        self = narrowed
    }
}

extension Int: CypherValueConvertible {}
extension Int8: CypherValueConvertible {}
extension Int16: CypherValueConvertible {}
extension Int32: CypherValueConvertible {}
extension Int64: CypherValueConvertible {}
extension UInt: CypherValueConvertible {}
extension UInt8: CypherValueConvertible {}
extension UInt16: CypherValueConvertible {}
extension UInt32: CypherValueConvertible {}
extension UInt64: CypherValueConvertible {}

extension Double: CypherValueConvertible {
    public func cypherValue() -> CypherValue { .float(self) }
    /// A `.float`, or an `.int` widened.
    public init(cypherValue: CypherValue) throws(RelateError) {
        self = try payload(\.doubleValue, of: cypherValue, as: Double.self)
    }
}

extension Float: CypherValueConvertible {
    public func cypherValue() -> CypherValue { .float(Double(self)) }
    /// A `.float` narrowed to the nearest `Float`, or an `.int` widened.
    public init(cypherValue: CypherValue) throws(RelateError) {
        self = Float(try payload(\.doubleValue, of: cypherValue, as: Float.self))
    }
}

extension Optional: CypherValueConvertible where Wrapped: CypherValueConvertible {
    /// `nil` is Cypher `null`.
    public func cypherValue() throws(RelateError) -> CypherValue {
        guard let self else { return .null }
        return try self.cypherValue()
    }

    public init(cypherValue: CypherValue) throws(RelateError) {
        self = cypherValue.isNull ? .none : .some(try Wrapped(cypherValue: cypherValue))
    }
}

extension Array: CypherValueConvertible where Element: CypherValueConvertible {
    /// A Cypher list. (A `[Float]` is a list of floats; a vector is always
    /// explicit — `CypherValue(vector:)`.)
    public func cypherValue() throws(RelateError) -> CypherValue {
        var items: [CypherValue] = []
        items.reserveCapacity(count)
        for element in self { items.append(try element.cypherValue()) }
        return .list(items)
    }

    /// A `.list`, or a `.vector` read as the list of its coordinates.
    public init(cypherValue: CypherValue) throws(RelateError) {
        try self.init(items: try payload(\.sequenceItems, of: cypherValue, as: [Element].self))
    }

    /// The elements `items` convert to, a mismatch naming its position.
    init(items: [CypherValue]) throws(RelateError) {
        var elements: [Element] = []
        elements.reserveCapacity(items.count)
        for (index, item) in items.enumerated() {
            elements.append(try convert(item, under: "[\(index)]"))
        }
        self = elements
    }
}

extension Dictionary: CypherValueConvertible where Key == String, Value: CypherValueConvertible {
    /// A Cypher map.
    public func cypherValue() throws(RelateError) -> CypherValue {
        var entries: [String: CypherValue] = [:]
        entries.reserveCapacity(count)
        for (key, value) in self { entries[key] = try value.cypherValue() }
        return .map(entries)
    }

    /// A `.map` (a node or relationship is not one; read its `properties`).
    public init(cypherValue: CypherValue) throws(RelateError) {
        let entries = try payload(\.mapValue, of: cypherValue, as: [String: Value].self)
        var result: [String: Value] = [:]
        result.reserveCapacity(entries.count)
        for (key, value) in entries {
            result[key] = try convert(value, under: key)
        }
        self = result
    }
}

/// A `RawRepresentable` type converts through its raw value once it
/// declares the conformance: `extension Status: CypherValueConvertible {}`.
extension CypherValueConvertible where Self: RawRepresentable, RawValue: CypherValueConvertible {
    public func cypherValue() throws(RelateError) -> CypherValue {
        try rawValue.cypherValue()
    }

    public init(cypherValue: CypherValue) throws(RelateError) {
        let raw = try RawValue(cypherValue: cypherValue)
        guard let value = Self(rawValue: raw) else {
            throw mismatch(Self.self, cypherValue, reason: "\(raw) is not one of its raw values")
        }
        self = value
    }
}

// MARK: - The typed structs

/// One of `CypherValue`'s payload structs: it converts out to its own case
/// and in from that case only. A conforming type names the case; the
/// conversions are stated once, below.
protocol CypherPayload: CypherValueConvertible {
    /// The case's constructor and its accessor.
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) { get }
}

extension CypherPayload {
    public func cypherValue() -> CypherValue { Self.payloadCase.wrap(self) }

    public init(cypherValue: CypherValue) throws(RelateError) {
        self = try payload(Self.payloadCase.extract, of: cypherValue, as: Self.self)
    }
}

// Entities (node, relationship, path) are read, never bound: binding one is
// refused by the params builder with the same `.invalidParameter` a
// hand-built `.node` gets.
extension CypherValue.Point: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.point, \.pointValue)
    }
}
extension CypherValue.Date: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.date, \.dateValue)
    }
}
extension CypherValue.Time: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.time, \.timeValue)
    }
}
extension CypherValue.LocalTime: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.localTime, \.localTimeValue)
    }
}
extension CypherValue.DateTime: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.dateTime, \.dateTimeValue)
    }
}
extension CypherValue.LocalDateTime: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.localDateTime, \.localDateTimeValue)
    }
}
extension CypherValue.Duration: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.duration, \.durationValue)
    }
}
extension CypherValue.Vector: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.vector, \.vectorValue)
    }
}
extension CypherValue.Node: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.node, \.nodeValue)
    }
}
extension CypherValue.Relationship: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.relationship, \.relationshipValue)
    }
}
extension CypherValue.Path: CypherPayload {
    static var payloadCase: (wrap: (Self) -> CypherValue, extract: KeyPath<CypherValue, Self?>) {
        (CypherValue.path, \.pathValue)
    }
}

// MARK: - Parameter dictionaries

/// The one conversion of a parameter dictionary of Swift values into the
/// typed values the params builder binds.
func convertParameters(
    _ parameters: [String: any CypherValueConvertible]
) throws(RelateError) -> [String: CypherValue] {
    var converted: [String: CypherValue] = [:]
    converted.reserveCapacity(parameters.count)
    for (name, value) in parameters { converted[name] = try value.cypherValue() }
    return converted
}
