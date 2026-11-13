package ai.relatedb;

/**
 * The client error class: the engine classified the failure as the
 * caller's fault ({@code Neo.ClientError.*}) — a syntax error, a type
 * error, an unknown namespace, a write against a read-only database.
 * Never retryable: re-running the same statement fails the same way.
 */
public class ClientException extends RelateException {
    private static final long serialVersionUID = 1L;

    ClientException(String message, String code, String gqlStatus, Stage stage) {
        super(message, code, gqlStatus, stage, false, null);
    }
}
