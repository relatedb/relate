// MARK: - Transaction scope

/// What a transaction body may do: run statements. Beginning, committing,
/// rolling back and changing the namespace belong to the call that opened
/// the transaction, and are not reachable from here.
///
/// A `Transaction` is valid only inside the body it was passed to. Using
/// one that escaped throws ``RelateError/misuse(_:)``.
public final class Transaction {
    /// What kind of scope this is. The mode owns how the scope ends and
    /// which statements it keeps.
    enum Mode {
        /// Commits when the body returns.
        case write
        /// Always rolls back; a statement that is not a read is refused.
        case read

        /// Ends a transaction whose body ran to completion.
        func finish(on connection: Connection) throws(RelateError) {
            switch self {
            case .write: try connection.commit()
            case .read: try connection.rollback()
            }
        }

        /// Refuses a statement this mode does not keep.
        func check(_ result: QueryResult) throws(RelateError) {
            // The statement ran, but only in this transaction's overlay,
            // which a read always rolls back. Refusing here is what tells
            // the caller the write was never going to land.
            if self == .read, result.queryType != .read {
                throw RelateError.misuse(
                    "a \(result.queryType) statement ran in a read; nothing it changed is kept — use a write")
            }
        }
    }

    private let connection: Connection
    private let mode: Mode
    private var isActive = true

    /// Whether this scope is a read: its transaction is rolled back, and a
    /// statement that is not a read is refused.
    public var isReadOnly: Bool { mode == .read }

    init(connection: Connection, mode: Mode) {
        self.connection = connection
        self.mode = mode
    }

    /// Ends the scope: later calls on it are refused.
    func end() {
        isActive = false
    }

    /// Runs one statement inside the transaction.
    @discardableResult
    public func query(_ cypher: String, _ parameters: [String: CypherValue] = [:])
        throws(RelateError) -> QueryResult
    {
        guard isActive else {
            throw RelateError.misuse("this transaction has ended; its scope is the body it was passed to")
        }
        let result = try connection.query(cypher, parameters)
        try mode.check(result)
        return result
    }

    /// Runs one statement with Swift values as parameters.
    @_disfavoredOverload
    @discardableResult
    public func query(_ cypher: String, _ parameters: [String: any CypherValueConvertible])
        throws(RelateError) -> QueryResult
    {
        try query(cypher, try convertParameters(parameters))
    }

    /// Runs one ``Statement``.
    @discardableResult
    public func run(_ statement: Statement) throws(RelateError) -> QueryResult {
        let (cypher, parameters) = statement.rendered()
        return try query(cypher, parameters)
    }

    /// Runs statements in order, stopping at the first that throws.
    @discardableResult
    public func run(_ statements: [Statement]) throws(RelateError) -> [QueryResult] {
        var results: [QueryResult] = []
        results.reserveCapacity(statements.count)
        for statement in statements { results.append(try run(statement)) }
        return results
    }
}

// MARK: - Running a transaction

extension Connection {
    /// Runs `body` in an explicit transaction: commits when it returns,
    /// rolls back and rethrows when it throws — and likewise when the commit
    /// itself throws.
    ///
    /// With `retries`, a failure the engine marks retryable (a write-write
    /// conflict, on a statement or on the commit) rolls back and runs `body`
    /// again, up to that many more times (a count below one: never).
    /// `body` must therefore be safe to run more than once; anything it did
    /// outside the transaction is not undone.
    @discardableResult
    public func transaction<T>(retries: Int = 0, _ body: (Transaction) throws -> T) throws -> T {
        try retrying(retries, lift: { $0 }, body)
    }

    /// Runs `body` in a transaction that is always rolled back: every
    /// statement sees one snapshot, and nothing commits. A statement that is
    /// not a read is refused with ``RelateError/misuse(_:)``. The result is
    /// not discardable: a read has no effect but its value.
    public func read<T>(_ body: (Transaction) throws -> T) throws -> T {
        try attempt(.read, lift: { $0 }, body)
    }

    /// Runs statements in one transaction: all commit, or none do. With
    /// `retries`, a retryable failure runs them all again.
    @discardableResult
    public func runInTransaction(_ statements: [Statement], retries: Int = 0) throws(RelateError)
        -> [QueryResult]
    {
        try retrying(retries, lift: { $0 }) { (scope: Transaction) throws(RelateError) in
            try scope.run(statements)
        }
    }

    // The two helpers below are generic over the error the body throws, so
    // one retry loop and one lifecycle serve both the throwing form (any
    // error) and the typed one (the package's own). `lift` is how the
    // lifecycle's own failures (begin, commit, rollback) join that type.

    /// The one retry loop: a write attempt, again while the failure is one
    /// the engine marks retryable and attempts remain.
    private func retrying<T, Failure: Error>(
        _ retries: Int, lift: (RelateError) -> Failure, _ body: (Transaction) throws(Failure) -> T
    ) throws(Failure) -> T {
        var attempts = 0
        while true {
            do throws(Failure) {
                return try attempt(.write, lift: lift, body)
            } catch {
                guard attempts < retries, let own = error as? RelateError, own.isRetryable else {
                    throw error
                }
                attempts += 1
            }
        }
    }

    /// The one lifecycle: begin, body, then the mode's ending. The first
    /// error — the body's or the ending's — is the one surfaced; a rollback
    /// failure after it would only mask the cause, and a throwing commit
    /// must not leave the connection inside the failed transaction.
    private func attempt<T, Failure: Error>(
        _ mode: Transaction.Mode, lift: (RelateError) -> Failure,
        _ body: (Transaction) throws(Failure) -> T
    ) throws(Failure) -> T {
        do throws(RelateError) { try begin() } catch { throw lift(error) }
        let scope = Transaction(connection: self, mode: mode)
        defer { scope.end() }
        do throws(Failure) {
            let value = try body(scope)
            do throws(RelateError) { try mode.finish(on: self) } catch { throw lift(error) }
            return value
        } catch {
            try? rollback()
            throw error
        }
    }
}
