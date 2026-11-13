package ai.relatedb;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.function.Executable;
import static org.junit.jupiter.api.Assertions.*;

/**
 * The error channel of ABI v2 (ADR-0502 §2): one JSON shape travels with
 * the failing call, and the code + GQLSTATUS drive the exception class —
 * no message-text classification anywhere.
 */
public class ErrorsTest {

    @Test
    void arithmeticErrorIsAClientException() {
        Database db = Database.open(":memory:");
        ClientException e = assertThrows(ClientException.class,
                () -> db.connect().query("RETURN 1 / 0"));
        assertEquals("Neo.ClientError.Statement.ArithmeticError", e.getCode());
        assertEquals("22G03", e.getGqlStatus());
        assertEquals(RelateException.Stage.RUNTIME, e.getStage());
        assertFalse(e.isRetryable());
        assertNotNull(e.getMessage());
    }

    @Test
    void syntaxErrorIsAClientExceptionAtCompileStage() {
        Database db = Database.open(":memory:");
        ClientException e = assertThrows(ClientException.class,
                () -> db.connect().query("RETURN"));
        assertEquals("Neo.ClientError.Statement.SyntaxError", e.getCode());
        assertEquals(RelateException.Stage.COMPILE, e.getStage());
        assertFalse(e.isRetryable());
    }

    @Test
    void missingParameterIsAClientException() {
        Database db = Database.open(":memory:");
        ClientException e = assertThrows(ClientException.class,
                () -> db.connect().query("RETURN $missing"));
        assertNotNull(e.getCode());
        assertTrue(e.getCode().startsWith("Neo.ClientError."), e.getCode());
    }

    @Test
    void unknownNamespaceIsAClientException() {
        Database db = Database.open(":memory:");
        ClientException e = assertThrows(ClientException.class, () -> db.connect("nope"));
        assertTrue(e.getMessage().endsWith("Graph not found: nope"),
                "unexpected message: " + e.getMessage());
    }

    @Test
    void writeWriteConflictIsTransientAndRetryable() {
        Database db = Database.open(":memory:");
        Database.Connection winner = db.connect();
        Database.Connection loser = db.connect();
        winner.query("CREATE CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE");
        winner.begin();
        loser.begin();
        winner.query("CREATE (:P {id: 77})");
        loser.query("CREATE (:P {id: 77})");
        assertNotNull(winner.commit(), "the winning transaction wrote");
        // The conflict surfaces on the commit call's own error channel
        TransientException e = assertThrows(TransientException.class, () -> loser.commit());
        assertEquals("Neo.TransientError.Transaction.Outdated", e.getCode());
        assertTrue(e.isRetryable(), "the retry verdict comes from the engine's table");
        assertEquals(RelateException.Stage.RUNTIME, e.getStage());
    }

    @Test
    void theConflictingConnectionRecoversAfterRollback() {
        Database db = Database.open(":memory:");
        Database.Connection winner = db.connect();
        winner.query("CREATE CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE");
        Database.Connection loser = db.connect();
        winner.begin();
        loser.begin();
        winner.query("CREATE (:P {id: 77})");
        loser.query("CREATE (:P {id: 77})");
        assertNotNull(winner.commit());
        assertThrows(TransientException.class, () -> loser.commit());
        // The failed commit ended the transaction (no rollback needed);
        // the connection itself is still usable for new work
        assertEquals(1L, loser.query("MATCH (n:P) RETURN count(n) AS n").rows().get(0).get(0));
        loser.query("CREATE (:P {id: 78})");
        assertEquals(2L, loser.query("MATCH (n:P) RETURN count(n) AS n").rows().get(0).get(0));
    }

    @Test
    void malformedErrorJsonFailsLoudly() {
        RelateException e = assertThrows(RelateException.class,
                () -> RelateException.fromErrorJson(
                        "{\"code\":\"Neo.ClientError.Statement.ArithmeticError\"}", "test"));
        assertTrue(e.getMessage().contains("gql_status"), "names the missing key: " + e.getMessage());
        assertNull(e.getCode(), "a malformed shape is not classified");
    }

    @Test
    void aNonObjectErrorShapeRefusesThroughTheOnePolicy() {
        // The object guard is total: a null node (an empty document) and a
        // non-object node refuse through the one Json policy
        RelateException e = assertThrows(RelateException.class,
                () -> RelateException.fromErrorJson("[]", "test"));
        assertTrue(e.getMessage().contains("not a JSON object"), e.getMessage());
        RelateException empty = assertThrows(RelateException.class,
                () -> RelateException.fromErrorJson("", "test"));
        assertNotNull(empty.getMessage(), "an empty document refuses too");
    }

    @Test
    void theStageEnumDecodesTheWireNames() {
        // The decode contract: each ABI wire name maps to its stage — a
        // swapped table cannot hide — and an unknown name is the decode
        // site's refusal (null here), never a wrong stage
        assertEquals(RelateException.Stage.COMPILE, RelateException.Stage.fromWire("compile"));
        assertEquals(RelateException.Stage.RUNTIME, RelateException.Stage.fromWire("runtime"));
        assertEquals(RelateException.Stage.INTERNAL, RelateException.Stage.fromWire("internal"));
        assertNull(RelateException.Stage.fromWire("nope"));
    }

    @Test
    void anUnknownErrorStageIsAnAbiContractViolation() {
        // LABS-930: the stage decodes into the closed enum — compile,
        // runtime, internal — and a value outside the three is an ABI
        // contract violation, never a lenient string (the Go and Swift
        // clients' contract)
        RelateException e = assertThrows(RelateException.class,
                () -> RelateException.fromErrorJson(
                        "{\"code\":\"Neo.ClientError.Statement.ArithmeticError\","
                                + "\"gql_status\":\"22G03\",\"stage\":\"nope\","
                                + "\"retryable\":false,\"message\":\"x\"}", "test"));
        assertTrue(e.getMessage().contains("unknown stage"), "names the stage: " + e.getMessage());
        assertTrue(e.getMessage().contains("nope"), "names the value: " + e.getMessage());
        assertNull(e.getCode(), "a malformed shape is not classified");
    }

    @Test
    void closedConnectionFailsLoudly() {
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        conn.close();
        conn.close(); // idempotent
        RelateException e = assertThrows(RelateException.class,
                () -> conn.query("RETURN 1"));
        assertEquals("connection is closed", e.getMessage());
    }

    @Test
    void resultMetadataVocabulariesRefuseUnknownValues() {
        for (QueryType qt : QueryType.values()) {
            assertEquals(qt, ResultReader.decodeQueryType(qt.wire()));
        }
        assertEquals(QueryType.READ_WRITE, ResultReader.decodeQueryType("rw"));
        for (Namespace ns : Namespace.values()) {
            assertEquals(ns, ResultReader.decodeNamespace(ns.wire()));
        }
        assertEquals(Namespace.SYSTEM_ONTOLOGIES, ResultReader.decodeNamespace("system_ontologies"));
        for (String bad : new String[] {"", "R", "x", null}) {
            RelateException e = assertThrows(RelateException.class,
                    () -> ResultReader.decodeQueryType(bad));
            assertTrue(e.getMessage().startsWith("ABI contract violated: query type"),
                    e.getMessage());
        }
        for (String bad : new String[] {"", "Neo4j", "other", null}) {
            RelateException e = assertThrows(RelateException.class,
                    () -> ResultReader.decodeNamespace(bad));
            assertTrue(e.getMessage().startsWith("ABI contract violated: namespace"),
                    e.getMessage());
        }
    }

    @Test
    void anUnknownStageWithControlCharactersIsSanitized() {
        // Oplane OPLANE_REQ-00351534 (CWE-117): the untrusted stage value is
        // echoed into the refusal — CR, LF, ESC, NUL and the Unicode
        // separators must arrive escaped, never verbatim.
        String[] hostile = {"a\\u000db", "a\\u000ab", "a\\u001bb", "a\\u0000b",
                "a\\u2028b", "a\\u2029b", "a\\u200bb", "x".repeat(400)};
        for (String value : hostile) {
            RelateException e = assertThrows(RelateException.class,
                    () -> RelateException.fromErrorJson(
                            "{\"code\":\"c\",\"gql_status\":\"g\",\"stage\":\"" + value + "\","
                                    + "\"retryable\":false,\"message\":\"m\"}", "test"));
            assertTrue(e.getMessage().contains("unknown stage"), e.getMessage());
            WireSanitizeAssertions.assertNoRawUnprintables(e.getMessage());
            assertTrue(e.getMessage().length() < 2000, "bounded: " + e.getMessage().length());
        }
    }

    @Test
    void resultMetadataRefusalsSanitizeTheOffendingValue() {
        // Oplane OPLANE_REQ-00351534 (CWE-117): decodeWire echoes the raw
        // ABI string; a hostile native library must not smuggle control
        // characters into the message.
        String[] hostile = {"a\rb", "a\nb", "a\u001bb", "a\u0000b", "a\u2028b",
                "a\u2029b", "a\u200bb", "a\"b\\c", "x".repeat(500)};
        for (String bad : hostile) {
            assertDecodeRefuses(() -> ResultReader.decodeQueryType(bad),
                    "ABI contract violated: query type");
            assertDecodeRefuses(() -> ResultReader.decodeNamespace(bad),
                    "ABI contract violated: namespace");
        }
    }

    /** One decode-refusal assertion: the prefix holds, no raw non-printable, bounded. */
    private static void assertDecodeRefuses(Executable decode, String prefix) {
        RelateException e = assertThrows(RelateException.class, decode);
        assertTrue(e.getMessage().startsWith(prefix), e.getMessage());
        WireSanitizeAssertions.assertNoRawUnprintables(e.getMessage());
        assertTrue(e.getMessage().length() < 200, "bounded: " + e.getMessage().length());
    }

    @Test
    void aQuoteOrBackslashInAnUnknownValueCannotBreakTheMessageQuoting() {
        RelateException e = assertThrows(RelateException.class,
                () -> ResultReader.decodeQueryType("a\"b\\c"));
        // The value's own quote and backslash arrive escaped, so the
        // message's surrounding quotes stay its own.
        assertTrue(e.getMessage().contains("\\u0022"), e.getMessage());
        assertTrue(e.getMessage().contains("\\u005c"), e.getMessage());
        WireSanitizeAssertions.assertNoRawUnprintables(e.getMessage());
    }

    @Test
    void theSharedRefusalSanitizesAndBoundsTheEchoedPayload() throws Exception {
        // The payload side of requireStage/requireWire goes through the
        // same sanitizer: control characters escaped, long payloads cut.
        JsonNode node = Json.STRICT.readTree("{\"stage\":\"nope\"}");
        String hostilePayload = WireSanitizeAssertions.HOSTILE_PAYLOAD;
        RelateException e = assertThrows(RelateException.class,
                () -> Json.requireStage(node, "stage", "error shape", hostilePayload));
        assertTrue(e.getMessage().contains("unknown stage"), e.getMessage());
        assertTrue(e.getMessage().contains("...(truncated)"), e.getMessage());
        WireSanitizeAssertions.assertNoRawUnprintables(e.getMessage());
        assertTrue(e.getMessage().length() < 512, "bounded: " + e.getMessage().length());
    }

    @Test
    void everyRequiredKeyRefusalSanitizesAndBoundsTheEchoedPayload() throws Exception {
        // Oplane OPLANE_REQ-00351534 (CWE-117): every Json refusal echoes
        // the raw payload — requireText/requireBool/requireObject/optionalLong
        // must sanitize and bound it like the enum-decode sites.
        String hostile = WireSanitizeAssertions.HOSTILE_PAYLOAD;
        JsonNode empty = Json.STRICT.readTree("{}");
        JsonNode array = Json.STRICT.readTree("[]");
        JsonNode nonLong = Json.STRICT.readTree("{\"n\":1.5}");
        List<Executable> refusals = List.of(
                () -> Json.requireText(empty, "k", "error shape", hostile),
                () -> Json.requireBool(empty, "k", "error shape", hostile),
                () -> Json.requireObject(array, "error shape", hostile),
                () -> Json.optionalLong(nonLong, "n", "error shape", hostile));
        for (Executable refusal : refusals) {
            RelateException e = assertThrows(RelateException.class, refusal);
            WireSanitizeAssertions.assertNoRawUnprintables(e.getMessage());
            assertTrue(e.getMessage().length() < 512, "bounded: " + e.getMessage().length());
        }
    }

    @Test
    void aWhitespaceOnlyErrorShapeIsSanitizedInTheRefusal() {
        // Oplane OPLANE_REQ-00351534 (CWE-117): fromErrorJson passes the
        // raw error JSON as the refusal payload, and CR/LF between JSON
        // tokens is legal whitespace — a missing-key refusal must not echo
        // it verbatim.
        RelateException e = assertThrows(RelateException.class,
                () -> RelateException.fromErrorJson("{\r\n}", "test"));
        assertTrue(e.getMessage().contains("is missing"), e.getMessage());
        assertTrue(e.getMessage().contains("\\u000d"), e.getMessage());
        WireSanitizeAssertions.assertNoRawUnprintables(e.getMessage());
    }

    @Test
    void theEchoedPayloadKeepsItsStructuralQuotesAndBackslashes() throws Exception {
        // The payload is a whole document appended bare (not inside the
        // message's quotes), so only its non-printables escape — its own
        // quotes and backslashes stay legible for a reader.
        JsonNode node = Json.STRICT.readTree("{}");
        RelateException e = assertThrows(RelateException.class,
                () -> Json.requireText(node, "code", "error shape", "{\"a\":\"b\\c\"}"));
        assertTrue(e.getMessage().contains("{\"a\":\"b\\c\"}"), e.getMessage());
        assertFalse(e.getMessage().contains("\\u0022"), e.getMessage());
        WireSanitizeAssertions.assertNoRawUnprintables(e.getMessage());
    }

    @Test
    void aSupplementaryPlaneFormatCodePointIsEscaped() {
        // U+E0001 (LANGUAGE TAG) is a supplementary-plane FORMAT code point:
        // two UTF-16 code units, escaped as the surrogate pair rather than
        // scanned as two unrelated chars.
        String languageTag = new String(Character.toChars(0xE0001));
        RelateException e = assertThrows(RelateException.class,
                () -> ResultReader.decodeQueryType("a" + languageTag + "b"));
        assertTrue(e.getMessage().contains("\\udb40"), e.getMessage());
        assertTrue(e.getMessage().contains("\\udc01"), e.getMessage());
        WireSanitizeAssertions.assertNoRawUnprintables(e.getMessage());
    }
}
