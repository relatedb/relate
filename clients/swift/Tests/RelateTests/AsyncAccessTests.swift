import Foundation
import Testing
import os

@testable import Relate

private struct Book: Decodable, Sendable, Equatable {
    let title: String
    let year: Int
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

    @Test func aTaskCancelledWhileItWaitsDoesNotStartItsBody() async throws {
        let db = try Database.inMemory()
        let entered = DispatchSemaphore(value: 0)
        let release = DispatchSemaphore(value: 0)
        async let parked: Void = db.write { _ in
            entered.signal()
            #expect(release.wait(timeout: .now() + 10) == .success)
        }
        #expect(await signalled(entered))
        // The writer is busy, so this write can only wait its turn; it is
        // cancelled before that turn comes.
        let ran = OSAllocatedUnfairLock(initialState: false)
        let waiting = Task {
            try await db.write { tx in
                ran.withLock { $0 = true }
                try tx.query("CREATE (:Late)")
            }
        }
        waiting.cancel()
        release.signal()
        try await parked
        await #expect(throws: CancellationError.self) { try await waiting.value }
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

    @Test func aManagedConnectionRefersBackToItsDatabase() async throws {
        let db = try Database.inMemory()
        let same = try await db.pool.withReader(for: db) { connection in connection.database === db }
        #expect(same)
    }
}
