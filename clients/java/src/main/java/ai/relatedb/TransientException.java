package ai.relatedb;

/**
 * The transient class: the engine marked the failure retryable — today the
 * write-write conflict ({@code Neo.TransientError.Transaction.Outdated})
 * and the retryable workspace cap. A retry loop re-runs the work; the
 * conflict can surface on a statement's error result (auto-commit) or on
 * {@code commit()} (explicit transaction), so a managed-transaction retry
 * loop must catch it in both places.
 */
public class TransientException extends RelateException {
    private static final long serialVersionUID = 1L;

    TransientException(String message, String code, String gqlStatus, Stage stage) {
        super(message, code, gqlStatus, stage, true, null);
    }
}
