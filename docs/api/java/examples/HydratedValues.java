package ai.relatedb.examples;

/**
 * The one canonical hydrated-value comparison for the docs examples. The
 * typed value walker (ADR-0502 §1) hydrates every Cypher integer as a
 * {@code Long} and every float as a {@code Double}; this helper normalizes
 * an integral {@code Number} on either side to {@code Long} before
 * comparing, so a literal {@code 1} in an example matches the hydrated
 * {@code 1L}. Anything else compares by equals and a mismatch fails
 * loudly.
 */
final class HydratedValues {

    private HydratedValues() {
    }

    /** Normalize an integral number to Long; pass everything else through. */
    static Object canon(Object value) {
        return value instanceof Integer || value instanceof Short || value instanceof Byte
                ? ((Number) value).longValue()
                : value;
    }

    /** True when the two hydrated values are equal after normalization. */
    static boolean equal(Object actual, Object expected) {
        return java.util.Objects.equals(canon(actual), canon(expected));
    }
}
