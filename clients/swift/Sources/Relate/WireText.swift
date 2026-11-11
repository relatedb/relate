// Untrusted wire text echoed into an error message (CWE-117). The C ABI's
// strings come from a native library the client does not control, so a
// refusal never echoes one raw: control and other non-printable characters
// become escapes and the text is bounded. The Java and Go clients apply the
// same policy and the same two bounds.

/// The bound on an echoed closed-vocabulary value.
private let wireValueMax = 64
/// The bound on an echoed payload (a whole JSON document).
private let wirePayloadMax = 256

/// The display form of an offending wire value: sanitized (the value's own
/// quote and backslash escaped too), bounded to 64, wrapped in single quotes.
func quotedWire(_ raw: String) -> String {
    "'" + escapeWire(raw, max: wireValueMax) + "'"
}

/// The display form of an echoed payload: sanitized and bounded to 256.
func safeWire(_ raw: String) -> String {
    escapeWire(raw, max: wirePayloadMax)
}

/// Escapes every non-printable scalar, and the quote and
/// backslash that would break the message's own quoting, as `\uXXXX` (a supplementary scalar as its UTF-16
/// surrogate pair), bounds the escaped body to `max` UTF-16 units, and appends
/// `...(truncated)` when the input is cut.
private func escapeWire(_ raw: String, max: Int) -> String {
    var out = ""
    var units = 0
    var cut = false
    for scalar in raw.unicodeScalars {
        let escape = isUnprintable(scalar) || scalar == "\\" || scalar == "'"
        let piece = escape ? scalar.utf16.map(hex4).joined() : String(scalar)
        let width = escape ? 6 * scalar.utf16.count : scalar.utf16.count
        if units + width > max {
            cut = true
            break
        }
        out += piece
        units += width
    }
    return cut ? out + "...(truncated)" : out
}

private func hex4(_ unit: UInt16) -> String {
    let digits = String(unit, radix: 16)
    return "\\u" + String(repeating: "0", count: 4 - digits.count) + digits
}

/// Control, line/paragraph separator, format and unassigned scalars — the
/// Java client's policy.
private func isUnprintable(_ scalar: Unicode.Scalar) -> Bool {
    switch scalar.properties.generalCategory {
    case .control, .format, .surrogate, .lineSeparator, .paragraphSeparator, .unassigned:
        true
    default:
        false
    }
}
