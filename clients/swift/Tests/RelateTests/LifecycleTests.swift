import Foundation
import Testing

@testable import Relate

@Suite struct LifecycleTests {
    @Test func transactionCommitsAndRollsBack() throws {
        let conn = try Database.inMemory().connect()
        try conn.transaction { try $0.query("CREATE (:Kept)") }
        struct Boom: Error {}
        #expect(throws: Boom.self) {
            try conn.transaction { tx in
                try tx.query("CREATE (:Dropped)")
                throw Boom()
            }
        }
        #expect(try conn.query("MATCH (n:Kept) RETURN count(n)").scalar() == 1)
        #expect(try conn.query("MATCH (n:Dropped) RETURN count(n)").scalar() == 0)
    }

    @Test func persistsAcrossReopenAndReadOnly() throws {
        try withTemporaryStore { path in
            do {
                let db = try Database(path: path)
                try db.connect().query("CREATE (:P {v: $v})", ["v": 7])
                try db.checkpoint()
                try db.connect().query("CREATE (:P {v: 8})")  // WAL-only tail
            }
            do {
                let db = try Database(readOnlyPath: path)
                #expect(db.isReadOnly)
                let conn = try db.connect()
                #expect(try conn.query("MATCH (p:P) RETURN sum(p.v)").scalar() == 15)
                #expect(throws: RelateError.self) { try conn.query("CREATE (:P)") }
            }
        }
    }

    @Test func aStoreOpensFromAFileURL() throws {
        try withTemporaryStoreFile { url in
            do {
                let db = try Database(url: url)
                #expect(!db.isReadOnly && db.path == url.path)
                try db.connect().query("CREATE (:P {v: 1})")
            }
            let reader = try Database(url: url, readOnly: true)
            #expect(reader.isReadOnly)
            #expect(try reader.connect().query("MATCH (p:P) RETURN p.v").scalar() == 1)
            #expect(throws: RelateError.self) { try reader.connect().query("CREATE (:P)") }
            // `file://localhost/…` names the same local file.
            var local = try #require(URLComponents(url: url, resolvingAgainstBaseURL: false))
            local.host = "localhost"
            #expect(try Database(url: try #require(local.url), readOnly: true).path == url.path)
        }
    }

    @Test func aURLThatIsNotALocalFileIsRefusedBeforeTheEngine() throws {
        func refusal(_ text: String) throws -> String {
            let url = try #require(URL(string: text))
            let error = try #require(throws: RelateError.self) { try Database(url: url) }
            guard case .invalidParameter(let message) = error else {
                Issue.record("expected invalidParameter for \(text), got \(error)")
                return ""
            }
            return message
        }
        #expect(try refusal("https://example.com/store.rl").contains("scheme 'https'"))
        // A file URL naming another host: its path alone would open (or,
        // writable, create) an unrelated local file.
        #expect(try refusal("file://fileserver.example/shared/store.rl").contains("host 'fileserver.example'"))
        #expect(!FileManager.default.fileExists(atPath: "/shared/store.rl"))
    }

    @Test func aSecondWritableOpenOfOneStoreIsRefused() throws {
        try withTemporaryStore { path in
            let first = try Database(path: path)
            #expect(throws: RelateError.self) { try Database(path: path) }
            _ = first
        }
    }

    @Test func connectionsOnSeparateTasksShareOneDatabase() async throws {
        let db = try Database.inMemory()
        try db.connect().query("CREATE (:Seed)")
        let counts = try await withThrowingTaskGroup(of: Int64.self) { group in
            for _ in 0..<8 {
                group.addTask {
                    let conn = try db.connect()
                    return try #require(conn.query("MATCH (n) RETURN count(n)").scalar().intValue)
                }
            }
            return try await group.reduce(into: []) { $0.append($1) }
        }
        #expect(counts == Array(repeating: 1, count: 8))
    }

    @Test func aThrowingCommitInsideTransactionLeavesNoOpenTransaction() throws {
        // LABS-957 (F-1-8): when commit() throws, transaction(_:) must roll
        // back so the connection is not left inside the failed transaction.
        let db = try Database.inMemory()
        let winner = try db.connect()
        try winner.query("CREATE CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE")
        let loser = try db.connect()
        try winner.begin()
        let error = try #require(throws: RelateError.self) {
            try loser.transaction { tx in
                try winner.query("CREATE (:P {id: 77})")
                try tx.query("CREATE (:P {id: 77})")
                _ = try winner.commit()
            }
        }
        #expect(try #require(error.engineError).retryable)
        // A fresh transaction begins: nothing is left open by the failure.
        try loser.begin()
        try loser.rollback()
    }
}
