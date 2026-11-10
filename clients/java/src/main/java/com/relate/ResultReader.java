package com.relate;

import com.fasterxml.jackson.core.type.TypeReference;
import com.sun.jna.Pointer;

import java.util.ArrayList;
import java.util.List;

/**
 * The result handle's one reader (ADR-0502), the read tier the value
 * walker ({@link ValueWalker}) sits in: it reads one whole result off the
 * live handle into a {@link Database.Result} — the client's public value
 * type stays constructors and getters only.
 */
final class ResultReader {

    private ResultReader() {
    }

    /**
     * Read one whole result off the live handle: the error check first
     * (a failed statement crosses as an error result, ADR-0502 §2 —
     * the call itself returned OK), then counts, metadata strings,
     * column names, the bookmark and the cells — all inside the
     * handle's lifetime — and destroy the handle in the finally. The
     * one place that touches a result pointer.
     */
    static Database.Result read(Pointer result) {
        try {
            if (NativeLib.lib().relate_result_is_error(result) != 0) {
                // result-owned: no free, the destroy in the finally owns it
                throw RelateException.fromErrorJson(
                        NativeLib.lib().relate_result_error_json(result), "the statement");
            }
            int cols = Math.toIntExact(NativeLib.lib().relate_column_count(result));
            int rowsN = Math.toIntExact(NativeLib.lib().relate_row_count(result));
            // Result metadata (ADR-0285): result-owned strings; JNA
            // copies them during marshalling, and the copies are
            // decoded below while the handle is still alive.
            QueryType queryType = decodeQueryType(NativeLib.lib().relate_result_query_type(result));
            Namespace namespace = decodeNamespace(NativeLib.lib().relate_result_namespace(result));
            String bookmark = NativeLib.lib().relate_result_bookmark(result);
            // Notifications (ADR-0363): the C ABI always returns a JSON
            // array string, [] on a plain run.
            String notificationsJson = NativeLib.lib().relate_result_notifications(result);
            // The structured plan (LABS-418, ADR-0381): a relate.plan/v1
            // JSON document, or NULL on a plain run (JNA marshals NULL as null).
            String planJson = NativeLib.lib().relate_result_plan_json(result);
            List<String> columns = new ArrayList<>(cols);
            for (int i = 0; i < cols; i++) {
                columns.add(NativeLib.lib().relate_column_name(result, i));
            }
            return new Database.Result(columns, decodeRows(result, cols, rowsN),
                    queryType, namespace, bookmark, decodeNotifications(notificationsJson),
                    decodePlan(planJson));
        } finally {
            NativeLib.lib().relate_destroy_result(result);
        }
    }

    /**
     * Decode the rows while the result handle is alive: one typed value
     * walker pass per cell (ADR-0502 §1), Cypher null as Java null.
     */
    private static List<List<Object>> decodeRows(Pointer result, int cols, int rowsN) {
        List<List<Object>> rows = new ArrayList<>(rowsN);
        for (int r = 0; r < rowsN; r++) {
            List<Object> row = new ArrayList<>(cols);
            for (int c = 0; c < cols; c++) {
                row.add(ValueWalker.decode(NativeLib.lib().relate_value_at(result, r, c)));
            }
            rows.add(row);
        }
        return rows;
    }

    /**
     * Check the ABI's query_type string against its closed vocabulary; any
     * other value (or NULL) is an ABI contract violation and surfaces as
     * {@link RelateException}, never a lenient string.
     */
    static QueryType decodeQueryType(String raw) {
        return decodeWire(QueryType.class, "query type", raw);
    }

    /** As {@link #decodeQueryType}, for the resolved namespace. */
    static Namespace decodeNamespace(String raw) {
        return decodeWire(Namespace.class, "namespace", raw);
    }

    /** The one refusal policy for a closed-vocabulary ABI string. */
    private static <E extends Enum<E> & WireEnum> E decodeWire(Class<E> type, String what,
                                                               String raw) {
        E constant = WireEnum.fromWire(type, raw);
        if (constant == null) {
            // The untrusted ABI string is echoed through Json.quoted — a
            // hostile native library cannot inject control characters into
            // the message (CWE-117).
            throw new RelateException("ABI contract violated: " + what + " "
                    + Json.quoted(raw));
        }
        return constant;
    }

    /**
     * Decode the C ABI's structured plan document (LABS-418, ADR-0381).
     * The accessor returns NULL on a plain run (a legitimately absent
     * plan, not an error) and a JSON object otherwise; a malformed
     * payload is an ABI contract violation and surfaces as
     * {@link RelateException}.
     */
    private static PlanDocument decodePlan(String raw) {
        // The accessor returns NULL or the full document — never "".
        // PLAN is lenient about unknown keys — the engine keeps
        // adding plan fields — while notification entries stay strict
        // (Notification's creator validates every key it reads).
        if (raw == null) {
            return null;
        }
        try {
            return Json.PLAN.readValue(raw, PlanDocument.class);
        } catch (Exception e) {
            throw new RelateException("decoding result plan: " + e, e);
        }
    }

    /**
     * Decode the C ABI's engine-native notification array (ADR-0363). The
     * accessor returns a JSON array string, always present and {@code []}
     * on a plain run; a malformed value is an ABI contract violation and
     * surfaces as {@link RelateException} rather than a dropped entry.
     */
    private static List<Notification> decodeNotifications(String raw) {
        if (raw == null) {
            throw new RelateException("relate_result_notifications returned NULL");
        }
        // One decode path: the delegating creator on Notification is
        // the same one Jackson uses when a notification is nested in a
        // plan, so per-entry validation lives in exactly one place.
        try {
            return Json.STRICT.readValue(raw, new TypeReference<List<Notification>>() { });
        } catch (Exception e) {
            throw new RelateException("decoding result notifications: " + e, e);
        }
    }
}
