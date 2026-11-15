import Foundation
import Testing
import os

@testable import Relate

/// Installs a trace handler on `db` that keeps every trace it is given.
private func collectingTraces(of db: Database) -> OSAllocatedUnfairLock<[QueryTrace]> {
    let traces = OSAllocatedUnfairLock(initialState: [QueryTrace]())
    db.setTraceHandler { trace in traces.withLock { $0.append(trace) } }
    return traces
}

@Suite struct ObservationTests {
    private func bookmarkNumber(_ bookmark: String) throws -> Int {
        let digits = try #require(bookmark.split(separator: ":").last)
        return try #require(Int(digits))
    }

    // MARK: Tracing

    @Test func everyStatementIsTracedWithoutItsValues() async throws {
        let db = try Database.inMemory()
        let traces = collectingTraces(of: db)
        let conn = try db.connect()
        let secret = "s3cret-value"
        try conn.query("CREATE (:P {name: $name, n: $n})", ["name": secret, "n": 1])
        _ = try conn.query("MATCH (p:P) RETURN p.name")
        // Statements run by the managed connections are traced too.
        _ = try await db.read(try "MATCH (p:P {name: \(secret)}) RETURN p.n")

        let seen = traces.withLock { $0 }
        #expect(seen.count == 3)
        #expect(seen[0].statement == "CREATE (:P {name: $name, n: $n})")
        #expect(seen[0].parameterNames == ["n", "name"] && seen[0].queryType == .write)
        #expect(seen[0].rowCount == 0 && seen[0].error == nil)
        #expect(seen[1].rowCount == 1 && seen[1].queryType == .read && seen[1].parameterNames.isEmpty)
        #expect(seen[1].elapsedMs > 0 && seen[1].wallMs >= seen[1].elapsedMs)
        #expect(seen[1].parseMs + seen[1].planMs + seen[1].executeMs <= seen[1].elapsedMs)
        #expect(seen[2].statement == "MATCH (p:P {name: $_p0}) RETURN p.n" && seen[2].parameterNames == ["_p0"])
        // A bound value appears nowhere in a trace.
        #expect(!seen.contains { "\($0)".contains(secret) })
    }

    @Test func aFailedStatementIsTracedWithItsError() throws {
        let db = try Database.inMemory()
        let traces = collectingTraces(of: db)
        #expect(throws: RelateError.self) { try db.connect().query("RETURN 1/0") }
        let seen = traces.withLock { $0 }
        let failed = try #require(seen.first)
        #expect(seen.count == 1 && failed.statement == "RETURN 1/0")
        #expect(failed.error?.engineError?.stage == .runtime && failed.queryType == nil)
        #expect(failed.rowCount == 0 && failed.elapsedMs == 0 && failed.wallMs > 0)
    }

    @Test func theHealthProbeIsNotTraced() throws {
        // The probe is the package's own statement, not the caller's.
        let db = try Database.inMemory()
        let traces = collectingTraces(of: db)
        #expect(!(try db.connect().isPoisoned()))
        #expect(traces.withLock { $0.isEmpty })
    }

    @Test func clearingTheHandlerStopsTracing() throws {
        let db = try Database.inMemory()
        let traces = collectingTraces(of: db)
        let conn = try db.connect()
        _ = try conn.query("RETURN 1")
        db.setTraceHandler(nil)
        _ = try conn.query("RETURN 2")
        #expect(traces.withLock { $0.map(\.statement) } == ["RETURN 1"])
    }

    // MARK: Commits

    @Test func autoCommitWritesAreReportedAndReadsAreNot() async throws {
        let db = try Database.inMemory()
        var commits = db.commits().makeAsyncIterator()
        let conn = try db.connect()
        let created = try conn.query("CREATE (:A {x: 1}), (:A)")
        _ = try conn.query("MATCH (a:A) RETURN a")
        _ = try conn.query("MATCH (a:Nobody) SET a.x = 1")  // a write that changes nothing
        let removed = try conn.query("MATCH (a:A) DETACH DELETE a")

        let first = try #require(await commits.next())
        #expect(first.bookmark == created.bookmark && first.tags.isEmpty)
        #expect(first.stats.nodesCreated == 2 && first.stats.propertiesSet == 1)
        // The read and the no-op write produced no event: the next is the delete.
        let second = try #require(await commits.next())
        #expect(second.bookmark == removed.bookmark && second.stats.nodesDeleted == 2)
        #expect(try bookmarkNumber(second.bookmark) > bookmarkNumber(first.bookmark))
    }

    @Test func aTransactionIsOneCommitWithSummedCountersAndTags() async throws {
        let db = try Database.inMemory()
        var commits = db.commits().makeAsyncIterator()
        try db.connect().transaction(tags: ["books", "import"]) { tx in
            try tx.query("CREATE (:Book {title: 'Dune'})")
            try tx.query("CREATE (:Book {title: 'Emma'})-[:BY]->(:Author {name: 'Austen'})")
        }
        let commit = try #require(await commits.next())
        #expect(commit.tags == ["books", "import"])
        #expect(commit.stats.nodesCreated == 3 && commit.stats.relationshipsCreated == 1)
        #expect(commit.stats.propertiesSet == 3 && commit.stats.labelsAdded == 3)
    }

    @Test func aTransactionThatKeepsNothingReportsNothing() async throws {
        let db = try Database.inMemory()
        var commits = db.commits().makeAsyncIterator()
        let conn = try db.connect()
        struct Boom: Error {}
        #expect(throws: Boom.self) {
            try conn.transaction(tags: ["dropped"]) { tx in
                try tx.query("CREATE (:Dropped)")
                throw Boom()
            }
        }
        try conn.transaction(tags: ["empty"]) { tx in _ = try tx.query("RETURN 1") }
        #expect(try conn.read { tx in try tx.query("MATCH (n) RETURN count(n)").scalar(as: Int.self) } == 0)
        // The first event is the write made after all three, and none of
        // their tags leaked into it.
        try conn.query("CREATE (:Plain)")
        let commit = try #require(await commits.next())
        #expect(commit.tags.isEmpty && commit.stats.nodesCreated == 1)
    }

    @Test func aStatementListIsOneTaggedCommit() async throws {
        let db = try Database.inMemory()
        var commits = db.commits().makeAsyncIterator()
        let batch = try db.connect().runInTransaction(["CREATE (:Shelf)", "CREATE (:Shelf)"], tags: ["shelves"])
        #expect(batch.allSatisfy { $0.bookmark == nil })  // statements inside a transaction carry none
        let commit = try #require(await commits.next())
        #expect(commit.tags == ["shelves"] && commit.stats.nodesCreated == 2)
    }

    @Test func managedWritesReportToEverySubscriber() async throws {
        let db = try Database.inMemory()
        let subscribers = [db.commits(), db.commits()]
        try await db.write(tags: ["notes"]) { tx in try tx.query("CREATE (:Note {text: 'a'})") }
        try await db.write("CREATE (:Note {text: 'b'})", tags: ["notes", "quick"])
        #expect(try await db.read("MATCH (n:Note) RETURN count(n)").scalar() == 2)
        for subscriber in subscribers {
            var commits = subscriber.makeAsyncIterator()
            #expect(try #require(await commits.next()).tags == ["notes"])
            #expect(try #require(await commits.next()).tags == ["notes", "quick"])
        }
    }

    @Test func aStreamStartsAtItsCreation() async throws {
        let db = try Database.inMemory()
        try await db.write("CREATE (:Note {text: 'before'})", tags: ["early"])
        var commits = db.commits().makeAsyncIterator()
        try await db.write("CREATE (:Note {text: 'after'})", tags: ["late"])
        #expect(try #require(await commits.next()).tags == ["late"])
    }

    @Test func aRetriedTransactionReportsOnce() async throws {
        let (winner, loser) = try counterRacers()
        var commits = winner.database.commits().makeAsyncIterator()
        var attempts = 0
        try loser.transaction(retries: 1, tags: ["retry"]) { tx in
            attempts += 1
            if attempts == 1 { try winner.run(incrementCounter) }
            try tx.run(incrementCounter)
        }
        // The winner's auto-commit, then the loser's one successful attempt.
        #expect(try #require(await commits.next()).tags.isEmpty)
        let retried = try #require(await commits.next())
        #expect(retried.tags == ["retry"] && retried.stats.propertiesSet == 1 && attempts == 2)
    }

    @Test func aStatementAfterAFailedCommitIsItsOwnCommit() async throws {
        let (winner, loser) = try counterRacers()
        var commits = winner.database.commits().makeAsyncIterator()
        try loser.begin()
        try winner.run(incrementCounter)
        try loser.run(incrementCounter)
        #expect(throws: RelateError.self) { try loser.commit() }
        // The lost transaction's counters do not join the next commit.
        try loser.query("CREATE (:After)")
        #expect(try #require(await commits.next()).stats.propertiesSet == 1)  // the winner's
        let after = try #require(await commits.next())
        #expect(after.stats.nodesCreated == 1 && after.stats.propertiesSet == 0)
    }

    @Test func aCommitIsPublishedBeforeItsStatementIsTraced() async throws {
        let db = try Database.inMemory()
        let commits = db.commits()
        let received = DispatchSemaphore(value: 0)
        let consumer = Task {
            for await _ in commits {
                received.signal()
                return
            }
        }
        // The handler runs on the writer's queue, where it may block: it
        // waits for the consumer to have been handed the commit.
        let publishedFirst = OSAllocatedUnfairLock(initialState: false)
        db.setTraceHandler { _ in
            let arrived = received.wait(timeout: .now() + 10) == .success
            publishedFirst.withLock { $0 = arrived }
        }
        _ = try await db.pool.withWriter(for: db) { try $0.query("CREATE (:A)") }
        await consumer.value
        #expect(publishedFirst.withLock { $0 })
    }

    @Test func aBoundedStreamKeepsTheNewestCommits() async throws {
        let db = try Database.inMemory()
        var commits = db.commits(bufferingPolicy: .bufferingNewest(1)).makeAsyncIterator()
        let conn = try db.connect()
        try conn.query("CREATE (:A)")
        try conn.query("CREATE (:B), (:B)")
        let latest = try conn.query("CREATE (:C), (:C), (:C)")
        #expect(try #require(await commits.next()).bookmark == latest.bookmark)
    }

    @Test func aReleasedStreamUnsubscribes() throws {
        let db = try Database.inMemory()
        withExtendedLifetime(db.commits()) {
            #expect(db.core.hub.subscriberCount == 1)
        }
        // The stream was released without being consumed.
        #expect(db.core.hub.subscriberCount == 0)
    }

    @Test func closingTheStoreEndsItsStreams() async throws {
        var db: Database? = try Database.inMemory()
        var commits = try #require(db).commits().makeAsyncIterator()
        try await #require(db).write("CREATE (:Last)")
        #expect(await commits.next() != nil)
        db = nil  // the store closes: the stream ends rather than hanging
        #expect(await commits.next() == nil)
    }
}
