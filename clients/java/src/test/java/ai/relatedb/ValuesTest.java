package ai.relatedb;

import java.time.LocalDate;
import java.time.LocalDateTime;
import java.time.LocalTime;
import java.time.OffsetDateTime;
import java.time.OffsetTime;
import java.time.ZoneId;
import java.time.ZoneOffset;
import java.time.ZonedDateTime;
import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.*;

/**
 * The typed value walker (ADR-0502 §1): every Cypher kind hydrates into
 * its Java value through {@code relate_value_at} + the accessors —
 * no JSON decode, no re-typing.
 */
public class ValuesTest {

    private static Object cell(String cypher) {
        Database db = Database.open(":memory:");
        Database.Result r = db.connect().query(cypher);
        assertEquals(1, r.rows().size(), cypher);
        return r.rows().get(0).get(0);
    }

    @Test
    void scalars() {
        Database db = Database.open(":memory:");
        Database.Result r = db.connect().query(
                "RETURN true AS b, 1 AS i, 9007199254740993 AS big, 1.5 AS f, 'Ada' AS s, null AS n");
        List<Object> row = r.rows().get(0);
        assertEquals(Boolean.TRUE, row.get(0));
        // INT is a Long: a Java long holds every int64 exactly (LABS-901's guard)
        assertEquals(1L, row.get(1));
        assertEquals(9007199254740993L, row.get(2));
        assertEquals(1.5, row.get(3));
        assertEquals("Ada", row.get(4));
        assertNull(row.get(5));
    }

    @Test
    void intBeyondDoublePrecisionStaysExact() {
        assertEquals(9007199254740993L, cell("RETURN 9007199254740993"));
        assertEquals(Long.class, cell("RETURN 9007199254740993").getClass());
    }

    @Test
    void floatSpecialsCrossAsThemselves() {
        assertTrue(((Double) cell("RETURN 0.0 / 0.0")).isNaN(), "NaN crosses as NaN");
        assertTrue(((Double) cell("RETURN 1.0 / 0.0")).isInfinite(), "+Inf crosses as +Inf");
    }

    @Test
    void stringWithInteriorNulCrossesWhole() {
        String s = (String) cell("RETURN 'a\\u0000b' AS s");
        assertEquals("a\u0000b", s, "the length form must carry the interior NUL");
        assertEquals(3, s.length());
    }

    @Test
    void listAndMap() {
        Database db = Database.open(":memory:");
        Database.Result r = db.connect().query("RETURN [1.5, null] AS xs, {k: 'v'} AS m");
        List<Object> row = r.rows().get(0);
        assertEquals(java.util.Arrays.asList(1.5, null), row.get(0));
        assertEquals(Map.of("k", "v"), row.get(1));
    }

    @Test
    void mapKeyWithInteriorNulIsRefusedNotTruncated() {
        // map_get takes a NUL-terminated key (the C ABI), so a key with an
        // interior NUL cannot be looked up. Without the refusal the lookup
        // would truncate at the NUL and land on the "a" key — silently
        // returning 2, a different key's value.
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        RelateException e = assertThrows(RelateException.class,
                () -> conn.query("RETURN {'a\\u0000b': 1, a: 2} AS m"));
        assertTrue(e.getMessage().contains("interior NUL"), () -> e.getMessage());
    }

    @Test
    void nodeHydratesTyped() {
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        conn.query("CREATE (a:Person {name: 'Ada', born: 1815})");
        Database.Result r = conn.query("MATCH (a:Person) RETURN a");
        Node node = (Node) r.rows().get(0).get(0);
        assertNotNull(node.elementId(), "the element id is always present on the default namespace");
        assertEquals(List.of("Person"), node.labels());
        assertEquals(Map.of("name", "Ada", "born", 1815L), node.properties());
        // The legacy numeric id (ADR-0506) equals what id(n) returns
        Database.Result ids = conn.query("MATCH (a:Person) RETURN id(a) AS i");
        assertEquals(ids.rows().get(0).get(0), node.id(), "node.id() is id(n)");
    }

    @Test
    void relationshipHydratesTyped() {
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        conn.query("CREATE (a:Person {name: 'Ada'})-[k:KNOWS {since: 1833}]->(b:Person {name: 'Charles'})");
        Database.Result r = conn.query("MATCH ()-[k:KNOWS]->() RETURN k");
        Relationship rel = (Relationship) r.rows().get(0).get(0);
        assertNotNull(rel.elementId());
        assertEquals("KNOWS", rel.type());
        assertNotNull(rel.startElementId());
        assertNotNull(rel.endElementId());
        assertNotEquals(rel.startElementId(), rel.endElementId());
        assertEquals(Map.of("since", 1833L), rel.properties());
        // The ids round-trip: WHERE elementId(n) = $id shape works
        Database.Result ends = conn.query(
                "MATCH (a)-[k:KNOWS]->(b) WHERE elementId(a) = $start AND elementId(b) = $end"
                        + " RETURN a.name AS from, b.name AS to",
                Map.of("start", rel.startElementId(), "end", rel.endElementId()));
        assertEquals("Ada", ends.rows().get(0).get(0));
        assertEquals("Charles", ends.rows().get(0).get(1));
        // The legacy numeric ids (ADR-0506) equal what id(r) / id(startNode(r))
        // / id(endNode(r)) return
        Database.Result ids = conn.query(
                "MATCH ()-[k:KNOWS]->() RETURN id(k) AS rid, id(startNode(k)) AS sid,"
                        + " id(endNode(k)) AS eid");
        List<Object> idRow = ids.rows().get(0);
        assertEquals(idRow.get(0), rel.id(), "rel.id() is id(r)");
        assertEquals(idRow.get(1), rel.startId(), "rel.startId() is id(startNode(r))");
        assertEquals(idRow.get(2), rel.endId(), "rel.endId() is id(endNode(r))");
    }

    @Test
    void pathHydratesAlternatingElements() {
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        conn.query("CREATE (a:Person {name: 'Ada'})-[:KNOWS]->(b:Person {name: 'Charles'})");
        Database.Result r = conn.query("MATCH p = (a:Person)-[k]->(b:Person) RETURN p");
        Path path = (Path) r.rows().get(0).get(0);
        assertEquals(3, path.elements().size(), "one hop: node, relationship, node");
        assertInstanceOf(Node.class, path.elements().get(0));
        assertInstanceOf(Relationship.class, path.elements().get(1));
        assertInstanceOf(Node.class, path.elements().get(2));
    }

    @Test
    void temporalsHydrateAsJavaTime() {
        Database db = Database.open(":memory:");
        Database.Result r = db.connect().query(
                "RETURN date('1815-12-10') AS d, localdatetime('2026-01-02T03:04:05') AS ldt,"
                        + " localtime('09:30:00') AS lt");
        List<Object> row = r.rows().get(0);
        assertEquals(LocalDate.of(1815, 12, 10), row.get(0));
        assertEquals(LocalDateTime.of(2026, 1, 2, 3, 4, 5), row.get(1));
        assertEquals(LocalTime.of(9, 30, 0), row.get(2));
    }

    @Test
    void zonedTemporalsKeepOffsetOrZone() {
        Database db = Database.open(":memory:");
        Database.Result r = db.connect().query(
                "RETURN time('09:30:00+01:00') AS t, datetime('2026-11-04T09:30:00+01:00') AS odt,"
                        + " datetime({year: 2026, month: 11, day: 4, timezone: 'Europe/Berlin'}) AS zdt");
        List<Object> row = r.rows().get(0);
        assertEquals(OffsetTime.of(9, 30, 0, 0, ZoneOffset.ofHours(1)), row.get(0));
        assertEquals(OffsetDateTime.of(2026, 11, 4, 9, 30, 0, 0, ZoneOffset.ofHours(1)), row.get(1));
        assertEquals(ZonedDateTime.of(2026, 11, 4, 0, 0, 0, 0, ZoneId.of("Europe/Berlin")), row.get(2));
    }

    @Test
    void durationHydratesAsItsFourComponents() {
        assertEquals(new DurationValue(14, 0, 0, 0), cell("RETURN duration('P1Y2M')"));
        // PT36H stays 36 hours of seconds: it is not P1DT12H (ADR-0502 §1)
        assertEquals(new DurationValue(0, 0, 36 * 3600, 0), cell("RETURN duration('PT36H')"));
    }

    @Test
    void pointHydratesTyped() {
        assertEquals(new Point(7203, 1.0, 2.0, null), cell("RETURN point({x: 1, y: 2})"));
        assertEquals(new Point(9157, 1.0, 2.0, 3.0), cell("RETURN point({x: 1, y: 2, z: 3})"));
        assertEquals(new Point(4326, 12.5, 55.6, null), cell("RETURN point({longitude: 12.5, latitude: 55.6})"));
    }

    @Test
    void vectorHydratesTypedByDtype() {
        assertEquals(new VectorValue("INTEGER8", List.of(1L, 2L, 3L)),
                cell("RETURN vector([1, 2, 3], 3, INTEGER8)"));
        assertEquals(new VectorValue("FLOAT32", List.of(0.25, -1.0)),
                cell("RETURN vector([0.25, -1.0], 2, FLOAT32)"));
    }

    @Test
    void nestedStructuralKindsStayStructural() {
        // A user map shaped like a node is still a MAP; a path nested in a
        // list is still a PATH (ADR-0502 §1 — no shape sniffing).
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        conn.query("CREATE (a:P)-[:K]->(b:P)");
        Database.Result r = conn.query(
                "MATCH p = (a:P)-[k]->(b:P) RETURN [{elementId: elementId(a), labels: labels(a),"
                        + " properties: properties(a)}] AS fakeNode, [p] AS pathInList");
        List<Object> row = r.rows().get(0);
        List<Object> fakeNodeList = (List<Object>) row.get(0);
        Map<String, Object> fakeNode = (Map<String, Object>) fakeNodeList.get(0);
        assertTrue(fakeNode.containsKey("elementId"), "a user map shaped like a node stays a MAP: "
                + fakeNode);
        List<Object> pathInList = (List<Object>) row.get(1);
        assertInstanceOf(Path.class, pathInList.get(0), "a nested path stays a PATH");
    }
}
