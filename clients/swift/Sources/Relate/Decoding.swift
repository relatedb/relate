// Closed-vocabulary decoding of wire strings: the refusal text quotes the
// offending value through the sanitizer in WireText.swift.

/// The one closed-vocabulary check for a wire string enum: the case whose raw
/// value is `raw`, or an ABI contract violation naming `what` and the
/// sanitized offending value. Query type, namespace, error stage, severity
/// and category all refuse through it.
func closedWire<E: RawRepresentable>(_ type: E.Type, _ raw: String, what: String) throws(RelateError) -> E
where E.RawValue == String {
    guard let value = E(rawValue: raw) else {
        throw RelateError.decoding("ABI contract violated: \(what) \(quotedWire(raw))")
    }
    return value
}

/// The `Decodable` form of `closedWire` for a closed enum nested in a JSON
/// document: a `DecodingError` naming the key (the decoder's last coding key,
/// always present under a keyed container) and the sanitized value. A missing
/// or null key is already a `DecodingError` from the keyed container.
func decodeWire<E: RawRepresentable>(_ type: E.Type, from decoder: any Decoder) throws -> E
where E.RawValue == String {
    let raw = try decoder.singleValueContainer().decode(String.self)
    do {
        return try closedWire(type, raw, what: "unknown \(decoder.codingPath.last?.stringValue ?? "?")")
    } catch {
        throw DecodingError.dataCorrupted(
            .init(codingPath: decoder.codingPath, debugDescription: error.message))
    }
}
