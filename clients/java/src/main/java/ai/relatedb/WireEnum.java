package ai.relatedb;

/**
 * A closed ABI vocabulary mapped to a Java enum: every constant carries
 * the exact wire string the ABI emits, and {@link #fromWire} is the one
 * lookup the decoders share — a value outside the vocabulary is an ABI
 * contract violation each decode site refuses with its own context, never
 * a lenient string.
 */
interface WireEnum {

    /** The ABI's wire string for this constant. */
    String wire();

    /**
     * The constant of {@code type} whose wire string is {@code raw}, or
     * null when {@code raw} (possibly null itself) is outside the
     * vocabulary.
     */
    static <E extends Enum<E> & WireEnum> E fromWire(Class<E> type, String raw) {
        for (E constant : type.getEnumConstants()) {
            if (constant.wire().equals(raw)) {
                return constant;
            }
        }
        return null;
    }
}
