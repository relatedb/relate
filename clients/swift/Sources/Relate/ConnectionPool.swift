import Foundation
import os

// MARK: - The connections behind Database.read and Database.write

/// One writer and a bounded set of readers, each opened on first use and
/// confined to its own serial queue.
///
/// The pool's connections belong to the `Database` that owns the pool, so
/// they refer back to it without owning it: releasing the last `Database`
/// reference releases them.
final class ConnectionPool: Sendable {
    /// What the pool needs from its surroundings, injectable so the
    /// recovery path can be tested without an engine panic (and so a test
    /// can count the connections a pool opens).
    struct Hooks: Sendable {
        /// Opens a connection for the pool of `database`.
        var open: @Sendable (Database) throws(RelateError) -> Connection
        var isPoisoned: @Sendable (Connection) -> Bool = { $0.isPoisoned() }

        /// The hooks a `Database` runs with.
        static let managed = Hooks(open: { (database: Database) throws(RelateError) in
            try database.core.connect(
                namespaceNamed: DatabaseNamespace.neo4j.rawValue, database: database, retained: false)
        })
    }

    private struct State {
        var writer: Worker?
        var readers: [(worker: Worker, pending: Int)] = []
    }

    private let hooks: Hooks
    private let maxReaders: Int
    private let state = OSAllocatedUnfairLock(initialState: State())

    init(maxReaders: Int, hooks: Hooks = .managed) {
        self.hooks = hooks
        self.maxReaders = maxReaders
    }

    /// Runs `work` on the single writer connection: every write runs on its
    /// queue, one at a time, so writes made through the pool never conflict
    /// with each other.
    func withWriter<T: Sendable>(
        for database: Database, _ work: @escaping @Sendable (Connection) throws -> T
    ) async throws -> T {
        let worker = state.withLock { state in
            if let writer = state.writer { return writer }
            let writer = Worker(label: "ai.relatedb.relate.writer", hooks: hooks)
            state.writer = writer
            return writer
        }
        return try await worker.perform(for: database, work)
    }

    /// Runs `work` on the least busy reader, opening another (up to the
    /// maximum) when every existing reader has work pending.
    func withReader<T: Sendable>(
        for database: Database, _ work: @escaping @Sendable (Connection) throws -> T
    ) async throws -> T {
        let (worker, slot) = state.withLock { state -> (Worker, Int) in
            let leastBusy = state.readers.indices.min {
                state.readers[$0].pending < state.readers[$1].pending
            }
            let slot: Int
            if let leastBusy, state.readers[leastBusy].pending == 0 || state.readers.count >= maxReaders {
                slot = leastBusy
            } else {
                let label = "ai.relatedb.relate.reader-\(state.readers.count)"
                state.readers.append((Worker(label: label, hooks: hooks), 0))
                slot = state.readers.count - 1
            }
            state.readers[slot].pending += 1
            return (state.readers[slot].worker, slot)
        }
        defer { state.withLock { $0.readers[slot].pending -= 1 } }
        return try await worker.perform(for: database, work)
    }
}

/// One connection confined to its own serial queue.
///
/// A `Connection` is not `Sendable`: calls on it must never overlap. The
/// queue is what guarantees that here — the connection is created, used and
/// replaced only inside blocks the queue runs — which is why this class can
/// be `@unchecked Sendable` while the connection it guards is not.
private final class Worker: @unchecked Sendable {
    private let queue: DispatchQueue
    private let hooks: ConnectionPool.Hooks
    /// Queue-confined.
    private var connection: Connection?

    init(label: String, hooks: ConnectionPool.Hooks) {
        self.queue = DispatchQueue(label: label)
        self.hooks = hooks
    }

    /// Runs `work` with the connection on this worker's queue and returns
    /// its result to the awaiting task. The task suspends; the queue's
    /// thread, not a cooperative-pool thread, blocks in the engine.
    /// `database` is held for the duration, so the connection's reference
    /// back to it stays valid.
    func perform<T: Sendable>(
        for database: Database, _ work: @escaping @Sendable (Connection) throws -> T
    ) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                continuation.resume(with: Result { try self.performOnQueue(for: database, work) })
            }
        }
    }

    private func performOnQueue<T>(for database: Database, _ work: (Connection) throws -> T) throws -> T {
        let connection = try current(for: database)
        do {
            return try work(connection)
        } catch let error as RelateError where error.engineError?.stage == .internal {
            // A boundary-stage failure may mean a panic poisoned the handle;
            // the probe decides. A poisoned connection is dropped, so the
            // next use opens a fresh one; the work that failed is rethrown
            // either way, never run again.
            if hooks.isPoisoned(connection) {
                self.connection = nil
            }
            throw error
        }
    }

    private func current(for database: Database) throws(RelateError) -> Connection {
        if let connection { return connection }
        let fresh = try hooks.open(database)
        connection = fresh
        return fresh
    }
}
