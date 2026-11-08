import Relate

let db = try Database.inMemory()

// connect() is connect(namespace: "neo4j"); the other fixed namespaces are
// "system" and "system_ontologies" (ADR-0117).
let conn = try db.connect()
try conn.query("CREATE (:Item)")

// USE routes one statement; the session namespace is unchanged.
let routed = try conn.query("USE system SHOW DATABASES YIELD name RETURN name ORDER BY name")
print("databases:", routed.rows.map { $0[0] })
precondition(routed.namespace == .system)
let unrouted = try conn.query("MATCH (n) RETURN count(n)")
precondition(unrouted.namespace == .neo4j)

// setNamespace(_:) switches the session between transactions.
try conn.setNamespace("system")
let shown = try conn.query("SHOW DATABASES")
precondition(shown.namespace == .system)

let system = try db.connect(namespace: "system")
let defaultDatabase = try system.query("SHOW DEFAULT DATABASE")
precondition(defaultDatabase.namespace == .system)

do {
    _ = try db.connect(namespace: "nope")
    preconditionFailure("an unknown namespace must be refused")
} catch let error as RelateError {
    print("refused:", error.message)
}
