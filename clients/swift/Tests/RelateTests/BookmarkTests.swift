import Foundation
import Testing

@testable import Relate

@Suite struct BookmarkTests {
    /// The bookmark's commit sequence, parsed from `"relate:<n>"`
    /// (ADR-0510). Monotonicity compares the parsed `n`, never the strings:
    /// lexicographic order breaks once the sequence gains a digit
    /// (`"relate:9"` sorts after `"relate:10"`).
    private func commitSequence(_ bookmark: String) throws -> Int {
        let prefix = "relate:"
        guard bookmark.hasPrefix(prefix),
            let sequence = Int(bookmark.dropFirst(prefix.count))
        else {
            Issue.record("bookmark '\(bookmark)' is not the documented 'relate:<n>' shape")
            throw RelateError.decoding("unparsable bookmark '\(bookmark)'")
        }
        return sequence
    }

    @Test func autoCommitBookmarksRideTheResult() throws {
        let conn = try Database.inMemory().connect()
        let read = try conn.query("RETURN 1")
        #expect(read.bookmark == nil)
        let write = try conn.query("CREATE (:N)")
        let bookmark = try #require(write.bookmark)
        #expect(bookmark.hasPrefix("relate:"))
        let second = try #require(try conn.query("CREATE (:N)").bookmark)
        #expect(try commitSequence(second) > commitSequence(bookmark))
        let explain = try conn.query("EXPLAIN CREATE (:N)")
        #expect(explain.bookmark == nil)
    }

    @Test func explicitCommitsReturnTheBookmark() throws {
        let conn = try Database.inMemory().connect()
        try conn.begin()
        try conn.query("CREATE (:Kept)")
        let bookmark = try #require(try conn.commit())
        #expect(bookmark.hasPrefix("relate:"))
        // A transaction that committed nothing returns nil, not a
        // fabricated bookmark.
        try conn.begin()
        try conn.query("RETURN 1")
        #expect(try conn.commit() == nil)
    }

    @Test func aFailedCommitThrowsTheTypedConflict() throws {
        // ADR-0510: a commit failure rides the ordinary status + error-json
        // channel like every other call — no NULL special case. The
        // retryable write-write conflict (ADR-0439) surfaces on the losing
        // commit with the classification a retry loop needs.
        let error = try losingCommitError()
        let engine = try #require(error.engineError)
        #expect(engine.code == "Neo.TransientError.Transaction.Outdated")
        #expect(engine.retryable)
    }
}
