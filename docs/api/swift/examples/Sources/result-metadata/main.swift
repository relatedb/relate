import Relate

let conn = try Database.inMemory().connect()

let write = try conn.query("CREATE (:City {name: 'Oslo'}), (:City {name: 'Bergen'})")
print("type:", write.queryType, "nodes created:", write.stats.nodesCreated)
precondition(write.queryType == .write)
precondition(write.stats.nodesCreated == 2 && write.stats.propertiesSet == 2)

let read = try conn.query("MATCH (c:City) RETURN c.name AS name, c AS node ORDER BY name")
print("type:", read.queryType, "namespace:", read.namespace, "elapsed ms:", read.elapsedMs)
precondition(read.queryType == .read && read.namespace == .neo4j)
// The elapsed total splits into parse / plan / execute windows (ADR-0491);
// their sum never exceeds the total.
precondition(read.parseMs >= 0 && read.planMs >= 0 && read.executeMs >= 0)
precondition(read.parseMs + read.planMs + read.executeMs <= read.elapsedMs)
// Every cell carries its own kind (per-value, not per-column).
precondition(read.rows[0].map(\.kind) == [.string, .node])
precondition(read.notifications.isEmpty && read.plan == nil)
let node = read.rows[0][1].nodeValue!
precondition(node.labels == ["City"] && node.properties["name"] == "Bergen")
precondition(read.rows[0][1]["name"] == "Bergen")  // CypherValue subscripts reach node properties

// EXPLAIN plans without executing; PROFILE executes and attaches metrics.
let explain = try conn.query("EXPLAIN MATCH (a:City), (b:City) RETURN a, b")
let plan = explain.plan!
print("plan root:", plan.root.operator, "notifications:", explain.notifications.map(\.code))
precondition(plan.schema == "relate.plan/v1" && !plan.profiled && plan.root.actual == nil)
precondition(explain.notifications.contains { $0.code.hasSuffix("CartesianProduct") })
// Severity and category are closed, typed enums.
precondition(explain.notifications.allSatisfy { $0.severity == .information && $0.category == .performance })

let profile = try conn.query("PROFILE MATCH (c:City) RETURN count(c)")
precondition(profile.plan!.profiled && profile.plan!.root.actual != nil)
