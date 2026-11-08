package com.relate;

import org.junit.jupiter.api.Test;
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
        for (String qt : new String[] {"r", "w", "rw", "s"}) {
            assertEquals(qt, ResultReader.decodeQueryType(qt));
        }
        for (String ns : new String[] {"neo4j", "system", "system_ontologies"}) {
            assertEquals(ns, ResultReader.decodeNamespace(ns));
        }
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
}
