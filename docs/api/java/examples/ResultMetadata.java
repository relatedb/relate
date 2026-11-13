package ai.relatedb.examples;

import ai.relatedb.Category;
import ai.relatedb.Database;
import ai.relatedb.Namespace;
import ai.relatedb.QueryType;
import ai.relatedb.Severity;

public class ResultMetadata {
    public static void main(String[] args) {
        var db = Database.open(":memory:");
        var conn = db.connect();

        // EXPLAIN reports the wrapped statement's query type without
        // executing it (ADR-0162): WRITE here, and no node is created.
        var explained = conn.query("EXPLAIN CREATE (n)");
        if (explained.getQueryType() != QueryType.WRITE) {
            throw new IllegalStateException("unexpected");
        }
        if (!conn.query("MATCH (n) RETURN n").rows().isEmpty()) {
            throw new IllegalStateException("EXPLAIN must not execute");
        }

        // A USE-routed statement reports its resolved namespace (ADR-0117);
        // the session namespace is unchanged for unrouted statements.
        var routed = conn.query("USE system SHOW DATABASES YIELD name RETURN name");
        if (routed.getNamespace() != Namespace.SYSTEM) {
            throw new IllegalStateException("unexpected");
        }
        if (conn.query("RETURN 1 AS n").getNamespace() != Namespace.NEO4J) {
            throw new IllegalStateException("unexpected");
        }

        // Query notifications (ADR-0363): EXPLAIN derives the plan-time list.
        // The two patterns share no node variable, so the plan is a Cartesian
        // product and the list carries its code, GQLSTATUS, severity,
        // category and remedy. A plain run derives no notifications.
        conn.query("CREATE (:Person)");
        conn.query("CREATE (:City)");
        var explainedCartesian = conn.query("EXPLAIN MATCH (a:Person), (b:City) RETURN a, b");
        var cartesian = explainedCartesian.getNotifications().stream()
                .filter(n -> "Neo.ClientNotification.Statement.CartesianProduct".equals(n.getCode()))
                .findFirst()
                .orElseThrow(() -> new IllegalStateException(
                        "expected CartesianProduct, saw " + explainedCartesian.getNotifications()));
        if (!"03N90".equals(cartesian.getGqlStatus())
                || cartesian.getSeverity() != Severity.INFORMATION
                || cartesian.getCategory() != Category.PERFORMANCE
                || !"Cartesian product".equals(cartesian.getTitle())
                || cartesian.getRemedy().isEmpty()) {
            throw new IllegalStateException("unexpected");
        }
        if (!conn.query("RETURN 1 AS n").getNotifications().isEmpty()) {
            throw new IllegalStateException("unexpected");
        }
    }
}
