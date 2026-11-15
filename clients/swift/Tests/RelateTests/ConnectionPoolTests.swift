import Foundation
import Testing
import os

@testable import Relate

/// A pool whose hooks count the connections it opens: the tests' view of
/// how many connections serve a workload, without the pool keeping
/// counters of its own.
private func countingPool(
    maxReaders: Int, isPoisoned: @escaping @Sendable (Connection) -> Bool = ConnectionPool.Hooks.managed.isPoisoned
) -> (pool: ConnectionPool, opened: OSAllocatedUnfairLock<Int>) {
    let opened = OSAllocatedUnfairLock(initialState: 0)
    let hooks = ConnectionPool.Hooks(
        open: { (database: Database) throws(RelateError) in
            opened.withLock { $0 += 1 }
            return try ConnectionPool.Hooks.managed.open(database)
        }, isPoisoned: isPoisoned)
    return (ConnectionPool(maxReaders: maxReaders, hooks: hooks), opened)
}

/// Runs a statement the params builder refuses at the boundary stage (31
/// February): the stage a poisoned handle fails at, from a healthy one.
private let failAtTheBoundary: @Sendable (Connection) throws -> Void = { connection in
    try connection.query("RETURN $d", ["d": .date(.init(year: 2026, month: 2, day: 31))])
}

@Suite struct ConnectionPoolTests {
    @Test func readsRunConcurrentlyUpToTheReaderLimit() async throws {
        let db = try Database.inMemory()
        let (pool, opened) = countingPool(maxReaders: 2)
        let entered = DispatchSemaphore(value: 0)
        let release = DispatchSemaphore(value: 0)
        let parked: @Sendable (Connection) throws -> Int = { connection in
            entered.signal()
            #expect(release.wait(timeout: .now() + 10) == .success)
            return try connection.query("RETURN 1").scalar(as: Int.self)
        }
        async let first = pool.withReader(for: db, parked)
        async let second = pool.withReader(for: db, parked)
        // Both bodies are inside at once only if two connections serve them.
        #expect(await signalled(entered))
        #expect(await signalled(entered))
        #expect(opened.withLock { $0 } == 2)
        release.signal()
        release.signal()
        #expect(try await first + second == 2)
        // Further reads reuse the two readers.
        for _ in 0..<10 { _ = try await pool.withReader(for: db) { try $0.query("RETURN 1") } }
        #expect(opened.withLock { $0 } == 2)
    }

    @Test func aSingleReaderServesReadsInTurn() async throws {
        let db = try Database.inMemory()
        let (pool, opened) = countingPool(maxReaders: 1)
        let total = try await withThrowingTaskGroup(of: Int.self) { group in
            for i in 0..<20 {
                group.addTask {
                    try await pool.withReader(for: db) { try $0.query("RETURN $i", ["i": i]).scalar(as: Int.self) }
                }
            }
            return try await group.reduce(0, +)
        }
        #expect(total == (0..<20).reduce(0, +))
        #expect(opened.withLock { $0 } == 1)
    }

    // The engine's panic is not something a test can order up, so the next
    // two tests inject the probe: the pool is told the connection is
    // poisoned after a real boundary-stage failure.

    @Test func aPoisonedReaderIsReplacedAndTheWorkIsNotRetried() async throws {
        let db = try Database.inMemory()
        let (pool, opened) = countingPool(maxReaders: 1, isPoisoned: { _ in true })
        let runs = OSAllocatedUnfairLock(initialState: 0)
        #expect(try await pool.withReader(for: db) { try $0.query("RETURN 1").scalar(as: Int.self) } == 1)
        #expect(opened.withLock { $0 } == 1)
        let error = try await #require(throws: RelateError.self) {
            try await pool.withReader(for: db) { connection in
                runs.withLock { $0 += 1 }
                try failAtTheBoundary(connection)
            }
        }
        #expect(error.engineError?.stage == .internal)
        // Rethrown, run once, and the next use opens a fresh connection.
        #expect(runs.withLock { $0 } == 1)
        #expect(try await pool.withReader(for: db) { try $0.query("RETURN 2").scalar(as: Int.self) } == 2)
        #expect(opened.withLock { $0 } == 2)
    }

    @Test func aPoisonedWriterIsReplaced() async throws {
        let db = try Database.inMemory()
        let (pool, opened) = countingPool(maxReaders: 1, isPoisoned: { _ in true })
        _ = try await pool.withWriter(for: db) { try $0.query("CREATE (:W)") }
        await #expect(throws: RelateError.self) { try await pool.withWriter(for: db, failAtTheBoundary) }
        _ = try await pool.withWriter(for: db) { try $0.query("CREATE (:W)") }
        #expect(opened.withLock { $0 } == 2)
        #expect(try await db.read("MATCH (w:W) RETURN count(w)").scalar() == 2)
    }

    @Test func aRealProbeLeavesAHealthyPooledConnectionAlone() async throws {
        let db = try Database.inMemory()
        let (pool, opened) = countingPool(maxReaders: 1)
        await #expect(throws: RelateError.self) { try await pool.withReader(for: db, failAtTheBoundary) }
        _ = try await pool.withReader(for: db) { try $0.query("RETURN 1") }
        #expect(opened.withLock { $0 } == 1)
    }
}
