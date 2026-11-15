#if canImport(SwiftUI)
import Combine
import Foundation
import SwiftUI
import Testing
import os

@testable import Relate

private struct Book: Decodable, Sendable, Equatable {
    let title: String
}

/// A request that reads the counter of a counter database, plus `offset`.
/// The offset is its id.
private func counterRequest(offset: Int = 0, tags: Set<String>? = nil) -> LiveQueryRequest<Int> {
    LiveQueryRequest(identity: LiveQueryIdentity(id: AnyHashable(offset), tags: tags), initial: -1) { tx in
        try counterValue(tx) + offset
    }
}

/// A counter request under `id` that counts the reads it runs.
private func countingRequest(
    id: AnyHashable = AnyHashable("counting"), _ reads: OSAllocatedUnfairLock<Int>
) -> LiveQueryRequest<Int> {
    LiveQueryRequest(identity: LiveQueryIdentity(id: id, tags: nil), initial: -1) { tx in
        reads.withLock { $0 += 1 }
        return try counterValue(tx)
    }
}

extension LiveQueryObserver {
    /// Waits until `condition` holds, letting the observation run in
    /// between. The suite's time limit bounds a condition that never holds.
    fileprivate func settles(_ condition: (LiveQueryObserver) -> Bool) async {
        while !condition(self) { await Task.yield() }
    }
}

@MainActor @Suite(.timeLimit(.minutes(1))) struct LiveQueryTests {
    // MARK: The observer

    @Test func anObserverPublishesTheValueAndFollowsCommits() async throws {
        let db = try counterDatabase()
        let request = counterRequest()
        let observer = LiveQueryObserver<Int>()
        #expect(observer.value == nil && observer.state(of: request.identity).isLoading)
        observer.observe(request, in: db)
        await observer.settles { $0.value == 0 }
        guard case .live = observer.state(of: request.identity) else {
            Issue.record("expected a live query, got \(observer.state(of: request.identity))")
            return
        }
        try await db.write(incrementCounter)
        await observer.settles { $0.value == 1 }
    }

    @Test func theSameRequestDoesNotStartAgain() async throws {
        let db = try counterDatabase()
        let reads = OSAllocatedUnfairLock(initialState: 0)
        let observer = LiveQueryObserver<Int>()
        observer.observe(countingRequest(reads), in: db)
        await observer.settles { $0.value == 0 }
        // Every view update hands the observer its request again.
        observer.observe(countingRequest(reads), in: db)
        observer.observe(countingRequest(reads), in: db)
        try await db.write(incrementCounter)
        await observer.settles { $0.value == 1 }
        // The first read and the one after the commit: a restart would have
        // read once more.
        #expect(reads.withLock { $0 } == 2)
    }

    @Test func aChangedRequestIsLoadingAtOnceAndKeepsTheLastValueUntilItsOwn() async throws {
        let db = try counterDatabase()
        let reads = OSAllocatedUnfairLock(initialState: 0)
        let observer = LiveQueryObserver<Int>()
        observer.observe(countingRequest(reads), in: db)
        await observer.settles { $0.value == 0 }

        let changed = counterRequest(offset: 100)
        observer.observe(changed, in: db)
        #expect(observer.state(of: changed.identity).isLoading && observer.value == 0)
        await observer.settles { $0.value == 100 }
        #expect(!observer.state(of: changed.identity).isLoading)
        // The earlier request is no longer followed.
        try await db.write(incrementCounter)
        await observer.settles { $0.value == 101 }
        #expect(reads.withLock { $0 } == 1)
    }

    @Test func changedTagsStartAgain() async throws {
        let db = try counterDatabase()
        let observer = LiveQueryObserver<Int>()
        observer.observe(counterRequest(), in: db)
        await observer.settles { $0.value == 0 }
        let tagged = counterRequest(tags: ["counter"])
        observer.observe(tagged, in: db)
        #expect(observer.state(of: tagged.identity).isLoading)
        await observer.settles { !$0.state(of: tagged.identity).isLoading }
        // The observation now follows only its tag.
        try await db.write(incrementCounter)
        try await db.write(incrementCounter, tags: ["counter"])
        await observer.settles { $0.value == 2 }
    }

    @Test func anotherDatabaseStartsAgain() async throws {
        let observer = LiveQueryObserver<Int>()
        observer.observe(counterRequest(), in: try counterDatabase())
        await observer.settles { $0.value == 0 }
        let other = try counterDatabase()
        try await other.write(incrementCounter)
        observer.observe(counterRequest(), in: other)
        await observer.settles { $0.value == 1 }
    }

    @Test func anIdThatDoesNotEqualItselfIsStillTheSameQuery() async throws {
        // NaN is not equal to itself, so an id made of one compares unequal
        // on every view update.
        let nan = AnyHashable(Double.nan)
        #expect(nan != AnyHashable(Double.nan))
        let db = try counterDatabase()
        let reads = OSAllocatedUnfairLock(initialState: 0)
        let observer = LiveQueryObserver<Int>()
        observer.observe(countingRequest(id: nan, reads), in: db)
        await observer.settles { $0.value == 0 }
        observer.observe(countingRequest(id: nan, reads), in: db)
        #expect(!observer.state(of: countingRequest(id: nan, reads).identity).isLoading)
        try await db.write(incrementCounter)
        await observer.settles { $0.value == 1 }
        #expect(reads.withLock { $0 } == 2)
        // Another such id is another query.
        #expect(LiveQueryIdentity(id: nan, tags: nil) != LiveQueryIdentity(id: AnyHashable(-Double.nan), tags: nil))
    }

    @Test func aReadEqualToTheValueHeldPublishesNothing() async throws {
        let db = try counterDatabase()
        let observer = LiveQueryObserver<Int>()
        var published: [Int?] = []
        let subscription = observer.$value.dropFirst().sink { published.append($0) }
        defer { subscription.cancel() }
        observer.observe(counterRequest(), in: db)
        await observer.settles { $0.value == 0 }
        // Another query that reads the same value.
        let same = countingRequest(OSAllocatedUnfairLock(initialState: 0))
        observer.observe(same, in: db)
        await observer.settles { !$0.state(of: same.identity).isLoading }
        #expect(published == [0])
    }

    @Test func aMissingDatabaseIsReportedAsMisuse() async throws {
        let request = counterRequest()
        let observer = LiveQueryObserver<Int>()
        observer.observe(request, in: nil)
        await observer.settles { !$0.state(of: request.identity).isLoading }
        let error = try #require(observer.state(of: request.identity).error as? RelateError)
        #expect(try misuseMessage(of: error).contains("relateDatabase"))
        #expect(observer.value == nil)
    }

    @Test func aFailedReadIsReportedTheLastValueStaysAndItStopsListening() async throws {
        let db = try counterDatabase()
        let observer = LiveQueryObserver<Int>()
        observer.observe(counterRequest(), in: db)
        await observer.settles { $0.value == 0 }
        let failing = LiveQueryRequest<Int>(
            identity: LiveQueryIdentity(id: AnyHashable("failing"), tags: nil), initial: -1
        ) { tx in try tx.query("RETURN 1/0").scalar(as: Int.self) }
        observer.observe(failing, in: db)
        await observer.settles { !$0.state(of: failing.identity).isLoading }
        let error = try #require(observer.state(of: failing.identity).error as? RelateError)
        #expect(error.engineError?.stage == .runtime)
        #expect(observer.value == 0)
        await db.commitSubscribersAreGone()
    }

    @Test func aReleasedObserverStopsListening() async throws {
        let db = try counterDatabase()
        var observer: LiveQueryObserver<Int>? = LiveQueryObserver<Int>()
        try #require(observer).observe(counterRequest(), in: db)
        await observer?.settles { $0.value == 0 }
        #expect(db.core.hub.subscriberCount == 1)
        observer = nil  // the view went away: its observation is cancelled
        await db.commitSubscribersAreGone()
    }

    // MARK: The statement form

    @Test func aStatementQueryDecodesItsRecords() async throws {
        let db = try Database.inMemory()
        try await db.write("CREATE (:Book {title: 'Dune'}), (:Book {title: 'Emma'})")
        let query = LiveQuery<[Book]>(
            try "MATCH (b:Book) WHERE b.title STARTS WITH \("D") RETURN b.title AS title", tags: ["books"])
        #expect(query.request.initial.isEmpty && query.request.identity.tags == ["books"])
        #expect(try await db.read(query.request.read) == [Book(title: "Dune")])
    }

    @Test func aStatementQueryIsIdentifiedByItsTextAndItsBoundValues() throws {
        func query(_ prefix: String, _ limit: Double = 1) throws -> LiveQueryIdentity {
            LiveQuery<[Book]>(
                try "MATCH (b:Book) WHERE b.title STARTS WITH \(prefix) RETURN b.title AS title LIMIT \(limit)"
            ).request.identity
        }
        #expect(try query("D") == query("D"))
        #expect(try query("D") != query("E"))
        // A bound NaN does not make the statement a stranger to itself.
        #expect(try query("D", .nan) == query("D", .nan))
        #expect(try query("D", .nan) != query("E", .nan))
    }

    @Test func aStatementThatCannotBeBuiltIsReportedByItsRead() throws {
        // An unsigned integer above Int64.max has no Cypher form.
        let query = LiveQuery<[Book]>(try "MATCH (b:Book {n: \(UInt64.max)}) RETURN b.title AS title")
        let conn = try Database.inMemory().connect()
        let message = try invalidParameterMessage { _ = try conn.read(query.request.read) }
        #expect(message.contains("\(UInt64.max)"))
    }

    // MARK: The environment

    @Test func theEnvironmentCarriesTheDatabase() throws {
        var environment = EnvironmentValues()
        #expect(environment.relateDatabase == nil)
        let db = try Database.inMemory()
        environment.relateDatabase = db
        #expect(environment.relateDatabase === db)
    }
}
#endif
