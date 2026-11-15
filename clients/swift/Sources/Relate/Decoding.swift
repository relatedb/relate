// Closed-vocabulary decoding of wire strings: the refusal text quotes the
// offending value through the sanitizer in WireText.swift.

/// The one closed-vocabulary check for a wire string enum: the case whose raw
/// value is `raw`, or an ABI contract violation naming `what` and the
/// sanitized offending value. Query type, namespace, error stage, severity
/// and category all refuse through it.
func closedWire<E: RawRepresentable>(_ raw: String, what: String) throws(RelateError) -> E
where E.RawValue == String {
    guard let value = E(rawValue: raw) else {
        throw RelateError.decoding("ABI contract violated: \(what) \(quotedWire(raw))")
    }
    return value
}

/// The `Decodable` form of `closedWire` for a closed enum nested in a JSON
/// document: a `DecodingError` naming the key (the decoder's last coding key)
/// and the sanitized value. A decoder with an empty coding path is a misuse
/// of this helper and is refused as such, never given a placeholder key. A missing
/// or null key is already a `DecodingError` from the keyed container.
func decodeWire<E: RawRepresentable>(_ type: E.Type, from decoder: any Decoder) throws -> E
where E.RawValue == String {
    // A value decoded through a keyed container always has a coding key; its
    // absence is a decoder misuse, not a wire condition to paper over.
    guard let key = decoder.codingPath.last?.stringValue else {
        throw DecodingError.dataCorrupted(
            .init(codingPath: decoder.codingPath, debugDescription: "closed value decoded outside a keyed container"))
    }
    let raw = try decoder.singleValueContainer().decode(String.self)
    do {
        return try closedWire(raw, what: "unknown \(key)") as E
    } catch {
        throw DecodingError.dataCorrupted(
            .init(codingPath: decoder.codingPath, debugDescription: error.message))
    }
}
