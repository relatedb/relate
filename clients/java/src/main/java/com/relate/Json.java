package com.relate;

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
                    what + " is missing the \"" + key + "\" string: " + payload);
        }
        return value.asText();
    }

    /** A required boolean wire key — the exact sibling of {@link #requireText}. */
    static boolean requireBool(JsonNode node, String key, String what, String payload) {
        JsonNode value = node.get(key);
        if (value == null || !value.isBoolean()) {
            throw new RelateException(
                    what + " is missing the \"" + key + "\" boolean: " + payload);
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
                    what + " carries an unknown stage \"" + wire + "\": " + payload);
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
                    what + " carries an unknown " + key + " \"" + wire + "\": " + payload);
        }
        return constant;
    }

    /**
     * A required JSON object — total: a null node (an empty document
     * parses to one) and any other shape refuse in the one message shape.
     */
    static JsonNode requireObject(JsonNode node, String what, String payload) {
        if (node == null || !node.isObject()) {
            throw new RelateException(what + " is not a JSON object: " + payload);
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
                    what + " has a non-long \"" + key + "\": " + payload);
        }
        return value.longValue();
    }
}
