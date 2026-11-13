package ai.relatedb.examples;

import ai.relatedb.Database;

public class Transaction {
    public static void main(String[] args) {
        try (Database db = Database.open(":memory:");
             Database.Connection conn = db.connect()) {
            conn.begin();
            conn.query("CREATE (n:Person {name: 'Ada'})");
            var result = conn.query("MATCH (n:Person) RETURN n.name AS name");
            if (!"Ada".equals(result.rows().get(0).get(0))) {
                throw new IllegalStateException("unexpected");
            }
            // commit() returns the transaction's bookmark — the engine's
            // commit sequence in the "relate:<n>" form — or null when
            // the transaction committed nothing (no bookmark is fabricated)
            String bookmark = conn.commit();
            if (bookmark == null) {
                throw new IllegalStateException("the transaction wrote: it has a bookmark");
            }
            // An auto-commit write's bookmark rides its result instead
            String auto = conn.query("CREATE (:Person {name: 'Bob'})").getBookmark();
            // The "relate:<n>" form is NOT lexicographically ordered
            // ("relate:10" sorts before "relate:9"), so compare the
            // numeric commit sequence n itself
            if (auto == null || sequence(auto) <= sequence(bookmark)) {
                throw new IllegalStateException("a later commit sorts after an earlier one");
            }
        }
    }

    /** The bookmark's numeric commit sequence: "relate:<n>" reads n. */
    private static long sequence(String bookmark) {
        return Long.parseLong(bookmark.substring("relate:".length()));
    }
}
