import Foundation
import Testing

@testable import Relate

/// One policy for a throwaway on-disk item: a UUID-named path under the
/// system temp, removed when the body returns. `suffix` picks the store
/// form — none for a store directory, `.rl` for a single-file store.
private func temporaryItemURL(suffix: String) -> URL {
    FileManager.default.temporaryDirectory
        .appendingPathComponent("relate-swift-\(UUID().uuidString)\(suffix)")
}

private func withTemporaryItem(suffix: String, _ body: (URL) throws -> Void) throws {
    let url = temporaryItemURL(suffix: suffix)
    defer { try? FileManager.default.removeItem(at: url) }
    try body(url)
}

/// The same policy for a body that may suspend.
private func withTemporaryItem<T>(suffix: String, _ body: (URL) async throws -> T) async throws -> T {
    let url = temporaryItemURL(suffix: suffix)
    defer { try? FileManager.default.removeItem(at: url) }
    return try await body(url)
}

/// A throwaway store directory, by path.
func withTemporaryStore(_ body: (String) throws -> Void) throws {
    try withTemporaryItem(suffix: "") { try body($0.path) }
}

/// A throwaway single-file `.rl` store, by file URL.
func withTemporaryStoreFile(_ body: (URL) throws -> Void) throws {
    try withTemporaryItem(suffix: ".rl", body)
}

/// A throwaway single-file `.rl` store, for a body that may suspend.
func withTemporaryStoreFile<T>(_ body: (URL) async throws -> T) async throws -> T {
    try await withTemporaryItem(suffix: ".rl", body)
}

/// A throwaway directory, created empty, for a body that may suspend.
func withTemporaryDirectory<T>(_ body: (URL) async throws -> T) async throws -> T {
    try await withTemporaryItem(suffix: "") { url in
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return try await body(url)
    }
}

/// Waits for a semaphore without parking a cooperative-pool thread; `false`
/// on timeout, so a broken test fails instead of hanging.
func signalled(_ semaphore: DispatchSemaphore, within seconds: Double = 10) async -> Bool {
    await withCheckedContinuation { continuation in
        DispatchQueue.global().async {
            continuation.resume(returning: semaphore.wait(timeout: .now() + seconds) == .success)
        }
    }
}

/// How many nodes carry `label`.
func countNodes(_ label: String, in connection: Connection) throws -> Int {
    try connection.run("MATCH (n:\(identifier: label)) RETURN count(n)").scalar(as: Int.self)
}

/// A database that holds a counter at zero.
func counterDatabase() throws -> Database {
    let db = try Database.inMemory()
    try db.connect().query("CREATE (:Counter {v: 0})")
    return db
}

/// A counter database and two connections on it: the fixture for a
/// write-write conflict on one node.
func counterRacers() throws -> (database: Database, winner: Connection, loser: Connection) {
    let db = try counterDatabase()
    return (db, try db.connect(), try db.connect())
}

/// Increments the counter of a counter database and returns its value.
let incrementCounter: Statement = "MATCH (c:Counter) SET c.v = c.v + 1 RETURN c.v"

/// Reads the counter of a counter database.
let counterValue: @Sendable (Transaction) throws -> Int = { tx in
    try tx.query("MATCH (c:Counter) RETURN c.v").scalar(as: Int.self)
}

extension Database {
    /// Waits until nothing is subscribed to this database's commits: how a
    /// test sees that an observation has let go. Cancellation reaches the
    /// subscription off the test's own task, so this waits rather than
    /// asserts; give the suite a time limit to bound a subscription that
    /// stays.
    func commitSubscribersAreGone() async {
        await waitUntil { core.hub.subscriberCount == 0 }
    }
}

/// Waits until `condition` holds, letting other tasks run in between: for
/// state that settles off the test's own task (an observation's read, a
/// cancelled subscription). It runs on the caller's actor, so a main-actor
/// test can read main-actor state in `condition`. It does not time out;
/// give the suite a time limit.
func waitUntil(isolation: isolated (any Actor)? = #isolation, _ condition: () throws -> Bool) async rethrows {
    while try !condition() { await Task.yield() }
}

/// Stages the engine's write-write conflict and returns what the losing
/// commit throws: two connections each create the same unique key inside a
/// transaction, the first commits, and the second commit loses (ADR-0439).
func losingCommitError() throws -> RelateError {
    let db = try Database.inMemory()
    let winner = try db.connect()
    try winner.query("CREATE CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE")
    let loser = try db.connect()
    try winner.begin()
    try loser.begin()
    try winner.query("CREATE (:P {id: 77})")
    try loser.query("CREATE (:P {id: 77})")
    _ = try winner.commit()
    return try thrownError { try loser.commit() }
}

/// What `read` finds in `error`. A case `read` does not read (it is named
/// `expected` in the failure) fails the test.
private func payload<Payload>(
    of error: RelateError, expected: String, _ read: (RelateError) -> Payload?
) throws -> Payload {
    guard let found = read(error) else {
        Issue.record("expected \(expected), got \(error)")
        throw error
    }
    return found
}

/// The `RelateError` that `body` must throw: `body` not throwing one fails
/// the test. Every helper in this file that needs the error a body throws
/// gets it here.
private func thrownError(_ body: () throws -> Void) throws -> RelateError {
    try #require(throws: RelateError.self) { try body() }
}

/// The message of the `invalidParameter` that `error` must be: any other
/// error fails the test. The form for an error a test already holds.
func invalidParameterMessage(of error: RelateError) throws -> String {
    try payload(of: error, expected: "invalidParameter") {
        if case .invalidParameter(let message) = $0 { message } else { nil }
    }
}

/// The message of the `invalidParameter` that `body` must throw.
func invalidParameterMessage(_ body: () throws -> Void) throws -> String {
    try invalidParameterMessage(of: try thrownError(body))
}

/// The mismatch that `error` must be: any other error fails the test.
func resultMismatch(of error: RelateError) throws -> ResultMismatch {
    try payload(of: error, expected: "unexpectedResult") {
        if case .unexpectedResult(let mismatch) = $0 { mismatch } else { nil }
    }
}

/// The mismatch `body` must throw.
func resultMismatch(_ body: () throws -> Void) throws -> ResultMismatch {
    try resultMismatch(of: try thrownError(body))
}

/// The message of the `misuse` that `error` must be: any other error fails
/// the test. The form for an error a test already holds (an awaited one).
func misuseMessage(of error: RelateError) throws -> String {
    try payload(of: error, expected: "misuse") {
        if case .misuse(let message) = $0 { message } else { nil }
    }
}

/// The message of the `misuse` that `body` must throw.
func misuseMessage(_ body: () throws -> Void) throws -> String {
    try misuseMessage(of: try thrownError(body))
}
