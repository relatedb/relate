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

/// Every error the Swift surface throws.
public enum RelateError: LocalizedError, Sendable, Equatable, CustomStringConvertible {
    /// The engine (or the C boundary) refused the call; the typed shape
    /// travels with the call that failed — there is no thread-local slot.
    case engine(EngineError)
    /// A parameter the wrapper cannot encode for the C ABI.
    case invalidParameter(String)
    /// The C ABI returned a payload that violates its documented contract.
    case decoding(String)

    public var message: String {
        switch self {
        case .engine(let e): e.message
        case .invalidParameter(let m), .decoding(let m): m
        }
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
        }
    }

    /// What `error.localizedDescription` shows (otherwise Foundation prints
    /// "The operation couldn't be completed").
    public var errorDescription: String? { description }
}
