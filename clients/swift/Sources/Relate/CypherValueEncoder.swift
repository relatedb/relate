// MARK: - Encodable → CypherValue

/// Encodes an `Encodable` value as a ``CypherValue``: a struct as a map, an
/// array as a list, so a model binds as one parameter
/// (`SET b += $props`).
///
/// A value that is ``CypherValueConvertible`` encodes through that
/// conversion, so a `Date` is a `DATETIME` and a `URL` a string — not the
/// number or nested container their own `Encodable` conformances write.
///
/// A `nil` optional property is left out of the map, as synthesized
/// `Encodable` does. That is what `SET n += $props` needs: a key holding
/// `null` there removes the property.
public struct CypherValueEncoder: Sendable {
    /// How property names become map keys.
    public var keyStrategy: CypherKeyStrategy = .useDefaultKeys

    public init(keyStrategy: CypherKeyStrategy = .useDefaultKeys) {
        self.keyStrategy = keyStrategy
    }

    /// Throws ``RelateError/invalidParameter(_:)`` when the value cannot be
    /// encoded (a value Cypher cannot hold, or an error the value's own
    /// `encode(to:)` throws).
    public func encode<T: Encodable>(_ value: T) throws(RelateError) -> CypherValue {
        do {
            return try encodeNode(value, codingPath: [], keyStrategy: keyStrategy).resolved()
        } catch let error as RelateError {
            throw error
        } catch let error as EncodingError {
            throw RelateError.invalidParameter("cannot encode \(T.self): \(describe(error))")
        } catch {
            throw RelateError.invalidParameter("cannot encode \(T.self): \(error)")
        }
    }
}

extension CypherValue {
    /// An `Encodable` value as a Cypher value (see ``CypherValueEncoder``).
    public init<T: Encodable>(
        encoding value: T, keyStrategy: CypherKeyStrategy = .useDefaultKeys
    ) throws(RelateError) {
        self = try CypherValueEncoder(keyStrategy: keyStrategy).encode(value)
    }
}

private func describe(_ error: EncodingError) -> String {
    switch error {
    case .invalidValue(_, let context):
        let path = mismatchPath(context.codingPath).joined(separator: ".")
        return path.isEmpty ? context.debugDescription : "\(path): \(context.debugDescription)"
    @unknown default:
        return "\(error)"
    }
}

/// One value being built. Containers hold a reference and fill it in, so
/// the order in which an `encode(to:)` asks for and writes its containers
/// does not matter.
private final class EncodedNode {
    enum Content {
        case empty
        case leaf(CypherValue)
        case map(MapBuilder)
        case list(ListBuilder)
    }

    var content: Content

    init(_ content: Content = .empty) {
        self.content = content
    }

    func resolved() -> CypherValue {
        switch content {
        // A value that asked for no container and wrote nothing encodes as
        // an empty map, the convention `JSONEncoder` follows.
        case .empty: .map([:])
        case .leaf(let value): value
        case .map(let builder): .map(builder.entries.mapValues { $0.resolved() })
        case .list(let builder): .list(builder.items.map { $0.resolved() })
        }
    }

    /// This node as a map: an empty node becomes one. Asking after the
    /// value encoded itself as something else is a bug in its
    /// `encode(to:)`, as it is for `JSONEncoder`.
    func keyed() -> MapBuilder {
        switch content {
        case .map(let builder):
            return builder
        case .empty:
            let builder = MapBuilder()
            content = .map(builder)
            return builder
        case .leaf, .list:
            preconditionFailure("a keyed container was requested for a value already encoded as something else")
        }
    }

    /// This node as a list: an empty node becomes one.
    func unkeyed() -> ListBuilder {
        switch content {
        case .list(let builder):
            return builder
        case .empty:
            let builder = ListBuilder()
            content = .list(builder)
            return builder
        case .leaf, .map:
            preconditionFailure("an unkeyed container was requested for a value already encoded as something else")
        }
    }
}

private final class MapBuilder {
    var entries: [String: EncodedNode] = [:]
}

private final class ListBuilder {
    var items: [EncodedNode] = []
}

/// Encodes one value into a fresh node: through its
/// `CypherValueConvertible` conversion when it has one, else through its
/// own `encode(to:)`.
private func encodeNode<T: Encodable>(
    _ value: T, codingPath: [any CodingKey], keyStrategy: CypherKeyStrategy
) throws -> EncodedNode {
    if let convertible = value as? any CypherValueConvertible {
        return EncodedNode(.leaf(try convertible.cypherValue()))
    }
    let node = EncodedNode()
    try value.encode(to: NodeEncoder(node: node, codingPath: codingPath, keyStrategy: keyStrategy))
    return node
}

private struct NodeEncoder: Encoder {
    let node: EncodedNode
    let codingPath: [any CodingKey]
    let keyStrategy: CypherKeyStrategy
    var userInfo: [CodingUserInfoKey: Any] { [:] }

    func container<Key: CodingKey>(keyedBy type: Key.Type) -> KeyedEncodingContainer<Key> {
        KeyedEncodingContainer(
            KeyedNode<Key>(map: node.keyed(), codingPath: codingPath, keyStrategy: keyStrategy))
    }

    func unkeyedContainer() -> any UnkeyedEncodingContainer {
        UnkeyedNode(list: node.unkeyed(), codingPath: codingPath, keyStrategy: keyStrategy)
    }

    func singleValueContainer() -> any SingleValueEncodingContainer {
        SingleNode(node: node, codingPath: codingPath, keyStrategy: keyStrategy)
    }
}

private struct KeyedNode<Key: CodingKey>: KeyedEncodingContainerProtocol {
    let map: MapBuilder
    let codingPath: [any CodingKey]
    let keyStrategy: CypherKeyStrategy

    private func stored(_ key: Key) -> String {
        keyStrategy.storedKey(for: key.stringValue)
    }

    /// An encoder for a fresh child stored under `name`: how every nested
    /// container and super encoder is made.
    private func child(at key: any CodingKey, named name: String) -> NodeEncoder {
        let child = EncodedNode()
        map.entries[name] = child
        return NodeEncoder(node: child, codingPath: codingPath + [key], keyStrategy: keyStrategy)
    }

    private func child(_ key: Key) -> NodeEncoder {
        child(at: key, named: stored(key))
    }

    mutating func encodeNil(forKey key: Key) throws {
        map.entries[stored(key)] = EncodedNode(.leaf(.null))
    }

    mutating func encode<T: Encodable>(_ value: T, forKey key: Key) throws {
        map.entries[stored(key)] = try encodeNode(
            value, codingPath: codingPath + [key], keyStrategy: keyStrategy)
    }

    mutating func nestedContainer<NestedKey: CodingKey>(
        keyedBy keyType: NestedKey.Type, forKey key: Key
    ) -> KeyedEncodingContainer<NestedKey> {
        child(key).container(keyedBy: NestedKey.self)
    }

    mutating func nestedUnkeyedContainer(forKey key: Key) -> any UnkeyedEncodingContainer {
        child(key).unkeyedContainer()
    }

    mutating func superEncoder() -> any Encoder {
        child(at: superCodingKey, named: superCodingKey.stringValue)
    }

    mutating func superEncoder(forKey key: Key) -> any Encoder {
        child(key)
    }
}

private struct UnkeyedNode: UnkeyedEncodingContainer {
    let list: ListBuilder
    let codingPath: [any CodingKey]
    let keyStrategy: CypherKeyStrategy

    var count: Int { list.items.count }

    private var nextPath: [any CodingKey] { codingPath + [AnyCodingKey(intValue: count)] }

    /// An encoder for a fresh child appended to the list.
    private func child() -> NodeEncoder {
        let encoder = NodeEncoder(node: EncodedNode(), codingPath: nextPath, keyStrategy: keyStrategy)
        list.items.append(encoder.node)
        return encoder
    }

    mutating func encodeNil() throws {
        list.items.append(EncodedNode(.leaf(.null)))
    }

    mutating func encode<T: Encodable>(_ value: T) throws {
        list.items.append(try encodeNode(value, codingPath: nextPath, keyStrategy: keyStrategy))
    }

    mutating func nestedContainer<NestedKey: CodingKey>(
        keyedBy keyType: NestedKey.Type
    ) -> KeyedEncodingContainer<NestedKey> {
        child().container(keyedBy: NestedKey.self)
    }

    mutating func nestedUnkeyedContainer() -> any UnkeyedEncodingContainer {
        child().unkeyedContainer()
    }

    mutating func superEncoder() -> any Encoder {
        child()
    }
}

private struct SingleNode: SingleValueEncodingContainer {
    let node: EncodedNode
    let codingPath: [any CodingKey]
    let keyStrategy: CypherKeyStrategy

    mutating func encodeNil() throws {
        node.content = .leaf(.null)
    }

    mutating func encode<T: Encodable>(_ value: T) throws {
        node.content = try encodeNode(value, codingPath: codingPath, keyStrategy: keyStrategy).content
    }
}
