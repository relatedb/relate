import Foundation
import Testing
import os

@testable import Relate

private struct Book: Decodable, Sendable, Equatable {
    let title: String
    let year: Int
}

/// Waits for a semaphore without parking a cooperative-pool thread; `false`
/// on timeout, so a broken test fails instead of hanging.
private func signalled(_ semaphore: DispatchSemaphore, within seconds: Double = 10) async -> Bool {
    await withCheckedContinuation { continuation in
        DispatchQueue.global().async {
            continuation.resume(returning: semaphore.wait(timeout: .now() + seconds) == .success)
        }
    }
}

/// A pool whose hooks count the connections it opens: the tests' view of
/// how many connections serve a workload, without the pool keeping
/// counters of its own.
private func countingPool(
    maxReaders: Int, isPoisoned: (@Sendable (Connection) -> Bool)? = nil
) -> (pool: ConnectionPool, opened: OSAllocatedUnfairLock<Int>) {
    let opened = OSAllocatedUnfairLock(initialState: 0)
    var hooks = ConnectionPool.Hooks(open: { (database: Database) throws(RelateError) in
        opened.withLock { $0 += 1 }
        return try ConnectionPool.Hooks.managed.open(database)
    })
    if let isPoisoned { hooks.isPoisoned = isPoisoned }
    return (ConnectionPool(maxReaders: maxReaders, hooks: hooks), opened)
}

/// Runs a statement the params builder refuses at the boundary stage (31
/// February): the stage a poisoned handle fails at, from a healthy one.
private let failAtTheBoundary: @Sendable (Connection) throws -> Void = { connection in
    try connection.query("RETURN $d", ["d": .date(.init(year: 2026, month: 2, day: 31))])
}

@Suite struct AsyncAccessTests {
    @Test func writesCommitAndReadsDecode() async throws {
        let db = try Database.inMemory()
        let created = try await db.write { tx in
            try tx.run("CREATE (:Book {title: \("Dune"), year: \(1965)})")
            try tx.run("CREATE (:Book {title: \("Emma"), year: \(1815)})")
            return try tx.query("MATCH (b:Book) RETURN count(b)").scalar(as: Int.self)
        }
        #expect(created == 2)
        let books = try await db.read { tx in
            try tx.run("MATCH (b:Book) RETURN b.title AS title, b.year AS year ORDER BY year").decode(Book.self)
        }
        #expect(books == [Book(title: "Emma", year: 1815), Book(title: "Dune", year: 1965)])
    }

    @Test func theStatementFormsWriteAndRead() async throws {
        let db = try Database.inMemory()
        let results = try await db.write([
            "CREATE (:Book {title: 'Dune', year: 1965})", "CREATE (:Book {title: 'Emma', year: 1815})",
        ])
        #expect(results.count == 2)
        let year = 1900
        let recent = try await db.read(try "MATCH (b:Book) WHERE b.year > \(year) RETURN b.title")
        #expect(recent.rows == [["Dune"]])
        let stats = try await db.write(try "MATCH (b:Book {title: \("Emma")}) SET b.read = true").stats
        #expect(stats.propertiesSet == 1)
    }

    @Test func aSchemaStatementIsAWrite() async throws {
        let db = try Database.inMemory()
        try await db.write("CREATE INDEX book_year FOR (b:Book) ON (b.year)")
        #expect(try await db.read("SHOW INDEXES").contains { $0["name"] == "book_year" })
    }

    @Test func aWriteThatThrowsIsRolledBack() async throws {
        let db = try Database.inMemory()
        struct Boom: Error {}
        await #expect(throws: Boom.self) {
            try await db.write { tx in
                try tx.query("CREATE (:Dropped)")
                throw Boom()
            }
        }
        #expect(try await db.read("MATCH (n:Dropped) RETURN count(n)").scalar() == 0)
    }

    @Test func aReadProceedsWhileAWriteIsOpen() async throws {
        let db = try Database.inMemory()
        let entered = DispatchSemaphore(value: 0)
        let release = DispatchSemaphore(value: 0)
        async let write: Void = db.write { tx in
            try tx.query("CREATE (:W)")
            entered.signal()
            #expect(release.wait(timeout: .now() + 10) == .success)
        }
        #expect(await signalled(entered))
        // The writer is parked inside its transaction; the read is neither
        // blocked by it nor shown its uncommitted node.
        let during = try await db.read("MATCH (w:W) RETURN count(w)").scalar(as: Int.self)
        #expect(during == 0)
        release.signal()
        try await write
        #expect(try await db.read("MATCH (w:W) RETURN count(w)").scalar() == 1)
    }

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

    @Test func aDatabaseNeedsAtLeastOneReader() throws {
        let message = try invalidParameterMessage {
            _ = try Database.inMemory(options: .init(maxReaders: 0))
        }
        #expect(message.contains("maxReaders 0"))
        // Every way of opening a store takes the options.
        try withTemporaryStoreFile { url in
            _ = try Database(path: url.path, options: .init(maxReaders: 1))
            #expect(throws: RelateError.self) { try Database(readOnlyPath: url.path, options: .init(maxReaders: 0)) }
        }
    }

    @Test func writesThroughThePoolNeverConflict() async throws {
        let db = try Database.inMemory()
        try await db.write("CREATE (:Counter {v: 0})")
        try await withThrowingTaskGroup(of: Void.self) { group in
            for _ in 0..<50 {
                group.addTask { try await db.write("MATCH (c:Counter) SET c.v = c.v + 1") }
            }
            try await group.waitForAll()
        }
        #expect(try await db.read("MATCH (c:Counter) RETURN c.v").scalar() == 50)
    }

    @Test func aConflictWithAnOutsideWriterIsRetried() async throws {
        let db = try Database.inMemory()
        try await db.write("CREATE (:Counter {v: 0})")
        let attempts = OSAllocatedUnfairLock(initialState: 0)
        let increment: Statement = "MATCH (c:Counter) SET c.v = c.v + 1 RETURN c.v"
        let seen = try await db.write(retries: 1) { tx in
            let attempt = attempts.withLock { state -> Int in
                state += 1
                return state
            }
            if attempt == 1 {
                // A connection outside the pool commits the same update first.
                try db.connect().run(increment)
            }
            return try tx.run(increment).scalar(as: Int.self)
        }
        #expect(seen == 2 && attempts.withLock { $0 } == 2)
    }

    @Test func aWriteInAManagedReadIsRefused() async throws {
        let db = try Database.inMemory()
        let message = try misuseMessage(
            of: try await #require(throws: RelateError.self) { try await db.read("CREATE (:Sneaky)") })
        #expect(message.contains("ran in a read"))
        #expect(try await db.read("MATCH (n:Sneaky) RETURN count(n)").scalar() == 0)
    }

    @Test func aReadOnlyDatabaseRefusesWritesAndCheckpoints() async throws {
        try await withTemporaryStoreFile { url in
            do {
                let seed = try Database(url: url)
                try await seed.write("CREATE (:P)")
            }
            let reader = try Database(url: url, readOnly: true)
            #expect(try await reader.read("MATCH (p:P) RETURN count(p)").scalar() == 1)
            let message = try misuseMessage(
                of: try await #require(throws: RelateError.self) { try await reader.write("CREATE (:P)") })
            #expect(message.contains("write") && message.contains("read-only"))
            let refused = try misuseMessage(
                of: try await #require(throws: RelateError.self) { try await reader.checkpointInBackground() })
            #expect(refused.contains("checkpoint"))
        }
    }

    @Test func aCancelledTaskDoesNotStartItsBody() async throws {
        let db = try Database.inMemory()
        let ran = OSAllocatedUnfairLock(initialState: false)
        let task = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            try await db.write { tx in
                ran.withLock { $0 = true }
                try tx.query("CREATE (:Late)")
            }
        }
        await #expect(throws: CancellationError.self) { try await task.value }
        #expect(!ran.withLock { $0 })
        #expect(try await db.read("MATCH (n:Late) RETURN count(n)").scalar() == 0)
    }

    @Test func releasingTheDatabaseClosesTheStore() async throws {
        // The pool's connections must not keep the Database (and so the
        // store's writer lease) alive: a second writable open succeeds once
        // the first Database is released.
        try await withTemporaryStoreFile { url in
            do {
                let db = try Database(url: url)
                try await db.write("CREATE (:Kept {v: 1})")
                _ = try await db.read("MATCH (k:Kept) RETURN k.v")
                try await db.checkpointInBackground()
                #expect(throws: RelateError.self) { try Database(url: url) }
            }
            let reopened = try Database(url: url)
            #expect(try await reopened.read("MATCH (k:Kept) RETURN k.v").scalar() == 1)
        }
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

    @Test func aManagedConnectionRefersBackToItsDatabase() async throws {
        let db = try Database.inMemory()
        let same = try await db.onReader { connection in connection.database === db }
        #expect(same)
    }
}
