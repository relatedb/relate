import Foundation
import Relate
import Testing

/// One policy for a throwaway on-disk store: a UUID-named item under the
/// system temp, removed when the body returns. `suffix` picks the store
/// form — none for a store directory, `.rl` for a single-file store.
private func withTemporaryItem(suffix: String, _ body: (URL) throws -> Void) throws {
    let url = FileManager.default.temporaryDirectory
        .appendingPathComponent("relate-swift-\(UUID().uuidString)\(suffix)")
    defer { try? FileManager.default.removeItem(at: url) }
    try body(url)
}

/// A throwaway store directory, by path.
func withTemporaryStore(_ body: (String) throws -> Void) throws {
    try withTemporaryItem(suffix: "") { try body($0.path) }
}

/// A throwaway single-file `.rl` store, by file URL.
func withTemporaryStoreFile(_ body: (URL) throws -> Void) throws {
    try withTemporaryItem(suffix: ".rl", body)
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

/// The message of the `invalidParameter` that `body` must throw: any other
/// outcome fails the test.
func invalidParameterMessage(_ body: () throws -> Void) throws -> String {
    let error = try #require(throws: RelateError.self) { try body() }
    guard case .invalidParameter(let message) = error else {
        Issue.record("expected invalidParameter, got \(error)")
        throw error
    }
    return message
}
