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
        for raw in ["neo4j", "system", "system_ontologies"] { _ = try Namespace.decode(raw) }
        #expect(try Namespace.decode("system_ontologies") == .systemOntologies)
        for bad in ["", "R", "x", "read"] {
            #expect(throws: RelateError.self) { try QueryType.decode(bad) }
        }
        for bad in ["", "Neo4j", "other"] {
            #expect(throws: RelateError.self) { try Namespace.decode(bad) }
        }
        do {
            _ = try QueryType.decode("x")
        } catch {
            #expect(error.message.contains("ABI contract violated: query type 'x'"))
        }
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
