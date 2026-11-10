package com.relate;

import static org.junit.jupiter.api.Assertions.assertFalse;

/**
 * The shared sanitization assertions for the wire-refusal tests (Oplane
 * OPLANE_REQ-00351534, CWE-117): a refusal message must never carry a raw
 * control, separator, format or unassigned code point decoded from an
 * untrusted ABI value. The predicate is {@link Json#isUnprintable} itself,
 * so the assertion can never drift from the sanitizer's policy; the
 * message's own quoting characters are printable, so they stay allowed (the
 * dedicated quote/backslash test covers the value's own delimiters).
 */
final class WireSanitizeAssertions {

    /**
     * The hostile payload fixture the payload-echo tests share: CR, ESC and
     * a Unicode line separator ahead of 400 printable characters (over the
     * 256 bound), so it exercises both escaping and truncation.
     */
    static final String HOSTILE_PAYLOAD = "pay\rload\u001b\u2028" + "y".repeat(400);

    private WireSanitizeAssertions() {
    }

    /** Assert no code point the sanitizer escapes appears raw in {@code s}. */
    static void assertNoRawUnprintables(String s) {
        for (int i = 0; i < s.length(); ) {
            int codePoint = s.codePointAt(i);
            i += Character.charCount(codePoint);
            assertFalse(Json.isUnprintable(codePoint),
                    "raw non-printable U+" + Integer.toHexString(codePoint) + " in: " + s);
        }
    }
}
