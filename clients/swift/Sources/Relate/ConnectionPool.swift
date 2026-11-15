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
            try database.connectManaged()
        })
    }

    /// A reader and the number of calls on it that have not returned.
    private struct Reader {
        let worker: Worker
        var pending = 0
    }

    private struct State {
        var writer: Worker?
        var readers: [Reader] = []

        /// Takes a reader for one call: an idle one when there is one,
        /// otherwise a new one while fewer than `maxReaders` exist,
        /// otherwise the least busy.
        mutating func acquireReader(maxReaders: Int, hooks: Hooks) -> Worker {
            let leastBusy = readers.indices.min { readers[$0].pending < readers[$1].pending }
            let hasIdle = leastBusy.map { readers[$0].pending == 0 } ?? false
            let atCapacity = readers.count >= maxReaders
            let slot: Int
            if let leastBusy, hasIdle || atCapacity {
                slot = leastBusy
            } else {
                let label = "ai.relatedb.relate.reader-\(readers.count)"
                readers.append(Reader(worker: Worker(label: label, hooks: hooks)))
                slot = readers.count - 1
            }
            readers[slot].pending += 1
            return readers[slot].worker
        }

        /// Gives back the reader `worker` is. It is found by identity, so
        /// the count returns to the reader it was taken from wherever that
        /// reader now sits in the list.
        mutating func releaseReader(_ worker: Worker) {
            guard let slot = readers.firstIndex(where: { $0.worker === worker }) else { return }
            readers[slot].pending -= 1
        }
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

    /// Runs `work` on a reader: an idle one, another opened for the call
    /// (up to the maximum), or the least busy when every reader has work
    /// pending.
    func withReader<T: Sendable>(
        for database: Database, _ work: @escaping @Sendable (Connection) throws -> T
    ) async throws -> T {
        let worker = state.withLock { $0.acquireReader(maxReaders: maxReaders, hooks: hooks) }
        defer { state.withLock { $0.releaseReader(worker) } }
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
    ///
    /// Cancellation is decided here, where the work starts: a task
    /// cancelled before the call, or while the work waited its turn on the
    /// queue, throws `CancellationError` when that turn comes, without
    /// touching the connection. Work that has started runs to its end.
    func perform<T: Sendable>(
        for database: Database, _ work: @escaping @Sendable (Connection) throws -> T
    ) async throws -> T {
        let cancelled = OSAllocatedUnfairLock(initialState: false)
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                queue.async {
                    continuation.resume(with: Result {
                        if cancelled.withLock({ $0 }) { throw CancellationError() }
                        return try self.performOnQueue(for: database, work)
                    })
                }
            }
        } onCancel: {
            cancelled.withLock { $0 = true }
        }
    }

    private func performOnQueue<T>(for database: Database, _ work: (Connection) throws -> T) throws -> T {
        let connection = try current(for: database)
        do {
            return try work(connection)
        } catch let error as RelateError where error.isBoundaryFailure {
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
