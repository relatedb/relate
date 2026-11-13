package ai.relatedb.examples;

import ai.relatedb.Database;

public class CreateMatch {
    public static void main(String[] args) {
        var db = Database.open(":memory:");
        var conn = db.connect();
        conn.query("CREATE (n:Person {name: 'Ada'})");
        var result = conn.query("MATCH (n:Person) RETURN n.name AS name");
        if (!"Ada".equals(result.rows().get(0).get(0))) {
            throw new IllegalStateException("unexpected");
        }
    }
}
