import Foundation
import Testing

@testable import Relate

@Suite struct MetadataTests {
    let conn = try! Database.inMemory().connect()

    @Test func queryTypeAndStats() throws {
        let w = try conn.query("CREATE (:A {x: 1}), (:A)")
        #expect(w.queryType == .write)
        #expect(w.stats.nodesCreated == 2 && w.stats.propertiesSet == 1 && w.stats.labelsAdded == 2)
        let r = try conn.query("MATCH (n:A) RETURN count(n) AS c")
        #expect(r.queryType == .read)
        #expect(try r.scalar() == 2)
        #expect(r.notifications.isEmpty && r.plan == nil && r.elapsedMs >= 0)
        #expect(try conn.query("CREATE (n:B) RETURN n").queryType == .readWrite)
        #expect(try conn.query("CREATE INDEX FOR (n:A) ON (n.x)").queryType == .schema)
    }

    @Test func metadataVocabulariesRefuseUnknownValues() throws {
        // The closed vocabularies (ADR-0162, ADR-0117): every listed value
        // decodes, anything else is an ABI contract violation.
        for raw in ["r", "w", "rw", "s"] { _ = try QueryType.decode(raw) }
        for raw in ["neo4j", "system", "system_ontologies"] { _ = try DatabaseNamespace.decode(raw) }
        #expect(try DatabaseNamespace.decode("system_ontologies") == .systemOntologies)
        for bad in ["", "R", "x", "read"] {
            #expect(throws: RelateError.self) { try QueryType.decode(bad) }
        }
        for bad in ["", "Neo4j", "other"] {
            #expect(throws: RelateError.self) { try DatabaseNamespace.decode(bad) }
        }
        do {
            _ = try QueryType.decode("x")
        } catch {
            #expect(error.message.contains("ABI contract violated: query type 'x'"))
        }
    }

    private func notifications(severity: String, category: String) -> String {
        """
        [{"code":"c","gql_status":"03N90","severity":\(severity),"category":\(category),\
        "title":"t","description":"d","remedy":"r"}]
        """
    }

    @Test func notificationVocabulariesAreClosed() throws {
        // ADR-0363: severity and category are closed, like query type and
        // namespace — every listed value decodes typed, anything else (and a
        // missing or null key) is refused naming the key and the value.
        let sevs: [String: NotificationSeverity] = ["INFORMATION": .information, "WARNING": .warning]
        let cats: [String: NotificationCategory] = [
            "PERFORMANCE": .performance, "UNRECOGNIZED": .unrecognized,
            "DEPRECATION": .deprecation, "SCHEMA": .schema, "HINT": .hint,
        ]
        for (sev, wantSev) in sevs {
            for (cat, wantCat) in cats {
                let json = notifications(severity: "\"\(sev)\"", category: "\"\(cat)\"")
                let n = try decodeMetadata([QueryNotification].self, json, what: "notifications")
                #expect(n.count == 1 && n[0].severity == wantSev && n[0].category == wantCat)
            }
        }
        for bad in ["", "warning", "ERROR"] {
            do {
                _ = try decodeMetadata(
                    [QueryNotification].self, notifications(severity: "\"\(bad)\"", category: "\"HINT\""),
                    what: "notifications")
                Issue.record("severity '\(bad)' decoded")
            } catch {
                #expect(error.message.contains("unknown severity '\(bad)'"))
            }
            do {
                _ = try decodeMetadata(
                    [QueryNotification].self, notifications(severity: "\"WARNING\"", category: "\"\(bad)\""),
                    what: "notifications")
                Issue.record("category '\(bad)' decoded")
            } catch {
                #expect(error.message.contains("unknown category '\(bad)'"))
            }
        }
        for json in [
            notifications(severity: "null", category: "\"HINT\""),
            notifications(severity: "7", category: "\"HINT\""),
            #"[{"code":"c","gql_status":"g","category":"HINT","title":"t","description":"d","remedy":"r"}]"#,
        ] {
            #expect(throws: RelateError.self) {
                try decodeMetadata([QueryNotification].self, json, what: "notifications")
            }
        }
    }

    @Test func refusalsSanitizeEchoedWireValues() throws {
        // CWE-117: an untrusted wire value echoed into an error message has
        // its control and non-printable characters escaped and is bounded
        // (64 for a value, 256 for a payload), like the Java and Go clients.
        let hostile = "INFO\r\nFAKE LOG '\\x\u{2028}\u{202E}\u{0}"
        func assertSanitized(_ message: String) {
            for scalar in message.unicodeScalars {
                #expect(scalar.value >= 0x20 && scalar.value != 0x7f
                    && scalar.value != 0x2028 && scalar.value != 0x202E)
            }
        }
        do {
            _ = try QueryType.decode(hostile)
            Issue.record("hostile query type decoded")
        } catch {
            assertSanitized(error.message)
            #expect(error.message.contains("\\u000d\\u000a") && error.message.contains("\\u2028"))
            #expect(error.message.contains("\\u0027") && error.message.contains("\\u005c"))
        }
        do {
            _ = try DatabaseNamespace.decode(hostile)
            Issue.record("hostile namespace decoded")
        } catch {
            assertSanitized(error.message)
        }
        let jsonHostile = "INFO\\r\\nFAKE\\u2028\\u0000"
        do {
            _ = try decodeMetadata(
                [QueryNotification].self, notifications(severity: "\"\(jsonHostile)\"", category: "\"HINT\""),
                what: "notifications")
            Issue.record("hostile severity decoded")
        } catch {
            assertSanitized(error.message)
            #expect(error.message.contains("unknown severity"))
        }
        do {
            _ = try DatabaseNamespace.decode(String(repeating: "a", count: 500))
            Issue.record("long namespace decoded")
        } catch {
            #expect(error.message.contains("...(truncated)"))
            #expect(!error.message.contains(String(repeating: "a", count: 65)))
        }
        // An unknown error stage and a non-shape error payload echo sanitized too.
        let stage = "{\"code\":\"c\",\"gql_status\":\"g\",\"stage\":\"x\\u000ay\",\"retryable\":false,\"message\":\"m\"}"
        let err = stage.withCString { engineError(from: $0, what: "error") }
        assertSanitized(err.message)
        #expect(err.message.contains("unknown error stage"))
        let big = "{" + String(repeating: "z", count: 600)
        let bigErr = big.withCString { engineError(from: $0, what: "error") }
        #expect(!bigErr.message.contains(String(repeating: "z", count: 300)))
        #expect(bigErr.message.contains("...(truncated)"))
    }

    @Test func phaseTimingSplit() throws {
        // ADR-0491: the elapsed total splits into three always-on, disjoint
        // sub-windows whose sum never exceeds the total (the inter-stage
        // gaps stay in the total alone).
        let r = try conn.query("MATCH (n:A) RETURN count(n) AS c")
        #expect(r.parseMs >= 0 && r.planMs >= 0 && r.executeMs >= 0)
        #expect(r.parseMs + r.planMs + r.executeMs <= r.elapsedMs)
        // An EXPLAIN statement's execute window is the plan render, not a
        // dispatch — the split rides it the same way.
        let e = try conn.query("EXPLAIN MATCH (n:A) RETURN n")
        #expect(e.parseMs + e.planMs + e.executeMs <= e.elapsedMs)
    }

    @Test func namespaceRouting() throws {
        #expect(try conn.query("RETURN 1").namespace == .neo4j)
        #expect(try conn.query("USE system SHOW DATABASES YIELD name RETURN name").namespace == .system)
        #expect(try conn.query("RETURN 1").namespace == .neo4j)
        try conn.setNamespace("system")
        #expect(try conn.query("SHOW DATABASES").namespace == .system)
        let sys = try conn.database.connect(namespace: "system")
        #expect(try sys.query("SHOW DATABASES").namespace == .system)
        #expect(throws: RelateError.self) { try conn.database.connect(namespace: "nope") }
    }

    @Test func explainAndProfileCarryAPlan() throws {
        let e = try conn.query("EXPLAIN CREATE (n)")
        #expect(e.queryType == .write)
        let plan = try #require(e.plan)
        #expect(plan.schema == "relate.plan/v1" && !plan.profiled && plan.root.actual == nil)
        #expect(try conn.query("MATCH (n) RETURN count(n) AS c").scalar() == 0)
        let p = try #require(try conn.query("PROFILE MATCH (n) RETURN n").plan)
        #expect(p.profiled && p.root.actual != nil)
    }
}
