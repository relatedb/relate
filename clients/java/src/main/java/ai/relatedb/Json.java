package ai.relatedb;

import com.fasterxml.jackson.databind.DeserializationFeature;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

/**
 * The package's one Jackson home: the mapper pair and the wire-key
 * refusal helpers. Jackson decodes only the ABI's JSON metadata payloads
 * — query notifications, the plan document and the error shape; result
 * cells cross through the typed value walker (ADR-0502). The strict
 * mapper validates every key it reads; the plan mapper tolerates unknown
 * keys (LABS-836: the engine keeps adding plan fields). Every wire-key
 * refusal lives here — each decoder passes its own context string, never
 * a hand-rolled sibling.
 */
final class Json {

    static final ObjectMapper STRICT = new ObjectMapper();

    /** The plan document mapper: unknown keys tolerated (LABS-836). */
    static final ObjectMapper PLAN = new ObjectMapper()
            .configure(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES, false);

    /** The display bound for one offending wire value in a refusal message. */
    static final int VALUE_MAX = 64;

    /** The display bound for the echoed JSON payload in a refusal message. */
    static final int PAYLOAD_MAX = 256;

    private static final char[] HEX = "0123456789abcdef".toCharArray();

    private Json() {
    }

    /**
     * A required textual wire key, the one shared refusal: a missing or
     * mistyped key throws naming the context and the payload — never a
     * silent fallback. Every JSON decoder in the package (the error shape,
     * the notification entries) consults this one, each passing its own
     * context string.
     */
    static String requireText(JsonNode node, String key, String what, String payload) {
        JsonNode value = node.get(key);
        if (value == null || !value.isTextual()) {
            throw new RelateException(
                    what + " is missing the \"" + key + "\" string: " + safe(payload, PAYLOAD_MAX));
        }
        return value.asText();
    }

    /** A required boolean wire key — the exact sibling of {@link #requireText}. */
    static boolean requireBool(JsonNode node, String key, String what, String payload) {
        JsonNode value = node.get(key);
        if (value == null || !value.isBoolean()) {
            throw new RelateException(
                    what + " is missing the \"" + key + "\" boolean: " + safe(payload, PAYLOAD_MAX));
        }
        return value.asBoolean();
    }

    /**
     * A required stage wire key: the closed {@link RelateException.Stage}
     * vocabulary, refused in the one message shape when the wire name is
     * outside it — never a lenient string.
     */
    static RelateException.Stage requireStage(JsonNode node, String key, String what,
                                                  String payload) {
        String wire = requireText(node, key, what, payload);
        RelateException.Stage stage = RelateException.Stage.fromWire(wire);
        if (stage == null) {
            throw new RelateException(
                    what + " carries an unknown stage " + quoted(wire) + ": "
                            + safe(payload, PAYLOAD_MAX));
        }
        return stage;
    }

    /**
     * A required closed-vocabulary wire key: the {@link WireEnum} constant
     * for the text, refused in the one message shape when the text is
     * outside the vocabulary — never a lenient string.
     */
    static <E extends Enum<E> & WireEnum> E requireWire(Class<E> type, JsonNode node,
                                                         String key, String what,
                                                         String payload) {
        String wire = requireText(node, key, what, payload);
        E constant = WireEnum.fromWire(type, wire);
        if (constant == null) {
            throw new RelateException(
                    what + " carries an unknown " + key + " " + quoted(wire)
                            + ": " + safe(payload, PAYLOAD_MAX));
        }
        return constant;
    }

    /**
     * A required JSON object — total: a null node (an empty document
     * parses to one) and any other shape refuse in the one message shape.
     */
    static JsonNode requireObject(JsonNode node, String what, String payload) {
        if (node == null || !node.isObject()) {
            throw new RelateException(what + " is not a JSON object: " + safe(payload, PAYLOAD_MAX));
        }
        return node;
    }

    /**
     * An optional integer wire key: absent or JSON null is a legitimate
     * absence ({@code null}); a value that is not an integer, or an
     * integer outside the long range, refuses in the one message shape —
     * never a silent truncation to the low 64 bits.
     */
    static Long optionalLong(JsonNode node, String key, String what, String payload) {
        JsonNode value = node.get(key);
        if (value == null || value.isNull()) {
            return null;
        }
        // canConvertToLong bounds the range: a BigIntegerNode is integral
        // yet does not fit a long
        if (!value.isIntegralNumber() || !value.canConvertToLong()) {
            throw new RelateException(
                    what + " has a non-long \"" + key + "\": " + safe(payload, PAYLOAD_MAX));
        }
        return value.longValue();
    }

    /**
     * Whether {@code codePoint} is a control or non-printable code point that
     * must never appear raw in a refusal message (CWE-117 log injection):
     * every ISO control (C0, C1 and DEL), the Unicode line/paragraph
     * separators, and unassigned or format code points. This is the
     * message-safety policy {@link #safe} and {@link #quoted} share; the
     * delimiter clause (a value's own quote and backslash) is {@link #quoted}'s
     * alone, because only a quoted value can break the message's own quoting.
     */
    static boolean isUnprintable(int codePoint) {
        if (Character.isISOControl(codePoint) || codePoint == 0x2028 || codePoint == 0x2029) {
            return true;
        }
        int type = Character.getType(codePoint);
        return type == Character.UNASSIGNED || type == Character.FORMAT;
    }

    /**
     * The echoed JSON payload of a refusal: a whole document appended bare
     * after the message's own text, so only its non-printable code points are
     * escaped (its structural quotes and backslashes stay legible) and the
     * result is bounded to {@code max} characters with a {@code ...(truncated)}
     * marker when cut. {@code null} renders as {@code null}.
     */
    static String safe(String s, int max) {
        return escape(s, max, false);
    }

    /**
     * The display form of an offending wire value: {@code null} renders as
     * the unquoted {@code null} (a NULL pointer, distinct from the literal
     * string {@code "null"}), any other value as its sanitized form wrapped
     * in double quotes — the one place a refusal quotes a value, so the
     * quoting (and the escape of the value's own quote and backslash) cannot
     * drift between the decode sites.
     */
    static String quoted(String s) {
        return s == null ? "null" : "\"" + escape(s, VALUE_MAX, true) + "\"";
    }

    /**
     * Escape {@code s} for a refusal message: every non-printable code point
     * — and, when {@code inQuotes} (the value sits inside the message's own
     * quotes), the value's own quote and backslash — becomes its
     * {@code \\uXXXX} escape (a supplementary code point as its surrogate
     * pair), the escaped body is bounded to {@code max} characters, and a
     * {@code ...(truncated)} marker is appended when the input is cut.
     */
    private static String escape(String s, int max, boolean inQuotes) {
        if (s == null) {
            return "null";
        }
        StringBuilder out = new StringBuilder(Math.min(s.length(), max) + 16);
        int i = 0;
        while (i < s.length()) {
            int codePoint = s.codePointAt(i);
            int units = Character.charCount(codePoint);
            boolean mustEscape = isUnprintable(codePoint)
                    || (inQuotes && (codePoint == '\\' || codePoint == '"'));
            int width = mustEscape ? 6 * units : units;
            if (out.length() + width > max) {
                break;
            }
            if (mustEscape) {
                for (char unit : Character.toChars(codePoint)) {
                    appendEscaped(out, unit);
                }
            } else {
                out.appendCodePoint(codePoint);
            }
            i += units;
        }
        if (i < s.length()) {
            out.append("...(truncated)");
        }
        return out.toString();
    }

    /** Append {@code c} as a {@code \\uXXXX} escape. */
    private static void appendEscaped(StringBuilder out, char c) {
        out.append("\\u");
        out.append(HEX[(c >> 12) & 0xF]);
        out.append(HEX[(c >> 8) & 0xF]);
        out.append(HEX[(c >> 4) & 0xF]);
        out.append(HEX[c & 0xF]);
    }
}
