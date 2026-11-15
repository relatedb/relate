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
    LiveQueryRequest(identity: LiveQueryIdentity(id: AnyHashable(offset), tags: tags)) { tx in
        try counterValue(tx) + offset
    }
}

/// A counter request under `id` that counts the reads it runs.
private func countingRequest(
    id: AnyHashable = AnyHashable("counting"), _ reads: OSAllocatedUnfairLock<Int>
) -> LiveQueryRequest<Int> {
    LiveQueryRequest(identity: LiveQueryIdentity(id: id, tags: nil)) { tx in
        reads.withLock { $0 += 1 }
        return try counterValue(tx)
    }
}

/// A request whose read fails in the engine.
private let failingRequest = LiveQueryRequest<Int>(identity: LiveQueryIdentity(id: AnyHashable("failing"), tags: nil)) {
    tx in try tx.query("RETURN 1/0").scalar(as: Int.self)
}

@MainActor @Suite(.timeLimit(.minutes(1))) struct LiveQueryTests {
    // MARK: The observer

    @Test func anObserverPublishesTheValueAndFollowsCommits() async throws {
        let db = try counterDatabase()
        let request = counterRequest()
        let observer = LiveQueryObserver<Int>()
        #expect(observer.value == nil && observer.state(of: request.key(in: db)).isLoading)
        observer.observe(request, in: db)
        await waitUntil { observer.value == 0 }
        #expect(observer.state(of: request.key(in: db)).isLive)
        try await db.write(incrementCounter)
        await waitUntil { observer.value == 1 }
    }

    @Test func theSameRequestDoesNotStartAgain() async throws {
        let db = try counterDatabase()
        let reads = OSAllocatedUnfairLock(initialState: 0)
        let observer = LiveQueryObserver<Int>()
        observer.observe(countingRequest(reads), in: db)
        await waitUntil { observer.value == 0 }
        // Every view update hands the observer its request again.
        observer.observe(countingRequest(reads), in: db)
        observer.observe(countingRequest(reads), in: db)
        try await db.write(incrementCounter)
        await waitUntil { observer.value == 1 }
        // The first read and the one after the commit: a restart would have
        // read once more.
        #expect(reads.withLock { $0 } == 2)
    }

    @Test func aChangedRequestIsLoadingAtOnceAndKeepsTheLastValueUntilItsOwn() async throws {
        let db = try counterDatabase()
        let reads = OSAllocatedUnfairLock(initialState: 0)
        let observer = LiveQueryObserver<Int>()
        observer.observe(countingRequest(reads), in: db)
        await waitUntil { observer.value == 0 }

        let changed = counterRequest(offset: 100)
        observer.observe(changed, in: db)
        #expect(observer.state(of: changed.key(in: db)).isLoading && observer.value == 0)
        await waitUntil { observer.value == 100 }
        #expect(!observer.state(of: changed.key(in: db)).isLoading)
        // The earlier request is no longer followed.
        try await db.write(incrementCounter)
        await waitUntil { observer.value == 101 }
        #expect(reads.withLock { $0 } == 1)
    }

    @Test func changedTagsStartAgain() async throws {
        let db = try counterDatabase()
        let observer = LiveQueryObserver<Int>()
        observer.observe(counterRequest(), in: db)
        await waitUntil { observer.value == 0 }
        let tagged = counterRequest(tags: ["counter"])
        observer.observe(tagged, in: db)
        #expect(observer.state(of: tagged.key(in: db)).isLoading)
        await waitUntil { !observer.state(of: tagged.key(in: db)).isLoading }
        // The observation now follows only its tag.
        try await db.write(incrementCounter)
        try await db.write(incrementCounter, tags: ["counter"])
        await waitUntil { observer.value == 2 }
    }

    @Test func anotherDatabaseIsLoadingAtOnceAndStartsAgain() async throws {
        let request = counterRequest()
        let first = try counterDatabase()
        let observer = LiveQueryObserver<Int>()
        observer.observe(request, in: first)
        await waitUntil { observer.value == 0 }
        let other = try counterDatabase()
        try await other.write(incrementCounter)
        // The same query on another database is another observation: what
        // the first one settled says nothing about it.
        #expect(observer.state(of: request.key(in: other)).isLoading)
        observer.observe(request, in: other)
        await waitUntil { observer.value == 1 }
        #expect(!observer.state(of: request.key(in: other)).isLoading)
    }

    @Test func anIdThatDoesNotEqualItselfIsStillTheSameQuery() async throws {
        // NaN is not equal to itself, so an id made of one compares unequal
        // on every view update. Its hash is what identifies the query.
        let nan = AnyHashable(Double.nan)
        #expect(nan != AnyHashable(Double.nan))
        #expect(LiveQueryIdentity(id: nan, tags: nil) == LiveQueryIdentity(id: AnyHashable(Double.nan), tags: nil))
        #expect(LiveQueryIdentity(id: nan, tags: nil) != LiveQueryIdentity(id: AnyHashable(-Double.nan), tags: nil))

        let db = try counterDatabase()
        let reads = OSAllocatedUnfairLock(initialState: 0)
        let observer = LiveQueryObserver<Int>()
        observer.observe(countingRequest(id: nan, reads), in: db)
        await waitUntil { observer.value == 0 }
        observer.observe(countingRequest(id: AnyHashable(Double.nan), reads), in: db)
        try await db.write(incrementCounter)
        await waitUntil { observer.value == 1 }
        #expect(reads.withLock { $0 } == 2)
    }

    @Test func aReadEqualToTheValueHeldPublishesNothing() async throws {
        let db = try counterDatabase()
        let observer = LiveQueryObserver<Int>()
        var published: [Int?] = []
        let subscription = observer.$value.dropFirst().sink { published.append($0) }
        defer { subscription.cancel() }
        observer.observe(counterRequest(), in: db)
        await waitUntil { observer.value == 0 }
        // Another query that reads the same value.
        let same = countingRequest(OSAllocatedUnfairLock(initialState: 0))
        observer.observe(same, in: db)
        await waitUntil { !observer.state(of: same.key(in: db)).isLoading }
        #expect(published == [0])
    }

    @Test func aMissingDatabaseIsReportedAsMisuse() async throws {
        let request = counterRequest()
        let observer = LiveQueryObserver<Int>()
        observer.observe(request, in: nil)
        await waitUntil { !observer.state(of: request.key(in: nil)).isLoading }
        let error = try #require(observer.state(of: request.key(in: nil)).error as? RelateError)
        #expect(try misuseMessage(of: error).contains("relateDatabase"))
        #expect(observer.value == nil)
    }

    @Test func aFailedReadIsReportedAndTheLastValueStays() async throws {
        let db = try counterDatabase()
        let observer = LiveQueryObserver<Int>()
        observer.observe(counterRequest(), in: db)
        await waitUntil { observer.value == 0 }
        observer.observe(failingRequest, in: db)
        await waitUntil { !observer.state(of: failingRequest.key(in: db)).isLoading }
        let error = try #require(observer.state(of: failingRequest.key(in: db)).error as? RelateError)
        #expect(error.engineError?.stage == .runtime)
        #expect(observer.value == 0)
    }

    @Test func aFailedReadStopsListening() async throws {
        let db = try counterDatabase()
        let observer = LiveQueryObserver<Int>()
        observer.observe(failingRequest, in: db)
        await waitUntil { !observer.state(of: failingRequest.key(in: db)).isLoading }
        await db.commitSubscribersAreGone()
    }

    @Test func aStoppedObserverStopsListeningAndKeepsItsValue() async throws {
        let db = try counterDatabase()
        let observer = LiveQueryObserver<Int>()
        observer.observe(counterRequest(), in: db)
        await waitUntil { observer.value == 0 }
        observer.stop()
        await db.commitSubscribersAreGone()
        #expect(observer.value == 0)
        // The same request starts again after a stop.
        try await db.write(incrementCounter)
        observer.observe(counterRequest(), in: db)
        await waitUntil { observer.value == 1 }
    }

    @Test func aReleasedObserverStopsListening() async throws {
        let db = try counterDatabase()
        var observer: LiveQueryObserver<Int>? = LiveQueryObserver<Int>()
        try #require(observer).observe(counterRequest(), in: db)
        await waitUntil { observer?.value == 0 }
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
        let request = try query.request.get()
        #expect(query.initial.isEmpty && request.identity.tags == ["books"])
        #expect(try await db.read(request.read) == [Book(title: "Dune")])
    }

    @Test func aStatementQueryIsIdentifiedByItsTextAndItsBoundValues() throws {
        func query(_ prefix: String, _ limit: Double = 1) throws -> LiveQueryIdentity {
            try LiveQuery<[Book]>(
                try "MATCH (b:Book) WHERE b.title STARTS WITH \(prefix) RETURN b.title AS title LIMIT \(limit)"
            ).request.get().identity
        }
        #expect(try query("D") == query("D"))
        #expect(try query("D") != query("E"))
        // A bound NaN does not make the statement a stranger to itself.
        #expect(try query("D", .nan) == query("D", .nan))
        #expect(try query("D", .nan) != query("E", .nan))
    }

    @Test func aStatementThatCannotBeBuiltIsTheQuerysFailure() throws {
        // An unsigned integer above Int64.max has no Cypher form.
        let query = LiveQuery<[Book]>(try "MATCH (b:Book {n: \(UInt64.max)}) RETURN b.title AS title")
        let message = try invalidParameterMessage { _ = try query.request.get() }
        #expect(message.contains("\(UInt64.max)") && query.initial.isEmpty)
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
