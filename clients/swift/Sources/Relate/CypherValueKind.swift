/// The value kinds the ABI's walker reports (one per ``CypherValue`` case);
/// the raw values are the `enum relate_value_kind` enumerators.
public enum CypherValueKind: UInt32, Sendable, Hashable, CaseIterable {
    case null = 0, bool = 1, int = 2, float = 3, string = 4
    case node = 5, relationship = 6, path = 7, list = 8, map = 9
    case point = 10, date = 11, time = 12, localTime = 13
    case dateTime = 14, localDateTime = 15, duration = 16, vector = 17

    /// The one conversion point for `relate_value_kind`'s `Int32`
    /// return: a negative raw (a NULL handle) never reaches here, and an
    /// enumerator this client does not know (a later, additive ABI) is a
    /// defined, reportable error — never a silent null.
    init(abi raw: Int32) throws(RelateError) {
        guard raw >= 0, let kind = CypherValueKind(rawValue: UInt32(raw)) else {
            throw RelateError.decoding("unsupported value kind \(raw) from the C ABI")
        }
        self = kind
    }
}
