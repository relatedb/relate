import Foundation
import Testing
import os

@testable import Relate

extension Database {
    /// Waits until nothing is subscribed to this database's commits: how a
    /// test sees that an observation has let go. Cancellation reaches the
    /// subscription off the test's own task, so this waits rather than
    /// asserts; the suite's time limit bounds a subscription that stays.
    func commitSubscribersAreGone() async {
        while core.hub.subscriberCount != 0 { await Task.yield() }
    }
}

/// A failure of these tests is a value that never arrives, so every test
/// is bounded instead of left to hang.
@Suite(.timeLimit(.minutes(1))) struct LiveValuesTests {
    @Test func theCurrentValueComesFirstThenOnePerChange() async throws {
        let db = try counterDatabase()
        var values = db.values(counterValue).makeAsyncIterator()
        #expect(try await values.next() == 0)
        try await db.write(incrementCounter)
        #expect(try await values.next() == 1)
    }

    @Test func aCallersOwnConnectionIsFollowedToo() async throws {
        let db = try counterDatabase()
        var values = db.values(counterValue).makeAsyncIterator()
        #expect(try await values.next() == 0)
        try db.connect().run(incrementCounter)
        #expect(try await values.next() == 1)
    }

    @Test func aWriteMadeBeforeTheFirstValueIsConsumedIsSeen() async throws {
        let db = try counterDatabase()
        let values = db.values(counterValue)
        try await db.write(incrementCounter)
        // Whether the first read ran before or after the write, the value
        // that settles is the written one.
        for try await value in values where value == 1 { return }
        Issue.record("the stream ended without the written value")
    }

    @Test func aTaggedObservationFollowsOnlyItsTags() async throws {
        let db = try counterDatabase()
        let reads = OSAllocatedUnfairLock(initialState: 0)
        var values = db.values(tags: ["counter"]) { tx in
            reads.withLock { $0 += 1 }
            return try counterValue(tx)
        }.makeAsyncIterator()
        #expect(try await values.next() == 0)
        try await db.write(incrementCounter, tags: ["other"])
        try db.connect().run(incrementCounter)  // an auto-commit statement carries no tag
        try await db.write(incrementCounter, tags: ["counter", "more"])
        #expect(try await values.next() == 3)
        #expect(reads.withLock { $0 } == 2)
    }

    @Test func aReadThatRepeatsTheLastValueYieldsNothing() async throws {
        let db = try counterDatabase()
        let reads = OSAllocatedUnfairLock(initialState: 0)
        let values = db.values { tx in
            let value = try counterValue(tx)
            reads.withLock { $0 += 1 }
            return value
        }
        // A consumer that is always waiting receives every value yielded.
        let received = OSAllocatedUnfairLock(initialState: [Int]())
        let consumer = Task {
            for try await value in values {
                received.withLock { $0.append(value) }
                if value == 1 { return }
            }
        }
        while received.withLock({ $0 }).isEmpty { await Task.yield() }
        // This commit leaves the counter alone: its read returns a second 0.
        try await db.write("CREATE (:Unrelated)")
        while reads.withLock({ $0 }) < 2 { await Task.yield() }
        try await db.write(incrementCounter)
        try await consumer.value
        #expect(received.withLock { $0 } == [0, 1])
    }

    @Test func aValueThatIsNotEquatableIsYieldedAfterEveryCommit() async throws {
        struct Box: Sendable { let value: Int }
        let db = try counterDatabase()
        var values = db.values { tx in Box(value: try counterValue(tx)) }.makeAsyncIterator()
        #expect(try await values.next()?.value == 0)
        try await db.write("CREATE (:Unrelated)")
        #expect(try await values.next()?.value == 0)
    }

    @Test func commitsDuringAReadAreNotFollowedByOneReadEach() async throws {
        let db = try counterDatabase()
        let reads = OSAllocatedUnfairLock(initialState: 0)
        let entered = DispatchSemaphore(value: 0)
        let release = DispatchSemaphore(value: 0)
        let values = db.values { tx in
            let isFirst = reads.withLock { reads -> Bool in
                reads += 1
                return reads == 1
            }
            if isFirst {
                entered.signal()
                #expect(release.wait(timeout: .now() + 10) == .success)
            }
            return try counterValue(tx)
        }
        // The first read is parked; twenty commits arrive meanwhile.
        #expect(await signalled(entered))
        let commits = 20
        for _ in 0..<commits { try await db.write(incrementCounter) }
        release.signal()
        for try await value in values where value == commits { break }
        // One read per commit would be the first plus twenty.
        #expect(reads.withLock { $0 } < 1 + commits)
    }

    @Test func aFailedReadEndsTheStreamWithItsErrorAndStopsListening() async throws {
        let db = try Database.inMemory()
        let values = db.values { tx in try tx.query("RETURN 1/0").scalar(as: Int.self) }
        let error = try await #require(throws: RelateError.self) {
            for try await value in values { Issue.record("a failed read yielded \(value)") }
        }
        #expect(error.engineError?.stage == .runtime)
        await db.commitSubscribersAreGone()
    }

    @Test func theStreamEndsWhenTheStoreCloses() async throws {
        var db: Database? = try counterDatabase()
        var values = try #require(db).values(counterValue).makeAsyncIterator()
        #expect(try await values.next() == 0)
        db = nil  // the observation did not keep the store open
        #expect(try await values.next() == nil)
    }

    @Test func aReleasedStreamStopsListening() async throws {
        let db = try counterDatabase()
        do {
            var values = db.values(counterValue).makeAsyncIterator()
            #expect(try await values.next() == 0)
            #expect(db.core.hub.subscriberCount == 1)
        }
        // Releasing the stream cancels its reads, which releases the commit
        // subscription; both happen off this task.
        await db.commitSubscribersAreGone()
    }
}
