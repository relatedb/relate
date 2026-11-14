///usr/bin/env jbang "$0" "$@" ; exit $?
//DEPS ai.relatedb:relate:1.0.2
//SOURCES HydratedValues.java
package ai.relatedb.examples;

import ai.relatedb.Database;
import ai.relatedb.Database.Result;
import ai.relatedb.Node;
import ai.relatedb.Relationship;

import java.time.LocalDate;
import java.util.List;
import java.util.Map;

/**
 * The end-to-end example (ADR-0490): everything a user's first contact
 * needs — resolve the jar, load the bundled native, run Cypher — through
 * JBang, with no {@code RELATE_LIB}, no {@code java.library.path} and
 * no system library search. The only native source is the
 * {@code /native/<platform>/} entry inside the jar.
 *
 * <p>Until the artifact is published, install it into the local Maven
 * repository first: {@code mvn -f clients/java/pom.xml install} (a release
 * build of the FFI library must exist, see docs/api/java/spec.md). Then:
 *
 * <pre>jbang docs/api/java/examples/RelateE2E.java</pre>
 */
public class RelateE2E {

    /** The success marker the PackagedJarIT asserts when it runs this example. */
    public static final String OK = "end-to-end OK";

    public static void main(String[] args) {
        if (System.getenv("RELATE_LIB") != null) {
            throw new IllegalStateException("RELATE_LIB must be unset:"
                    + " this example proves the bundled-native loader");
        }
        try (Database db = Database.open(":memory:");
             Database.Connection conn = db.connect()) {

            Result one = conn.query("RETURN 1 AS one");
            check(one.rows().get(0).get(0), 1, "RETURN 1");

            conn.query("CREATE (a:Person {name: 'Alice', born: date('1980-01-01')})"
                    + "-[:KNOWS {since: 2000}]->(b:Person {name: 'Bob'})");

            // Graph entities hydrate typed through the value walker
            // (ADR-0502 §1): Node / Relationship records with element ids,
            // labels/type and a nested properties map — no kind-tag
            // sniffing.
            Result graph = conn.query(
                    "MATCH (a:Person {name:'Alice'})-[r:KNOWS]->(b:Person)"
                            + " RETURN a, r, b.name, a.born ORDER BY b.name");
            List<Object> row = graph.rows().get(0);
            Node alice = (Node) row.get(0);
            Relationship knows = (Relationship) row.get(1);
            check(alice.labels().get(0), "Person", "node label");
            check(alice.properties().get("name"), "Alice", "node property");
            check(knows.type(), "KNOWS", "relationship type");
            check(knows.properties().get("since"), 2000L, "relationship property");
            check(knows.startElementId(), alice.elementId(), "the relationship starts at Alice");
            // Temporals hydrate as java.time values (the typed ABI keeps
            // the type — no ISO string to parse)
            check(row.get(3), LocalDate.of(1980, 1, 1), "date property (java.time)");

            // A parameterised query (the typed params builder): bind the
            // name, never concatenate it
            Result named = conn.query("MATCH (p:Person {name: $name}) RETURN p.name AS name",
                    Map.of("name", "Bob"));
            check(named.rows().get(0).get(0), "Bob", "parameterised query");

            conn.query("CREATE (n:Counter {n: 0})");
            conn.begin();
            conn.query("MATCH (n:Counter) SET n.n = n.n + 1");
            conn.query("MATCH (n:Counter) SET n.n = n.n + 1");
            String bookmark = conn.commit(); // the explicit commit's bookmark
            if (bookmark == null) {
                throw new IllegalStateException("the transaction wrote: it has a bookmark");
            }
            Result tx = conn.query("MATCH (n:Counter) RETURN n.n AS n");
            check(tx.rows().get(0).get(0), 2, "explicit transaction");

            System.out.println("relate " + one.getQueryType() + ": " + OK
                    + " (java " + System.getProperty("java.version") + ")");
        }
    }

    static void check(Object actual, Object expected, String what) {
        if (!HydratedValues.equal(actual, expected)) {
            throw new IllegalStateException(what + ": expected " + expected + ", got " + actual);
        }
        System.out.println("ok: " + what);
    }
}
