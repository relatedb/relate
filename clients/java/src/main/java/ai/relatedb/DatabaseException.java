package ai.relatedb;

/**
 * The database error class: the engine classified the failure as its own
 * ({@code Neo.DatabaseError.*}), including the ABI boundary's internal
 * stage (a poisoned handle, a caught panic, an argument that failed to
 * decode) which genericizes to {@code Neo.DatabaseError.General.UnknownError}.
 * Never retryable: recover by closing the handle and opening a new one.
 */
public class DatabaseException extends RelateException {
    private static final long serialVersionUID = 1L;

    DatabaseException(String message, String code, String gqlStatus, Stage stage) {
        super(message, code, gqlStatus, stage, false, null);
    }
}
