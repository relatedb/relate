// MARK: - Managed asynchronous access

extension Database {
    /// Runs `body` as a read on a connection the database manages, off the
    /// caller's thread.
    ///
    /// `body` is synchronous and runs in a transaction that is always
    /// rolled back: its statements see one snapshot, nothing it does is
    /// kept, and a statement that is not a read is refused with
    /// ``RelateError/misuse(_:)``. Reads run concurrently with each other
    /// (up to ``Options/maxReaders`` connections) and with a write.
    ///
    /// ```swift
    /// let titles = try await db.read { tx in
    ///     try tx.run("MATCH (b:Book) RETURN b.title AS title").map { try $0.get("title", as: String.self) }
    /// }
    /// ```
    ///
    /// The awaiting task suspends; the engine call blocks the connection's
    /// own queue, not a cooperative-pool thread. A task cancelled before
    /// the body starts — before the call, or while it waits its turn on a
    /// connection — throws `CancellationError` and runs nothing; a
    /// statement already running cannot be interrupted.
    public func read<T: Sendable>(
        _ body: @escaping @Sendable (Transaction) throws -> T
    ) async throws -> T {
        try await pool.withReader(for: self) { connection in try connection.read(body) }
    }

    /// Runs `body` as one write transaction on the database's single
    /// writer connection, off the caller's thread: committed when `body`
    /// returns, rolled back when it throws.
    ///
    /// Writes made this way run one at a time, so they never conflict with
    /// each other. `retries` covers a conflict with a writer outside the
    /// pool (a connection from ``connect()``, another process): a failure
    /// the engine marks retryable runs `body` again, up to that many more
    /// times. `tags` travel with the commit to ``commits(bufferingPolicy:)``.
    @discardableResult
    public func write<T: Sendable>(
        retries: Int = 0, tags: Set<String> = [],
        _ body: @escaping @Sendable (Transaction) throws -> T
    ) async throws -> T {
        try await onWriter("write") { connection in
            try connection.transaction(retries: retries, tags: tags, body)
        }
    }

    /// Runs one statement as a read. Like the body form, its result is not
    /// discardable: a read has no effect but its value.
    public func read(_ statement: Statement) async throws -> QueryResult {
        try await read { tx in try tx.run(statement) }
    }

    /// Runs one statement as a write transaction.
    @discardableResult
    public func write(
        _ statement: Statement, retries: Int = 0, tags: Set<String> = []
    ) async throws -> QueryResult {
        try await write(retries: retries, tags: tags) { tx in try tx.run(statement) }
    }

    /// Runs statements in one write transaction: all commit, or none do.
    @discardableResult
    public func write(
        _ statements: [Statement], retries: Int = 0, tags: Set<String> = []
    ) async throws -> [QueryResult] {
        try await write(retries: retries, tags: tags) { tx in try tx.run(statements) }
    }

    /// Writes a checkpoint off the caller's thread, on the writer's queue,
    /// so it runs between writes rather than beside one. A task cancelled
    /// before the checkpoint starts throws `CancellationError`.
    public func checkpointInBackground() async throws {
        try await onWriter("checkpoint") { connection in try connection.database.checkpoint() }
    }

    // MARK: The hop onto the writer

    /// Runs `work` on the single writer connection. Every managed operation
    /// that writes enters here, so the read-only refusal — which names the
    /// `operation` — is stated once.
    func onWriter<T: Sendable>(
        _ operation: String, _ work: @escaping @Sendable (Connection) throws -> T
    ) async throws -> T {
        guard !isReadOnly else {
            throw RelateError.misuse("\(operation) on a database opened read-only")
        }
        return try await pool.withWriter(for: self, work)
    }
}
