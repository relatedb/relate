import Foundation
import SwiftUI
import Testing

// Not @testable, and beside Foundation and SwiftUI on purpose: this file
// holds the package's two compile-time proofs. Its public names resolve
// unqualified next to Apple's (`Foundation.Notification`,
// `SwiftUI.Namespace`), and a `catch` after a typed-throwing call has a
// `RelateError` with no cast.
import Relate

@Suite struct NamingAndErrorTests {
    @Test func publicNamesDoNotCollideWithFoundationOrSwiftUI() throws {
        let conn = try Database.inMemory().connect(namespace: .neo4j)
        let result = try conn.query("EXPLAIN MATCH (a), (b) RETURN a, b")
        let namespace: DatabaseNamespace = result.namespace
        #expect(namespace == .neo4j)
        // The Cartesian product draws one performance notification.
        let notifications: [QueryNotification] = result.notifications
        #expect(!notifications.isEmpty)
        for notification in notifications {
            let severity: NotificationSeverity = notification.severity
            let category: NotificationCategory = notification.category
            #expect(severity == .information && category == .performance)
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
}
