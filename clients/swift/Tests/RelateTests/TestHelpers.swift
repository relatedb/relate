import Foundation
import Relate
import Testing

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

/// Waits for a semaphore without parking a cooperative-pool thread; `false`
/// on timeout, so a broken test fails instead of hanging.
func signalled(_ semaphore: DispatchSemaphore, within seconds: Double = 10) async -> Bool {
    await withCheckedContinuation { continuation in
        DispatchQueue.global().async {
            continuation.resume(returning: semaphore.wait(timeout: .now() + seconds) == .success)
        }
    }
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
    return try #require(throws: RelateError.self) { try loser.commit() }
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

/// What `read` finds in the `RelateError` that `body` must throw. `body`
/// not throwing fails the test too.
private func thrownPayload<Payload>(
    expected: String, _ body: () throws -> Void, _ read: (RelateError) -> Payload?
) throws -> Payload {
    try payload(of: try #require(throws: RelateError.self) { try body() }, expected: expected, read)
}

/// The message of the `invalidParameter` that `body` must throw: any other
/// outcome fails the test.
func invalidParameterMessage(_ body: () throws -> Void) throws -> String {
    try thrownPayload(expected: "invalidParameter", body) {
        if case .invalidParameter(let message) = $0 { message } else { nil }
    }
}

/// The mismatch `body` must throw: any other outcome fails the test.
func resultMismatch(_ body: () throws -> Void) throws -> ResultMismatch {
    try thrownPayload(expected: "unexpectedResult", body) {
        if case .unexpectedResult(let mismatch) = $0 { mismatch } else { nil }
    }
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
    try misuseMessage(of: try #require(throws: RelateError.self) { try body() })
}
