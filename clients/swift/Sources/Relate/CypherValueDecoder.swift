// MARK: - CypherValue → Decodable

/// Decodes a `Decodable` type from a result record, a map, a node or a
/// relationship.
///
/// ```swift
/// struct Book: Decodable { let title: String; let year: Int? }
/// let books = try conn.query("MATCH (b:Book) RETURN b.title AS title, b.year AS year")
///     .decode(Book.self)
/// let nodes = try conn.query("MATCH (b:Book) RETURN b").decode(Book.self, column: "b")
/// ```
///
/// A record decodes by column name; a node or relationship by its property
/// keys. A type that is ``CypherValueConvertible`` decodes through that
/// conversion, with the same strict rules as `Record.get(_:as:)`:
/// an integer widens to a float and nothing else converts. A failure throws
/// ``RelateError/unexpectedResult(_:)`` whose path runs from the column
/// down to the key or list position that did not decode.
public struct CypherValueDecoder: Sendable {
    /// How map keys, property keys and column names become property names.
    public var keyStrategy: CypherKeyStrategy = .useDefaultKeys

    public init(keyStrategy: CypherKeyStrategy = .useDefaultKeys) {
        self.keyStrategy = keyStrategy
    }

    /// Decodes one value: a struct from a map, node or relationship, an
    /// array from a list.
    public func decode<T: Decodable>(_ type: T.Type, from value: CypherValue) throws(RelateError) -> T {
        try decode(T.self, from: value, at: [])
    }

    /// Decodes a value found at `codingPath` (the column it was read from),
    /// so every failure below it reports the whole path.
    func decode<T: Decodable>(
        _ type: T.Type, from value: CypherValue, at codingPath: [any CodingKey]
    ) throws(RelateError) -> T {
        try typed {
            try decodeValue(T.self, from: .value(value), codingPath: codingPath, keyStrategy: keyStrategy)
        }
    }

    /// Decodes one record by column name. A type that decodes from a single
    /// value (a `String`, an `[Int]`) reads the one column of a one-column
    /// record.
    public func decode<T: Decodable>(_ type: T.Type, from record: Record) throws(RelateError) -> T {
        try typed { try decodeValue(T.self, from: .record(record), codingPath: [], keyStrategy: keyStrategy) }
    }

    /// Runs a decode, turning whatever it throws into the package's error:
    /// its own passes through, a `DecodingError` (thrown by a type's
    /// hand-written `init(from:)`) becomes a mismatch at its coding path.
    private func typed<T>(_ body: () throws -> T) throws(RelateError) -> T {
        do {
            return try body()
        } catch let error as RelateError {
            throw error
        } catch let error as DecodingError {
            throw RelateError.unexpectedResult(mismatch(for: error))
        } catch {
            throw shapeMismatch(expected: "a decodable value", found: "\(error)")
        }
    }

    private func mismatch(for error: DecodingError) -> ResultMismatch {
        switch error {
        case .keyNotFound(let key, let context):
            ResultMismatch(
                path: mismatchPath(context.codingPath + [key]), expected: "a value", found: "no such key")
        case .typeMismatch(let type, let context):
            ResultMismatch(
                path: mismatchPath(context.codingPath), expected: "\(type)",
                found: context.debugDescription)
        case .valueNotFound(let type, let context):
            ResultMismatch(path: mismatchPath(context.codingPath), expected: "\(type)", found: "null")
        case .dataCorrupted(let context):
            ResultMismatch(
                path: mismatchPath(context.codingPath), expected: "a valid value",
                found: context.debugDescription)
        @unknown default:
            ResultMismatch(path: [], expected: "a decodable value", found: "\(error)")
        }
    }
}

extension Record {
    /// Decodes this record by column name.
    public func decode<T: Decodable>(
        _ type: T.Type = T.self, using decoder: CypherValueDecoder = CypherValueDecoder()
    ) throws(RelateError) -> T {
        try decoder.decode(T.self, from: self)
    }

    /// Decodes the value in one column: a node, relationship or map as a
    /// struct.
    public func decode<T: Decodable>(
        _ type: T.Type = T.self, column: String,
        using decoder: CypherValueDecoder = CypherValueDecoder()
    ) throws(RelateError) -> T {
        try decoder.decode(T.self, from: try get(column), at: [AnyCodingKey(stringValue: column)])
    }
}

extension QueryResult {
    /// Decodes every record by column name.
    public func decode<T: Decodable>(
        _ type: T.Type = T.self, using decoder: CypherValueDecoder = CypherValueDecoder()
    ) throws(RelateError) -> [T] {
        var decoded: [T] = []
        decoded.reserveCapacity(count)
        for record in self { decoded.append(try record.decode(T.self, using: decoder)) }
        return decoded
    }

    /// Decodes one column of every record: `MATCH (b:Book) RETURN b` as
    /// `[Book]`.
    public func decode<T: Decodable>(
        _ type: T.Type = T.self, column: String,
        using decoder: CypherValueDecoder = CypherValueDecoder()
    ) throws(RelateError) -> [T] {
        var decoded: [T] = []
        decoded.reserveCapacity(count)
        for record in self { decoded.append(try record.decode(T.self, column: column, using: decoder)) }
        return decoded
    }
}

// MARK: - The Decoder

/// What a decoder reads from: a single value, or a whole record (whose columns
/// are its keys).
private enum Source {
    case value(CypherValue)
    case record(Record)
}

private func failure(
    _ codingPath: [any CodingKey], expected: String, found: String, reason: String? = nil
) -> RelateError {
    .unexpectedResult(
        ResultMismatch(path: mismatchPath(codingPath), expected: expected, found: found, reason: reason))
}

/// Decodes one `T` from a source: through `CypherValueConvertible` when the
/// type has that conversion, else through its own `init(from:)`.
private func decodeValue<T: Decodable>(
    _ type: T.Type, from source: Source, codingPath: [any CodingKey], keyStrategy: CypherKeyStrategy
) throws -> T {
    if let convertible = type as? any CypherValueConvertible.Type {
        let value = try singleValue(of: source, codingPath: codingPath, expected: "\(type)")
        do throws(RelateError) {
            // The metatype is `T`'s own, so the conversion yields a `T`.
            guard let converted = try convertible.init(cypherValue: value) as? T else {
                throw mismatch(type, value)
            }
            return converted
        } catch {
            throw error.nested(under: mismatchPath(codingPath))
        }
    }
    return try T(from: SourceDecoder(source: source, codingPath: codingPath, keyStrategy: keyStrategy))
}

/// The one value a source holds: itself, or the only column of a
/// one-column record.
private func singleValue(
    of source: Source, codingPath: [any CodingKey], expected: String
) throws(RelateError) -> CypherValue {
    switch source {
    case .value(let value):
        return value
    case .record(let record):
        guard record.count == 1 else {
            throw failure(codingPath, expected: expected, found: "a record of \(record.count) columns")
        }
        return record[0]
    }
}

private struct SourceDecoder: Decoder {
    let source: Source
    let codingPath: [any CodingKey]
    let keyStrategy: CypherKeyStrategy
    var userInfo: [CodingUserInfoKey: Any] { [:] }

    /// The source's entries by key: a record's columns, a map's entries, an
    /// entity's properties.
    private func entries() throws -> [String: CypherValue] {
        switch source {
        case .record(let record):
            return record.dictionary
        case .value(let value):
            guard let entries = value.entries else {
                throw failure(codingPath, expected: "a map, node or relationship", found: value.kind.name)
            }
            return entries
        }
    }

    func container<Key: CodingKey>(keyedBy type: Key.Type) throws -> KeyedDecodingContainer<Key> {
        KeyedDecodingContainer(
            KeyedSource<Key>(entries: try entries(), codingPath: codingPath, keyStrategy: keyStrategy))
    }

    func unkeyedContainer() throws -> any UnkeyedDecodingContainer {
        let value = try singleValue(of: source, codingPath: codingPath, expected: "a list")
        guard let items = value.sequenceItems else {
            throw failure(codingPath, expected: "a list", found: value.kind.name)
        }
        return UnkeyedSource(items: items, codingPath: codingPath, keyStrategy: keyStrategy)
    }

    func singleValueContainer() throws -> any SingleValueDecodingContainer {
        SingleSource(
            value: try singleValue(of: source, codingPath: codingPath, expected: "a single value"),
            codingPath: codingPath, keyStrategy: keyStrategy)
    }
}

private struct KeyedSource<Key: CodingKey>: KeyedDecodingContainerProtocol {
    let entries: [String: CypherValue]
    let codingPath: [any CodingKey]
    let keyStrategy: CypherKeyStrategy

    /// The stored keys this key type can name.
    var allKeys: [Key] { entries.keys.compactMap(Key.init(stringValue:)) }

    private func names(_ key: Key) -> [String] {
        keyStrategy.lookupKeys(for: key.stringValue)
    }

    func contains(_ key: Key) -> Bool {
        names(key).contains { entries[$0] != nil }
    }

    /// The entry under the first of `names` that exists; a missing one is
    /// reported at `key`.
    private func value(at key: any CodingKey, named names: [String]) throws -> CypherValue {
        guard let value = names.lazy.compactMap({ entries[$0] }).first else {
            throw failure(codingPath + [key], expected: "a value", found: "no such key")
        }
        return value
    }

    private func value(_ key: Key) throws -> CypherValue {
        try value(at: key, named: names(key))
    }

    func decodeNil(forKey key: Key) throws -> Bool {
        try value(key).isNull
    }

    func decode<T: Decodable>(_ type: T.Type, forKey key: Key) throws -> T {
        try decodeValue(
            T.self, from: .value(try value(key)), codingPath: codingPath + [key],
            keyStrategy: keyStrategy)
    }

    private func decoder(at key: any CodingKey, named names: [String]) throws -> SourceDecoder {
        SourceDecoder(
            source: .value(try value(at: key, named: names)), codingPath: codingPath + [key],
            keyStrategy: keyStrategy)
    }

    private func decoder(_ key: Key) throws -> SourceDecoder {
        try decoder(at: key, named: names(key))
    }

    func nestedContainer<NestedKey: CodingKey>(
        keyedBy type: NestedKey.Type, forKey key: Key
    ) throws -> KeyedDecodingContainer<NestedKey> {
        try decoder(key).container(keyedBy: NestedKey.self)
    }

    func nestedUnkeyedContainer(forKey key: Key) throws -> any UnkeyedDecodingContainer {
        try decoder(key).unkeyedContainer()
    }

    func superDecoder() throws -> any Decoder {
        try decoder(at: superCodingKey, named: [superCodingKey.stringValue])
    }

    func superDecoder(forKey key: Key) throws -> any Decoder {
        try decoder(key)
    }
}

private struct UnkeyedSource: UnkeyedDecodingContainer {
    let items: [CypherValue]
    let codingPath: [any CodingKey]
    let keyStrategy: CypherKeyStrategy
    var currentIndex = 0

    var count: Int? { items.count }
    var isAtEnd: Bool { currentIndex >= items.count }

    private var nextPath: [any CodingKey] { codingPath + [AnyCodingKey(intValue: currentIndex)] }

    private func next(expecting expected: String) throws -> CypherValue {
        guard !isAtEnd else {
            throw failure(nextPath, expected: expected, found: "the end of a list of \(items.count)")
        }
        return items[currentIndex]
    }

    mutating func decodeNil() throws -> Bool {
        guard try next(expecting: "a value").isNull else { return false }
        currentIndex += 1
        return true
    }

    mutating func decode<T: Decodable>(_ type: T.Type) throws -> T {
        let decoded = try decodeValue(
            T.self, from: .value(try next(expecting: "\(type)")), codingPath: nextPath,
            keyStrategy: keyStrategy)
        currentIndex += 1
        return decoded
    }

    private mutating func nextDecoder(expecting expected: String) throws -> SourceDecoder {
        let decoder = SourceDecoder(
            source: .value(try next(expecting: expected)), codingPath: nextPath,
            keyStrategy: keyStrategy)
        currentIndex += 1
        return decoder
    }

    mutating func nestedContainer<NestedKey: CodingKey>(
        keyedBy type: NestedKey.Type
    ) throws -> KeyedDecodingContainer<NestedKey> {
        try nextDecoder(expecting: "a map").container(keyedBy: NestedKey.self)
    }

    mutating func nestedUnkeyedContainer() throws -> any UnkeyedDecodingContainer {
        try nextDecoder(expecting: "a list").unkeyedContainer()
    }

    mutating func superDecoder() throws -> any Decoder {
        try nextDecoder(expecting: "a value")
    }
}

private struct SingleSource: SingleValueDecodingContainer {
    let value: CypherValue
    let codingPath: [any CodingKey]
    let keyStrategy: CypherKeyStrategy

    func decodeNil() -> Bool { value.isNull }

    func decode<T: Decodable>(_ type: T.Type) throws -> T {
        try decodeValue(T.self, from: .value(value), codingPath: codingPath, keyStrategy: keyStrategy)
    }
}
