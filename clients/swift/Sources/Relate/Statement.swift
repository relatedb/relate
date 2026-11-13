// MARK: - Statements

/// A Cypher statement with its parameters: a value that can be built once,
/// compared, passed across actors and run later.
///
/// Written as a string literal, an interpolated Swift value is **bound as a
/// parameter**, never spliced into the text:
///
/// ```swift
/// let key = "/works/OL59800W"
/// try conn.run("MATCH (b:Book {key: \(key)}) SET b.rating = \(4)")
/// // text:       MATCH (b:Book {key: $_p0}) SET b.rating = $_p1
/// // parameters: ["_p0": "/works/OL59800W", "_p1": 4]
/// ```
///
/// Interpolating a value can throw (a value Cypher cannot hold), so an
/// interpolated literal is written under `try`. What is not a value has its
/// own form: `\(identifier: name)` for a label, relationship type or
/// property key (backtick-quoted), `\(raw: text)` for trusted statement
/// text, and `\(statement)` to embed another `Statement` with its
/// parameters.
///
/// The `String`-taking `Connection.query` is a different entry point on
/// purpose. Passed to `query`, the same
/// literal is an ordinary Swift string and its values land in the text.
public struct Statement: Sendable, Hashable {
    /// Statement text and bound values, in order. Names for the bound
    /// values are assigned when the statement is rendered, so embedding one
    /// statement in another needs no renaming.
    enum Segment: Sendable, Hashable {
        case text(String)
        case value(CypherValue)
    }

    var segments: [Segment]
    /// Parameters the text names itself (`$key`).
    var named: [String: CypherValue]

    /// A statement whose text names its own parameters (`$key`).
    public init(_ cypher: String, _ parameters: [String: CypherValue] = [:]) {
        self.segments = [.text(cypher)]
        self.named = parameters
    }

    /// A statement whose text names its own parameters, given as Swift
    /// values.
    ///
    /// Disfavored so that a dictionary of literals keeps resolving to the
    /// non-throwing initializer above; a dictionary holding any Swift
    /// variable can only resolve here.
    @_disfavoredOverload
    public init(_ cypher: String, _ parameters: [String: any CypherValueConvertible])
        throws(RelateError)
    {
        self.init(cypher, try convertParameters(parameters))
    }

    /// The statement text, with each interpolated value replaced by a
    /// generated parameter (`$_p0`, `$_p1`, … in order of appearance — the
    /// same literal always renders the same text).
    public var cypher: String { rendered().cypher }

    /// Every parameter: the named ones and the generated ones.
    public var parameters: [String: CypherValue] { rendered().parameters }

    /// The text and parameters the engine receives. A generated name that
    /// the statement's own named parameters already use is skipped.
    func rendered() -> (cypher: String, parameters: [String: CypherValue]) {
        var cypher = ""
        var parameters = named
        var next = 0
        for segment in segments {
            switch segment {
            case .text(let text):
                cypher += text
            case .value(let value):
                var name = "_p\(next)"
                while named[name] != nil {
                    next += 1
                    name = "_p\(next)"
                }
                next += 1
                cypher += "$" + name
                parameters[name] = value
            }
        }
        return (cypher, parameters)
    }

    /// Appends another statement's text and parameters. A named parameter
    /// both statements bind must hold the same value in each; two different
    /// values under one name are refused.
    ///
    /// A refused append leaves this statement unchanged.
    public mutating func append(_ other: Statement) throws(RelateError) {
        var merged = named
        for (name, value) in other.named {
            if let existing = merged[name], existing != value {
                throw RelateError.invalidParameter(
                    "parameter \(quotedWire(name)) is bound to two different values")
            }
            merged[name] = value
        }
        named = merged
        segments += other.segments
    }

    /// A Cypher identifier (label, relationship type, property key) quoted
    /// in backticks, with any backtick in the name doubled.
    public static func quoted(identifier name: String) -> String {
        "`" + name.replacingOccurrences(of: "`", with: "``") + "`"
    }
}

extension Statement: ExpressibleByStringInterpolation {
    public init(stringLiteral value: String) {
        self.init(value)
    }

    public init(stringInterpolation: StringInterpolation) {
        self = stringInterpolation.statement
    }

    public struct StringInterpolation: StringInterpolationProtocol {
        var statement = Statement("")

        public init(literalCapacity: Int, interpolationCount: Int) {
            statement.segments = []
            statement.segments.reserveCapacity(interpolationCount * 2 + 1)
        }

        public mutating func appendLiteral(_ literal: String) {
            statement.segments.append(.text(literal))
        }

        /// Binds a Swift value as a parameter.
        public mutating func appendInterpolation<T: CypherValueConvertible>(_ value: T)
            throws(RelateError)
        {
            statement.segments.append(.value(try value.cypherValue()))
        }

        /// Binds an optional Swift value: `nil` is Cypher `null`. (Spelled
        /// out so that interpolating an optional is not flagged as an
        /// accidental debug description.)
        public mutating func appendInterpolation<T: CypherValueConvertible>(_ value: T?)
            throws(RelateError)
        {
            statement.segments.append(.value(try value.cypherValue()))
        }

        /// Embeds another statement: its text in place, its parameters with
        /// it.
        public mutating func appendInterpolation(_ fragment: Statement) throws(RelateError) {
            try statement.append(fragment)
        }

        /// A label, relationship type or property key, backtick-quoted.
        public mutating func appendInterpolation(identifier name: String) {
            statement.segments.append(.text(Statement.quoted(identifier: name)))
        }

        /// Statement text spliced in as written. For text the program
        /// itself wrote (a shared projection, a sort direction) — never for
        /// input.
        public mutating func appendInterpolation(raw text: String) {
            statement.segments.append(.text(text))
        }
    }
}

extension Statement: CustomStringConvertible {
    /// The rendered text. Parameter values are not included.
    public var description: String { cypher }
}

extension Connection {
    /// Runs one ``Statement``.
    @discardableResult
    public func run(_ statement: Statement) throws(RelateError) -> QueryResult {
        let (cypher, parameters) = statement.rendered()
        return try query(cypher, parameters)
    }
}
