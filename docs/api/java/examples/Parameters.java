package ai.relatedb.examples;

import ai.relatedb.ClientException;
import ai.relatedb.Database;

import java.time.LocalDate;
import java.time.OffsetDateTime;
import java.time.ZoneId;
import java.time.ZoneOffset;
import java.time.ZonedDateTime;
import java.util.List;
import java.util.Map;

/**
 * The parameterised query and the typed error surface (the typed ABI v2,
 * ADR-0502): one putter per Java type, no JSON on the way, and errors
 * classified by the engine's code + GQLSTATUS — never by message text.
 */
public class Parameters {
    public static void main(String[] args) {
        try (Database db = Database.open(":memory:");
             Database.Connection conn = db.connect()) {
            conn.query("CREATE (p:Person {name: 'Ada', born: date('1815-12-10')})");

            // Scalars, temporals, points, vectors and nested containers
            // all bind as their natural types
            var found = conn.query(
                    "MATCH (p:Person) WHERE p.name = $name AND p.born = $born RETURN p.name AS name",
                    Map.of("name", "Ada", "born", LocalDate.of(1815, 12, 10)));
            if (!"Ada".equals(found.rows().get(0).get(0))) {
                throw new IllegalStateException("unexpected");
            }

            // The temporal and typed-value forms (round-trip through the
            // value walker)
            var typed = conn.query(
                    "RETURN $at AS at, $tags AS tags, $emb AS emb",
                    Map.of(
                            "at", ZonedDateTime.of(2026, 11, 4, 9, 30, 0, 0, ZoneId.of("Europe/Berlin")),
                            "tags", List.of("x", 2L),
                            "emb", new ai.relatedb.VectorValue("FLOAT32", List.of(0.25, -1.0))));
            Object at = typed.rows().get(0).get(0);
            if (!(at instanceof ZonedDateTime zoned)
                    || !zoned.getZone().equals(ZoneId.of("Europe/Berlin"))) {
                throw new IllegalStateException("a zoned datetime keeps its zone: " + at);
            }

            // A failing statement throws the typed exception: the code and
            // GQLSTATUS drive the class, isRetryable() drives the retry
            try {
                conn.query("RETURN 1 / 0");
                throw new IllegalStateException("expected failure");
            } catch (ClientException e) {
                if (!"Neo.ClientError.Statement.ArithmeticError".equals(e.getCode())
                        || !"22G03".equals(e.getGqlStatus())
                        || e.isRetryable()) {
                    throw new IllegalStateException("unexpected classification: " + e.getCode());
                }
            }

            // Offset-only datetimes cross as their offset
            var offset = conn.query("RETURN $at AS at",
                    Map.of("at", OffsetDateTime.of(2026, 11, 4, 9, 30, 0, 0, ZoneOffset.ofHours(1))));
            if (!(offset.rows().get(0).get(0) instanceof OffsetDateTime)) {
                throw new IllegalStateException("an offset datetime stays offset-only");
            }
        }
    }
}
