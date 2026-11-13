// MARK: - Key naming for the Codable bridges

/// How ``CypherValueEncoder`` and ``CypherValueDecoder`` map a Swift
/// property name to a map key, property key or column name.
public enum CypherKeyStrategy: Sendable, Hashable {
    /// The Swift name as written.
    case useDefaultKeys
    /// `readingStatus` in Swift is `reading_status` in the graph. Encoding
    /// writes the snake-case key; decoding looks a property up under its
    /// own name first, then under its snake-case spelling.
    case snakeCase

    /// The key a Swift property name is written under.
    func storedKey(for name: String) -> String {
        switch self {
        case .useDefaultKeys: name
        case .snakeCase: snakeCased(name)
        }
    }

    /// The keys a Swift property name is read from, in order: the name as
    /// written, then its stored spelling. Converting the Swift name (not
    /// the stored one) is what makes `coverURL` find `cover_url`: the
    /// stored key has lost which letters were capitals.
    func lookupKeys(for name: String) -> [String] {
        let stored = storedKey(for: name)
        return stored == name ? [name] : [name, stored]
    }
}

/// `readingStatus` → `reading_status`, `coverURL` → `cover_url`,
/// `myURLProperty` → `my_url_property`. An underscore goes before an
/// uppercase letter that follows a lowercase letter or digit, or that ends
/// a run of capitals.
func snakeCased(_ key: String) -> String {
    let characters = Array(key)
    var out = ""
    out.reserveCapacity(characters.count + 4)
    for (index, character) in characters.enumerated() {
        if character.isUppercase, index > 0 {
            let previous = characters[index - 1]
            let nextIsLower = index + 1 < characters.count && characters[index + 1].isLowercase
            if previous.isLowercase || previous.isNumber || (previous.isUppercase && nextIsLower) {
                out.append("_")
            }
        }
        out.append(contentsOf: character.lowercased())
    }
    return out
}

/// A coding key made at run time (a list position, a dynamic map key).
struct AnyCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int?

    init(stringValue: String) {
        self.stringValue = stringValue
        self.intValue = nil
    }

    init(intValue: Int) {
        self.stringValue = "[\(intValue)]"
        self.intValue = intValue
    }
}

/// The key a class's `super` container is stored under, in both directions
/// (the name `JSONEncoder` uses).
let superCodingKey = AnyCodingKey(stringValue: "super")

/// A coding path as a ``ResultMismatch`` path: key names, and list
/// positions as `[2]`.
func mismatchPath(_ codingPath: [any CodingKey]) -> [String] {
    codingPath.map { $0.intValue.map { "[\($0)]" } ?? $0.stringValue }
}
