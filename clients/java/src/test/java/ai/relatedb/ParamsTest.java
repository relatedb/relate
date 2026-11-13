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
 * The parameterised query (the typed params builder, ADR-0502 §4): every
 * Java type binds through its own putter, no JSON on the way.
 */
public class ParamsTest {

    /** One-entry parameter map that tolerates a null value (Map.of does not). */
    private static Map<String, Object> params(Object value) {
        Map<String, Object> map = new java.util.HashMap<>();
        map.put("v", value);
        return map;
    }

    private static Object scalar(String cypher, Object value) {
        Database db = Database.open(":memory:");
        Database.Result r = db.connect().query(cypher, params(value));
        assertEquals(1, r.rows().size(), cypher);
        return r.rows().get(0).get(0);
    }

    @Test
    void scalarsBind() {
        assertNull(scalar("RETURN $v", null));
        assertEquals(Boolean.TRUE, scalar("RETURN $v", true));
        assertEquals(9007199254740993L, scalar("RETURN $v", 9007199254740993L),
                "the full int64 range binds losslessly");
        // every integral Java type binds as INT and hydrates back as Long
        assertEquals(1L, scalar("RETURN $v", 1));
        assertEquals(1.5, scalar("RETURN $v", 1.5));
        assertEquals("Ada", scalar("RETURN $v", "Ada"));
    }

    @Test
    void stringWithInteriorNulBindsWhole() {
        String bound = (String) scalar("RETURN $v", "a\u0000b");
        assertEquals("a\u0000b", bound, "the byte-array bind carries the interior NUL");
        assertEquals(3, bound.length());
    }

    @Test
    void temporalsBindAsTheirNaturalTypes() {
        assertEquals(LocalDate.of(1815, 12, 10), scalar("RETURN $v", LocalDate.of(1815, 12, 10)));
        assertEquals(LocalTime.of(9, 30, 0), scalar("RETURN $v", LocalTime.of(9, 30, 0)));
        assertEquals(OffsetTime.of(9, 30, 0, 0, ZoneOffset.ofHours(1)),
                scalar("RETURN $v", OffsetTime.of(9, 30, 0, 0, ZoneOffset.ofHours(1))));
        assertEquals(LocalDateTime.of(2026, 1, 2, 3, 4, 5),
                scalar("RETURN $v", LocalDateTime.of(2026, 1, 2, 3, 4, 5)));
        assertEquals(OffsetDateTime.of(2026, 11, 4, 9, 30, 0, 0, ZoneOffset.ofHours(1)),
                scalar("RETURN $v", OffsetDateTime.of(2026, 11, 4, 9, 30, 0, 0, ZoneOffset.ofHours(1))));
    }

    @Test
    void zonedDateTimeKeepsItsZone() {
        ZonedDateTime in = ZonedDateTime.of(2026, 11, 4, 9, 30, 0, 0, ZoneId.of("Europe/Berlin"));
        assertEquals(in, scalar("RETURN $v", in), "a zoned datetime round-trips with its zone id");
    }

    @Test
    void durationsBind() {
        DurationValue in = new DurationValue(14, 0, 36 * 3600, 0);
        assertEquals(in, scalar("RETURN $v", in));
        // java.time.Duration binds as a zero-months, zero-days duration
        assertEquals(new DurationValue(0, 0, 90, 0), scalar("RETURN $v", java.time.Duration.ofSeconds(90)));
    }

    @Test
    void pointsAndVectorsBind() {
        assertEquals(new Point(4979, 12.5, 55.6, 30.5),
                scalar("RETURN $v", new Point(4979, 12.5, 55.6, 30.5)));
        assertEquals(new Point(7203, 1.0, 2.0, null),
                scalar("RETURN $v", new Point(7203, 1.0, 2.0, null)));
        assertEquals(new VectorValue("FLOAT32", List.of(0.25, -1.0)),
                scalar("RETURN $v", new VectorValue("FLOAT32", List.of(0.25, -1.0))));
        assertEquals(new VectorValue("INTEGER8", List.of(1L, 2L, 3L)),
                scalar("RETURN $v", new VectorValue("INTEGER8", List.of(1L, 2L, 3L))));
    }

    @Test
    void nestedListsAndMapsBind() {
        Database db = Database.open(":memory:");
        Database.Result r = db.connect().query("RETURN $tags AS tags, $opts.strict AS strict",
                Map.of("tags", List.of("x", 2L), "opts", Map.of("strict", true)));
        assertEquals(List.of("x", 2L), r.rows().get(0).get(0));
        assertEquals(Boolean.TRUE, r.rows().get(0).get(1));
    }

    @Test
    void aBoundParameterDrivesAMatch() {
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        conn.query("CREATE (:Person {name: 'Ada'}), (:Person {name: 'Bob'})");
        Database.Result r = conn.query("MATCH (p:Person) WHERE p.name = $name RETURN p.name AS name",
                Map.of("name", "Ada"));
        assertEquals(List.of("Ada"), r.rows().get(0));
    }

    @Test
    void unsupportedJavaTypeRefusesLoudly() {
        Database db = Database.open(":memory:");
        Object unsupported = new Object();
        IllegalArgumentException e = assertThrows(IllegalArgumentException.class,
                () -> db.connect().query("RETURN $v", Map.of("v", unsupported)));
        assertTrue(e.getMessage().contains("Object"), "names the type: " + e.getMessage());
    }

    @Test
    void vectorCoordinateTypeMismatchRefusesLoudly() {
        Database db = Database.open(":memory:");
        IllegalArgumentException e = assertThrows(IllegalArgumentException.class,
                () -> db.connect().query("RETURN $v",
                        Map.of("v", new VectorValue("INTEGER8", List.of(1.5)))));
        assertTrue(e.getMessage().contains("INTEGER8"), "names the dtype: " + e.getMessage());
    }

    @Test
    void vectorCoordinateOutOfRangeRefusesThroughTheErrorChannel() {
        Database db = Database.open(":memory:");
        // 300 does not fit an INTEGER8 coordinate: the putter refuses
        // through the ABI's error channel — a typed RelateException
        // with the internal-stage shape, never a silent coercion
        RelateException e = assertThrows(RelateException.class,
                () -> db.connect().query("RETURN $v",
                        Map.of("v", new VectorValue("INTEGER8", List.of(300L)))));
        assertEquals(RelateException.Stage.INTERNAL, e.getStage());
        assertFalse(e.isRetryable());
        assertNotNull(e.getCode(), "the refusal carries the classified code");
        assertNotNull(e.getGqlStatus());
    }

    @Test
    void nullKeyAndNullCoordinateAreTypedRefusalsNotNpe() {
        Map<Object, Object> nullKey = new java.util.HashMap<>();
        nullKey.put(null, 1);
        IllegalArgumentException key = assertThrows(IllegalArgumentException.class,
                () -> scalar("RETURN $v", nullKey));
        assertTrue(key.getMessage().contains("is null"), key::getMessage);

        List<Object> coordinates = new java.util.ArrayList<>();
        coordinates.add(1L);
        coordinates.add(null);
        IllegalArgumentException coordinate = assertThrows(IllegalArgumentException.class,
                () -> scalar("RETURN $v", new VectorValue("INTEGER8", coordinates)));
        assertTrue(coordinate.getMessage().contains("is null"), coordinate::getMessage);
    }
}
