import Foundation

/// The typed error shape every C ABI v2 failure carries (ADR-0502 §2) —
/// classified by the engine's own single table, so the client never
/// classifies an error itself. Branch on `code` / `stage`, retry on
/// `retryable`, never on `message` text. A pure model: the channel's JSON
/// decode lives at the boundary (CABI.swift's `engineError`).
public struct EngineError: Sendable, Hashable {
    /// The Neo4j status code (`Neo.<Classification>.<Category>.<Title>`),
    /// passed through unchanged so a `code`-based classifier reaches the
    /// same verdict.
    public let code: String
    /// The GQLSTATUS (five characters).
    public let gqlStatus: String
    /// Where the failure happened: `compile` (parse/plan), `runtime`
    /// (execution) or `internal` — a failure the engine never saw: a NULL
    /// or poisoned handle, a caught panic, an argument that failed to
    /// decode, a params-builder refusal.
    public let stage: Stage
    /// The drivers' transient test: `true` exactly when re-running the same
    /// work may succeed (today the write-write conflict and the retryable
    /// workspace cap).
    public let retryable: Bool
    public let message: String

    public enum Stage: String, Sendable, Hashable {
        case compile, runtime, `internal`
    }

    init(code: String, gqlStatus: String, stage: Stage, retryable: Bool, message: String) {
        self.code = code
        self.gqlStatus = gqlStatus
        self.stage = stage
        self.retryable = retryable
        self.message = message
    }
}

// MARK: - Classification read off the engine's code

extension EngineError {
    /// The class the engine's status code names — its second segment
    /// (`Neo.<Classification>.<Category>.<Title>`).
    public enum Classification: String, Sendable, Hashable {
        /// The statement or its input is at fault; re-running it unchanged
        /// fails the same way.
        case clientError = "ClientError"
        /// A condition that may clear: the retry verdict is ``EngineError/retryable``.
        case transientError = "TransientError"
        /// The engine or its storage failed.
        case databaseError = "DatabaseError"
    }

    /// The status codes the predicates below test for, spelled once. The
    /// engine's table is the source of truth; the package's tests provoke
    /// each of these from a live engine, so a drift fails there.
    public enum StatusCode {
        public static let constraintValidationFailed = "Neo.ClientError.Schema.ConstraintValidationFailed"
        public static let syntaxError = "Neo.ClientError.Statement.SyntaxError"
        public static let parameterMissing = "Neo.ClientError.Statement.ParameterMissing"
        public static let transactionOutdated = "Neo.TransientError.Transaction.Outdated"
    }

    /// The code's classification segment, `nil` when the code does not have
    /// the four-segment `Neo.` shape or names a class this client does not
    /// know.
    public var classification: Classification? {
        let segments = code.split(separator: ".", omittingEmptySubsequences: false)
        guard segments.count == 4, segments[0] == "Neo" else { return nil }
        return Classification(rawValue: String(segments[1]))
    }

    /// A uniqueness or existence constraint refused the write.
    public var isConstraintViolation: Bool { code == StatusCode.constraintValidationFailed }
    /// The statement did not parse.
    public var isSyntaxError: Bool { code == StatusCode.syntaxError }
    /// The statement names a `$parameter` the call did not bind.
    public var isMissingParameter: Bool { code == StatusCode.parameterMissing }
    /// A write-write conflict with another transaction: re-run the
    /// transaction (``retryable`` is `true`).
    public var isWriteConflict: Bool { code == StatusCode.transactionOutdated }
}

/// What ``RelateError/unexpectedResult(_:)`` carries: a result that did not
/// have the shape or type the caller asked for.
public struct ResultMismatch: Sendable, Hashable {
    /// Where: the column name, then any map keys and list positions
    /// (`"[2]"`) below it. Empty for a whole-result shape (`single()`).
    public let path: [String]
    /// What was asked for: a Swift type name, or a shape (`exactly one record`).
    public let expected: String
    /// What was there: a value kind (`null`, `string`), or a shape.
    public let found: String
    /// Why an in-kind value still did not convert (`300 is out of range`).
    public let reason: String?

    init(path: [String], expected: String, found: String, reason: String? = nil) {
        self.path = path
        self.expected = expected
        self.found = found
        self.reason = reason
    }

    /// The mismatch as one line of text. A column name, a map key and a
    /// reason can echo strings that came from the engine's result, so each
    /// is sanitized here as every echoed wire value is (non-printable
    /// characters escaped, the length bounded); ``path`` and ``reason``
    /// themselves hold the strings as they are.
    public var message: String {
        let place = path.isEmpty ? "" : path.map(safeWire).joined(separator: ".") + ": "
        let why = reason.map { " (\(safeWire($0)))" } ?? ""
        return "\(place)expected \(expected), found \(found)\(why)"
    }
}

/// Every error the Swift surface throws.
public enum RelateError: LocalizedError, Sendable, Equatable, CustomStringConvertible {
    /// The engine (or the C boundary) refused the call; the typed shape
    /// travels with the call that failed — there is no thread-local slot.
    case engine(EngineError)
    /// A parameter the wrapper cannot encode for the C ABI.
    case invalidParameter(String)
    /// The C ABI returned a payload that violates its documented contract.
    case decoding(String)
    /// A result did not have the shape or type the caller asked for: a
    /// typed record accessor, `single()`, `scalar()` or a `Decodable`
    /// decode.
    case unexpectedResult(ResultMismatch)

    public var message: String {
        switch self {
        case .engine(let e): e.message
        case .invalidParameter(let m), .decoding(let m): m
        case .unexpectedResult(let mismatch): mismatch.message
        }
    }

    /// This error with `component` prefixed to its mismatch path: how a
    /// container or record names where inside it a conversion failed. Any
    /// other error is returned unchanged.
    func nested(under component: String) -> RelateError {
        nested(under: [component])
    }

    /// This error with a whole path prefixed to its mismatch path.
    func nested(under path: [String]) -> RelateError {
        guard case .unexpectedResult(let mismatch) = self, !path.isEmpty else { return self }
        return .unexpectedResult(
            ResultMismatch(
                path: path + mismatch.path, expected: mismatch.expected,
                found: mismatch.found, reason: mismatch.reason))
    }

    /// The typed shape when this is an `.engine` error, `nil` otherwise.
    public var engineError: EngineError? {
        if case .engine(let e) = self { e } else { nil }
    }

    /// Whether re-running the same work may succeed: the engine's own
    /// verdict, `false` for the two client-side cases.
    public var isRetryable: Bool { engineError?.retryable ?? false }

    public var description: String {
        switch self {
        case .engine(let e): "Relate: [\(e.code)] \(e.message)"
        case .invalidParameter(let m): "Relate invalid parameter: \(m)"
        case .decoding(let m): "Relate decoding error: \(m)"
        case .unexpectedResult(let mismatch): "Relate unexpected result: \(mismatch.message)"
        }
    }

    /// What `error.localizedDescription` shows (otherwise Foundation prints
    /// "The operation couldn't be completed").
    public var errorDescription: String? { description }
}
