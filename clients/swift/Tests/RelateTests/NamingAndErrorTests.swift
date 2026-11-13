import Foundation
import SwiftUI
import Testing

// Not @testable, and beside Foundation and SwiftUI on purpose: this file is
// the compile-time proof that the package's public names resolve unqualified
// next to Apple's (`Foundation.Notification`, `SwiftUI.Namespace`).
import Relate

@Suite struct NamingAndErrorTests {
    @Test func publicNamesDoNotCollideWithFoundationOrSwiftUI() throws {
        let conn = try Database.inMemory().connect(namespace: .neo4j)
        let result = try conn.query("EXPLAIN MATCH (a), (b) RETURN a, b")
        let notifications: [QueryNotification] = result.notifications
        let namespace: DatabaseNamespace = result.namespace
        #expect(namespace == .neo4j)
        for notification in notifications {
            let severity: NotificationSeverity = notification.severity
            let category: NotificationCategory = notification.category
            #expect(severity == .information || severity == .warning)
            #expect(category == .performance || category != .performance)
        }
        // Apple's own types of the old names still resolve unqualified.
        let posted = Notification(name: Notification.Name("relate.test"))
        #expect(posted.name.rawValue == "relate.test")
    }

    @Test func leafCallsThrowTheTypedError() throws {
        let conn = try Database.inMemory().connect()
        do {
            try conn.query("RETURN 1/0")
            Issue.record("the division did not throw")
        } catch {
            // No cast: the catch clause's error is statically a RelateError.
            let typed: RelateError = error
            #expect(typed.engineError?.stage == .runtime)
            #expect(!typed.isRetryable)
        }
    }

    @Test func aStoreOpensFromAFileURL() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("relate-swift-\(UUID().uuidString).rl")
        defer { try? FileManager.default.removeItem(at: url) }
        do {
            let db = try Database(url: url)
            #expect(!db.isReadOnly && db.path == url.path)
            try db.connect().query("CREATE (:P {v: 1})")
        }
        let reader = try Database(url: url, readOnly: true)
        #expect(reader.isReadOnly)
        #expect(try reader.connect().query("MATCH (p:P) RETURN p.v").scalar() == 1)
        #expect(throws: RelateError.self) { try reader.connect().query("CREATE (:P)") }
    }

    @Test func aNonFileURLIsRefusedBeforeTheEngine() throws {
        let error = try #require(throws: RelateError.self) {
            try Database(url: try #require(URL(string: "https://example.com/store.rl")))
        }
        guard case .invalidParameter(let message) = error else {
            Issue.record("expected invalidParameter, got \(error)")
            return
        }
        #expect(message.contains("'https'"))
    }

    @Test func aConnectionOpensOnATypedNamespace() throws {
        let db = try Database.inMemory()
        let system = try db.connect(namespace: .system)
        #expect(try system.query("SHOW DATABASES").namespace == .system)
        let conn = try db.connect()
        #expect(try conn.query("RETURN 1").namespace == .neo4j)
        try conn.setNamespace(.system)
        #expect(try conn.query("SHOW DEFAULT DATABASE").namespace == .system)
        try conn.setNamespace(.neo4j)
        #expect(try conn.query("RETURN 1").namespace == .neo4j)
    }

    // Each predicate is provoked from the live engine, so a code the engine
    // renames fails here rather than silently never matching.

    @Test func aConstraintViolationIsRecognized() throws {
        let conn = try Database.inMemory().connect()
        try conn.query("CREATE CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE")
        try conn.query("CREATE (:P {id: 1})")
        let error = try #require(throws: RelateError.self) { try conn.query("CREATE (:P {id: 1})") }
        let engine = try #require(error.engineError)
        #expect(engine.isConstraintViolation)
        #expect(engine.classification == .clientError)
        #expect(!engine.isSyntaxError && !engine.isMissingParameter && !engine.isWriteConflict)
    }

    @Test func aSyntaxErrorIsRecognized() throws {
        let conn = try Database.inMemory().connect()
        let error = try #require(throws: RelateError.self) { try conn.query("RETURN nope(") }
        let engine = try #require(error.engineError)
        #expect(engine.isSyntaxError && engine.classification == .clientError)
    }

    @Test func aMissingParameterIsRecognized() throws {
        let conn = try Database.inMemory().connect()
        let error = try #require(throws: RelateError.self) { try conn.query("RETURN $absent") }
        let engine = try #require(error.engineError)
        #expect(engine.isMissingParameter && engine.classification == .clientError)
    }

    @Test func aWriteConflictIsRecognizedAndRetryable() throws {
        let db = try Database.inMemory()
        let winner = try db.connect()
        try winner.query("CREATE CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE")
        let loser = try db.connect()
        try winner.begin()
        try loser.begin()
        try winner.query("CREATE (:P {id: 77})")
        try loser.query("CREATE (:P {id: 77})")
        try winner.commit()
        let error = try #require(throws: RelateError.self) { try loser.commit() }
        try? loser.rollback()
        let engine = try #require(error.engineError)
        #expect(engine.isWriteConflict && engine.retryable && error.isRetryable)
        #expect(engine.classification == .transientError)
    }

    @Test func anUnshapedCodeHasNoClassification() throws {
        // The client-side cases carry no engine code at all.
        #expect(RelateError.invalidParameter("x").engineError == nil)
        #expect(!RelateError.decoding("x").isRetryable)
    }
}
