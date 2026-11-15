// MARK: - Observed values

extension Database {
    /// The value `body` reads, now and again after each commit that may have
    /// changed it.
    ///
    /// ```swift
    /// for try await books in db.values(tags: ["books"]) { tx in
    ///     try tx.run("MATCH (b:Book) RETURN b.title AS title ORDER BY title").decode(Book.self)
    /// } {
    ///     show(books)
    /// }
    /// ```
    ///
    /// The stream yields the current value first, then a fresh one after a
    /// commit made through this `Database` object. It listens for commits
    /// from before the first read, so a write made at any point after this
    /// call is followed by a read that sees it.
    ///
    /// - `tags`: with `nil`, every commit is followed by a read. With a set,
    ///   only a commit that carries at least one of its tags is — a writer
    ///   that forgets its tag (an auto-commit statement carries none) leaves
    ///   the value stale, so narrow with tags only where every writer sets
    ///   them.
    /// - `body` runs as ``read(_:)``: one snapshot, nothing kept.
    /// - Commits that arrive while a read is running are followed by one
    ///   read, not one each, and a consumer that falls behind receives the
    ///   latest value only.
    /// - A read that throws ends the stream with its error. The stream also
    ///   ends when its consumer stops (a cancelled task, a released stream)
    ///   and when the store closes.
    public func values<T: Sendable>(
        tags: Set<String>? = nil, _ body: @escaping @Sendable (Transaction) throws -> T
    ) -> AsyncThrowingStream<T, any Error> {
        values(tags: tags, isRepeat: nil, body)
    }

    /// The value `body` reads, now and again after each commit that changed
    /// it: a read that returns a value equal to the last one yielded yields
    /// nothing, so a commit that left the answer alone is not a new value.
    /// (A consumer that fell behind while the answer changed and changed
    /// back can still receive the same value twice: the value in between
    /// was the one it missed.) Otherwise as the form for any `Sendable`
    /// value.
    public func values<T: Sendable & Equatable>(
        tags: Set<String>? = nil, _ body: @escaping @Sendable (Transaction) throws -> T
    ) -> AsyncThrowingStream<T, any Error> {
        values(tags: tags, isRepeat: { $0 == $1 }, body)
    }

    /// The one observation: a read now, then a read per change signal, each
    /// value yielded unless `isRepeat` says it repeats the last.
    private func values<T: Sendable>(
        tags: Set<String>?, isRepeat: (@Sendable (T, T) -> Bool)?,
        _ body: @escaping @Sendable (Transaction) throws -> T
    ) -> AsyncThrowingStream<T, any Error> {
        // Subscribed here, before the first read below can start.
        let changes = changeSignals(tags: tags)
        let (stream, continuation) = AsyncThrowingStream<T, any Error>.makeStream(
            bufferingPolicy: .bufferingNewest(1))
        // The task holds the database only while it reads, so an observation
        // does not keep a store open that nothing else holds.
        let reader = Task { [weak self] in
            var last: T?
            var changes = changes.makeAsyncIterator()
            do {
                repeat {
                    guard let database = self else { break }
                    let value = try await database.read(body)
                    if let last, let isRepeat, isRepeat(last, value) { continue }
                    continuation.yield(value)
                    last = value
                } while await changes.next() != nil
                continuation.finish()
            } catch {
                continuation.finish(throwing: error)
            }
        }
        continuation.onTermination = { _ in reader.cancel() }
        return stream
    }

    /// One signal for each burst of commits that concern `tags` (every
    /// commit when `nil`), starting from this call. Signals do not queue: a
    /// consumer busy with one finds at most one more waiting.
    private func changeSignals(tags: Set<String>?) -> AsyncStream<Void> {
        let commits = commits()
        let (signals, continuation) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        // The commit stream is drained as fast as commits arrive, so its
        // backlog stays empty however long a read takes.
        let pump = Task {
            for await commit in commits {
                if let tags, tags.isDisjoint(with: commit.tags) { continue }
                continuation.yield()
            }
            continuation.finish()
        }
        continuation.onTermination = { _ in pump.cancel() }
        return signals
    }
}
