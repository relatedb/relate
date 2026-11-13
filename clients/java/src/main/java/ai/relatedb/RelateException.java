package ai.relatedb;

import com.fasterxml.jackson.databind.JsonNode;

import java.util.Locale;

/**
 * The base exception of the Java client, carrying the typed error shape of
 * ABI v2 (ADR-0502 §2) whole: the Neo4j status {@code code}, the
 * {@code gql_status}, the {@code stage} — the closed {@link Stage} enum:
 * {@code COMPILE}, {@code RUNTIME} or {@code INTERNAL} — the
 * {@code retryable} verdict and the message. A failure the ABI classified
 * throws one of the subclasses — {@link TransientException} exactly when
 * {@code retryable} is true, {@link ClientException} for
 * {@code Neo.ClientError.*} codes and {@link DatabaseException} for
 * {@code Neo.DatabaseError.*} codes — so a caller never classifies by
 * message text (the classification is the engine's single table,
 * {@code relate::error_info}).
 *
 * <p>Constructors without the classification fields stay for the client's
 * own non-ABI failures (native loading, a malformed ABI payload): those
 * carry a null code and are never retryable.
 */
public class RelateException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    private final String code;
    private final String gqlStatus;
    private final Stage stage;
    private final boolean retryable;

    public RelateException(String message) {
        this(message, null, null, null, false, null);
    }

    public RelateException(String message, Throwable cause) {
        this(message, null, null, null, false, cause);
    }

    RelateException(String message, String code, String gqlStatus, Stage stage,
                        boolean retryable, Throwable cause) {
        super(message, cause);
        this.code = code;
        this.gqlStatus = gqlStatus;
        this.stage = stage;
        this.retryable = retryable;
    }

    /** The Neo4j status code, e.g. {@code Neo.ClientError.Statement.ArithmeticError}; null on a non-ABI failure. */
    public String getCode() { return code; }

    /** The GQLSTATUS, e.g. {@code 22G03}; null on a non-ABI failure. */
    public String getGqlStatus() { return gqlStatus; }

    /** Where the failure arose; null on a non-ABI failure. */
    public Stage getStage() { return stage; }

    /** True exactly when re-running the same work may succeed (the retry verdict). */
    public boolean isRetryable() { return retryable; }

    /**
     * Where a failure arose (ADR-0502 §2), as a closed enum — never an
     * unchecked string: a stage value outside the three is an ABI
     * contract violation and throws at decode, the same refusal the Go
     * and Swift clients make.
     */
    public enum Stage {
        /** A parse or plan failure. */
        COMPILE,
        /** An execution failure. */
        RUNTIME,
        /** A failure the engine never saw: a NULL or poisoned handle, a
         *  caught panic, an undecodable argument, a params refusal. */
        INTERNAL;

        /**
         * The ABI's wire name — the enum's own name in the ABI's lower
         * case ({@code "compile"} / {@code "runtime"} / {@code
         * "internal"}), derived, never a second literal table.
         */
        String wire() {
            return name().toLowerCase(Locale.ROOT);
        }

        /**
         * The stage for an ABI wire name, or null when the name is outside
         * the vocabulary — the decode site refuses that loudly with its
         * own context, the same null contract as
         * {@link VectorDtype#fromName}.
         */
        static Stage fromWire(String wire) {
            for (Stage stage : values()) {
                if (stage.wire().equals(wire)) {
                    return stage;
                }
            }
            return null;
        }
    }

    /**
     * Decode the ABI's error shape and throw the classified exception:
     * {@code retryable} drives {@link TransientException}, then the code's
     * classification ({@code Neo.ClientError.*} / {@code Neo.DatabaseError.*})
     * drives the class, exactly as the stock Neo4j drivers classify. A
     * malformed shape — a missing key, a mistyped value, an unknown stage —
     * is an ABI contract violation, never a silent fallback: it throws the
     * base class naming the payload.
     */
    static RelateException fromErrorJson(String json, String what) {
        JsonNode node;
        try {
            node = Json.STRICT.readTree(json);
        } catch (Exception e) {
            throw new RelateException("malformed error shape from " + what + ": " + json, e);
        }
        node = Json.requireObject(node, "error shape", json);
        return classify(Json.requireText(node, "code", "error shape", json),
                Json.requireText(node, "gql_status", "error shape", json),
                Json.requireStage(node, "stage", "error shape", json),
                Json.requireBool(node, "retryable", "error shape", json),
                Json.requireText(node, "message", "error shape", json));
    }

    private static RelateException classify(String code, String gqlStatus,
                                                Stage stage, boolean retryable, String message) {
        if (retryable) {
            return new TransientException(message, code, gqlStatus, stage);
        }
        if (code.startsWith("Neo.ClientError.")) {
            return new ClientException(message, code, gqlStatus, stage);
        }
        if (code.startsWith("Neo.DatabaseError.")) {
            return new DatabaseException(message, code, gqlStatus, stage);
        }
        return new RelateException(message, code, gqlStatus, stage, false, null);
    }
}
