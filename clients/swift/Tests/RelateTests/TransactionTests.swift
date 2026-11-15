import Foundation
import Testing

@testable import Relate

@Suite struct TransactionTests {
    @Test func aTransactionScopeRunsStatementsAndCommits() throws {
        let conn = try Database.inMemory().connect()
        let name = "Ada"
        let created = try conn.transaction { tx in
            try tx.query("CREATE (:P {name: $name})", ["name": name])
            try tx.run("CREATE (:P {name: \("Grace")})")
            #expect(!tx.isReadOnly)
            return try tx.run("MATCH (p:P) RETURN count(p)").scalar(as: Int.self)
        }
        #expect(created == 2)
        #expect(try countNodes("P", in: conn) == 2)
    }

    @Test func aScopeThatEscapesItsBodyIsRefused() throws {
        let conn = try Database.inMemory().connect()
        var escaped: Transaction?
        try conn.transaction { tx in escaped = tx }
        let scope = try #require(escaped)
        let message = try misuseMessage { try scope.query("CREATE (:Late)") }
        #expect(message.contains("has ended"))
        #expect(try countNodes("Late", in: conn) == 0)
    }

    @Test func aReadSeesOneSnapshotAndKeepsNothing() throws {
        let db = try Database.inMemory()
        let reader = try db.connect()
        let writer = try db.connect()
        try writer.query("CREATE (:N), (:N)")
        let (before, after) = try reader.read { tx in
            #expect(tx.isReadOnly)
            let before = try tx.query("MATCH (n:N) RETURN count(n)").scalar(as: Int.self)
            // Another connection commits between this read's two statements.
            try writer.query("CREATE (:N)")
            let after = try tx.query("MATCH (n:N) RETURN count(n)").scalar(as: Int.self)
            return (before, after)
        }
        #expect(before == 2 && after == 2)
        #expect(try countNodes("N", in: reader) == 3)
        // Introspection is a read.
        _ = try reader.read { tx in try tx.query("SHOW INDEXES") }
    }

    @Test func aWriteInAReadIsRefusedAndRolledBack() throws {
        let conn = try Database.inMemory().connect()
        var message = try misuseMessage { _ = try conn.read { tx in try tx.query("CREATE (:Sneaky)") } }
        #expect(message.contains("write statement ran in a read"))
        #expect(try countNodes("Sneaky", in: conn) == 0)
        // A schema statement too: the index is not there afterwards.
        let indexes = try conn.query("SHOW INDEXES").count
        message = try misuseMessage {
            _ = try conn.read { tx in try tx.query("CREATE INDEX sneaky FOR (n:Sneaky) ON (n.x)") }
        }
        #expect(message.contains("schema statement ran in a read"))
        #expect(try conn.query("SHOW INDEXES").count == indexes)
        // The connection is back in auto-commit: a new transaction begins.
        try conn.transaction { tx in try tx.query("CREATE (:Kept)") }
        #expect(try countNodes("Kept", in: conn) == 1)
    }

    @Test func aRetryableConflictRunsTheBodyAgain() throws {
        let (_, winner, loser) = try counterRacers()
        var attempts = 0
        let seen = try loser.transaction(retries: 1) { tx in
            attempts += 1
            if attempts == 1 {
                // The other connection updates the same node and commits
                // first: this attempt's commit loses.
                try winner.run(incrementCounter)
            }
            return try tx.run(incrementCounter).scalar(as: Int.self)
        }
        #expect(attempts == 2 && seen == 2)
        #expect(try winner.query("MATCH (c:Counter) RETURN c.v").scalar() == 2)
    }

    /// A count below one is no retry: the conflict surfaces after one
    /// attempt, marked retryable, and leaves no transaction open.
    @Test(arguments: [0, -1]) func withoutRetriesAConflictSurfaces(retries: Int) throws {
        let (_, winner, loser) = try counterRacers()
        var attempts = 0
        let error = try #require(throws: RelateError.self) {
            try loser.transaction(retries: retries) { tx in
                attempts += 1
                try winner.run(incrementCounter)
                try tx.run(incrementCounter)
            }
        }
        #expect(attempts == 1 && error.isRetryable && error.engineError?.isWriteConflict == true)
        try loser.transaction { tx in try tx.run(incrementCounter) }
        #expect(try winner.query("MATCH (c:Counter) RETURN c.v").scalar() == 2)
    }

    @Test func aNonRetryableFailureIsNotRetried() throws {
        let conn = try Database.inMemory().connect()
        var attempts = 0
        #expect(throws: RelateError.self) {
            try conn.transaction(retries: 3) { tx in
                attempts += 1
                try tx.query("RETURN 1/0")
            }
        }
        #expect(attempts == 1)
        struct Boom: Error {}
        #expect(throws: Boom.self) {
            try conn.transaction(retries: 3) { _ in
                attempts += 1
                throw Boom()
            }
        }
        #expect(attempts == 2)
    }

    @Test func statementsRunAllOrNone() throws {
        let conn = try Database.inMemory().connect()
        let results = try conn.runInTransaction([
            "CREATE (:Batch {i: 1})",
            try "CREATE (:Batch {i: \(2)})",
            "MATCH (b:Batch) RETURN count(b)",
        ])
        #expect(results.count == 3 && results[2].rows == [[2]])
        #expect(throws: RelateError.self) {
            try conn.runInTransaction(["CREATE (:Batch {i: 3})", "RETURN 1/0"])
        }
        #expect(try countNodes("Batch", in: conn) == 2)
        // The typed entry point: a catch needs no cast.
        do {
            try conn.runInTransaction(["RETURN nope("])
            Issue.record("the syntax error did not throw")
        } catch {
            #expect(error.engineError?.isSyntaxError == true)
        }
    }
}
