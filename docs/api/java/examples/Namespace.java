package ai.relatedb.examples;

import ai.relatedb.RelateException;
import ai.relatedb.Database;

public class Namespace {
    public static void main(String[] args) {
        var db = Database.open(":memory:");

        // connect() is equivalent to connect("neo4j").
        var conn = db.connect("system");

        // Names match case-insensitively.
        conn.setNamespace("NEO4J");
        var result = conn.query("RETURN 1 AS n");
        if (((Number) result.rows().get(0).get(0)).intValue() != 1) {
            throw new IllegalStateException("unexpected");
        }

        // Unknown names fail with "Graph not found: <name>" (ADR-0146).
        try {
            db.connect("nope");
            throw new IllegalStateException("expected failure");
        } catch (RelateException expected) {
            // ok
        }

        // Refused while an explicit transaction is open.
        conn.begin();
        try {
            conn.setNamespace("system");
            throw new IllegalStateException("expected failure");
        } catch (RelateException expected) {
            // "Cannot change the namespace while a transaction is open."
        } finally {
            conn.rollback();
        }
        conn.setNamespace("system");
    }
}
