package ai.relatedb;

import com.sun.jna.Pointer;
import com.sun.jna.ptr.PointerByReference;

import java.nio.charset.StandardCharsets;
import java.time.LocalDate;
import java.time.LocalDateTime;
import java.time.LocalTime;
import java.time.OffsetDateTime;
import java.time.OffsetTime;
import java.time.ZoneOffset;
import java.time.ZonedDateTime;
import java.util.List;
import java.util.Map;

/**
 * The typed params builder (ADR-0502 §4) as a Java-map bridge: one
 * Java-type dispatch, one putter per kind, no JSON on the way. The root
 * (and every child below it) is freed by {@code relate_params_free}
 * on the root — the one owner — in the caller's {@code finally}.
 *
 * <p>A Java type the builder cannot bind refuses loudly with
 * {@link IllegalArgumentException}: never a silent null, never a stringly
 * coercion. A putter refusal (an out-of-range component, an unknown zone,
 * a coordinate the dtype cannot hold) crosses the ABI's own error channel
 * and throws the typed {@link RelateException}.
 */
final class Params {

    private Params() {
    }

    /**
     * Build the statement's parameter map as a params root. The caller
     * owns the root and frees it with {@code relate_params_free};
     * every child builder dies with it.
     */
    static Pointer build(Map<String, ?> params) {
        Pointer root = NativeLib.lib().relate_params_new();
        PointerByReference err = new PointerByReference();
        try {
            for (Map.Entry<String, ?> entry : params.entrySet()) {
                put(root, entry.getKey(), entry.getValue(), err);
            }
        } catch (RuntimeException e) {
            NativeLib.lib().relate_params_free(root);
            throw e;
        }
        return root;
    }

    /** Bind one named entry of a map container (the root or a nested map). */
    private static void put(Pointer builder, String name, Object value, PointerByReference err) {
        if (value == null) {
            NativeLib.check(NativeLib.lib().relate_params_put_null(builder, name, err),
                    "relate_params_put_null", err);
        } else if (value instanceof Boolean b) {
            NativeLib.check(NativeLib.lib().relate_params_put_bool(builder, name,
                    (byte) (b ? 1 : 0), err), "relate_params_put_bool", err);
        } else if (value instanceof Byte || value instanceof Short || value instanceof Integer
                || value instanceof Long) {
            NativeLib.check(NativeLib.lib().relate_params_put_int(builder, name,
                    ((Number) value).longValue(), err), "relate_params_put_int", err);
        } else if (value instanceof Float || value instanceof Double) {
            NativeLib.check(NativeLib.lib().relate_params_put_float(builder, name,
                    ((Number) value).doubleValue(), err), "relate_params_put_float", err);
        } else if (value instanceof String s) {
            byte[] bytes = s.getBytes(StandardCharsets.UTF_8);
            NativeLib.check(NativeLib.lib().relate_params_put_string(builder, name, bytes,
                    bytes.length, err), "relate_params_put_string", err);
        } else if (value instanceof LocalDate d) {
            NativeLib.check(NativeLib.lib().relate_params_put_date(builder, name,
                    d.getYear(), d.getMonthValue(), d.getDayOfMonth(), err),
                    "relate_params_put_date", err);
        } else if (value instanceof LocalTime t) {
            NativeLib.check(NativeLib.lib().relate_params_put_local_time(builder, name,
                    t.getHour(), t.getMinute(), t.getSecond(), t.getNano(), err),
                    "relate_params_put_local_time", err);
        } else if (value instanceof OffsetTime t) {
            // A TIME keeps no zone id: the offset is the whole zone (ADR-0502 §5)
            NativeLib.check(NativeLib.lib().relate_params_put_time(builder, name,
                    t.getHour(), t.getMinute(), t.getSecond(), t.getNano(),
                    t.getOffset().getTotalSeconds(), null, err),
                    "relate_params_put_time", err);
        } else if (value instanceof LocalDateTime dt) {
            NativeLib.check(NativeLib.lib().relate_params_put_local_datetime(builder, name,
                    dt.getYear(), dt.getMonthValue(), dt.getDayOfMonth(), dt.getHour(),
                    dt.getMinute(), dt.getSecond(), dt.getNano(), err),
                    "relate_params_put_local_datetime", err);
        } else if (value instanceof OffsetDateTime dt) {
            // Offset-only: wall clock + offset, no zone id
            NativeLib.check(NativeLib.lib().relate_params_put_datetime(builder, name,
                    dt.getYear(), dt.getMonthValue(), dt.getDayOfMonth(), dt.getHour(),
                    dt.getMinute(), dt.getSecond(), dt.getNano(),
                    dt.getOffset().getTotalSeconds(), null, err),
                    "relate_params_put_datetime", err);
        } else if (value instanceof ZonedDateTime dt) {
            putZonedDateTime(builder, name, dt, err);
        } else if (value instanceof DurationValue d) {
            NativeLib.check(NativeLib.lib().relate_params_put_duration(builder, name,
                    d.months(), d.days(), d.seconds(), d.nanos(), err),
                    "relate_params_put_duration", err);
        } else if (value instanceof java.time.Duration d) {
            // A java.time.Duration holds no months or days: both bind as zero
            NativeLib.check(NativeLib.lib().relate_params_put_duration(builder, name,
                    0, 0, d.getSeconds(), d.getNano(), err),
                    "relate_params_put_duration", err);
        } else if (value instanceof Point p) {
            double[] z = p.z() == null ? null : new double[] {p.z()};
            NativeLib.check(NativeLib.lib().relate_params_put_point(builder, name,
                    p.srid(), p.x(), p.y(), z, err), "relate_params_put_point", err);
        } else if (value instanceof VectorValue vec) {
            putVector(builder, name, vec, err);
        } else if (value instanceof List<?> list) {
            putList(builder, name, list, err);
        } else if (value instanceof Map<?, ?> map) {
            putMap(builder, name, map, err);
        } else {
            throw new IllegalArgumentException("cannot bind parameter \"" + name
                    + "\": unsupported Java type " + value.getClass().getName());
        }
    }

    /**
     * A zoned datetime keeps its IANA zone (the engine resolves the offset
     * at that wall clock); a datetime zoned only by an offset crosses as
     * the offset form, matching {@link OffsetDateTime} — one putter call,
     * the zone id the only difference.
     */
    private static void putZonedDateTime(Pointer builder, String name, ZonedDateTime dt,
                                         PointerByReference err) {
        String zone = dt.getZone() instanceof ZoneOffset ? null : dt.getZone().getId();
        NativeLib.check(NativeLib.lib().relate_params_put_datetime(builder, name,
                dt.getYear(), dt.getMonthValue(), dt.getDayOfMonth(), dt.getHour(),
                dt.getMinute(), dt.getSecond(), dt.getNano(),
                dt.getOffset().getTotalSeconds(), zone, err),
                "relate_params_put_datetime", err);
    }

    private static void putVector(Pointer builder, String name, VectorValue vec,
                                  PointerByReference err) {
        String dtype = vec.dtype();
        VectorDtype known = VectorDtype.fromName(dtype);
        if (known == null) {
            throw new IllegalArgumentException("cannot bind parameter \"" + name
                    + "\": unsupported vector dtype " + dtype);
        }
        List<Object> coordinates = vec.coordinates();
        if (known.family() == VectorDtype.Family.INT) {
            long[] values = new long[coordinates.size()];
            for (int i = 0; i < values.length; i++) {
                Object coordinate = coordinates.get(i);
                if (!(coordinate instanceof Long n)) {
                    throw vectorCoordinateType(known, i, coordinate);
                }
                values[i] = n;
            }
            NativeLib.check(NativeLib.lib().relate_params_put_vector_int(builder, name,
                    dtype, values, values.length, err), "relate_params_put_vector_int", err);
        } else {
            double[] values = new double[coordinates.size()];
            for (int i = 0; i < values.length; i++) {
                Object coordinate = coordinates.get(i);
                if (!(coordinate instanceof Double n)) {
                    throw vectorCoordinateType(known, i, coordinate);
                }
                values[i] = n;
            }
            NativeLib.check(NativeLib.lib().relate_params_put_vector_float(builder, name,
                    dtype, values, values.length, err), "relate_params_put_vector_float", err);
        }
    }

    private static IllegalArgumentException vectorCoordinateType(VectorDtype dtype, int i,
                                                                 Object coordinate) {
        return new IllegalArgumentException("cannot bind vector parameter with dtype "
                + dtype.name()
                + ": coordinate " + i + " is " + describe(coordinate)
                + ", expected " + dtype.boxType().getSimpleName());
    }

    /** A refused value's type for a refusal message; null-safe, so the
     *  refusal itself never becomes a NullPointerException. */
    private static String describe(Object value) {
        return value == null ? "null" : value.getClass().getName();
    }

    /** A list child: entries append in call order, so every name is NULL. */
    private static void putList(Pointer builder, String name, List<?> list,
                                PointerByReference err) {
        PointerByReference child = new PointerByReference();
        NativeLib.check(NativeLib.lib().relate_params_put_list(builder, name, child, err),
                "relate_params_put_list", err);
        for (Object item : list) {
            put(child.getValue(), null, item, err);
        }
    }

    /** A map child: entries take their own names. */
    private static void putMap(Pointer builder, String name, Map<?, ?> map,
                               PointerByReference err) {
        PointerByReference child = new PointerByReference();
        NativeLib.check(NativeLib.lib().relate_params_put_map(builder, name, child, err),
                "relate_params_put_map", err);
        for (Map.Entry<?, ?> entry : map.entrySet()) {
            if (!(entry.getKey() instanceof String key)) {
                throw new IllegalArgumentException("cannot bind nested map parameter \""
                        + name + "\": key " + entry.getKey() + " is "
                        + describe(entry.getKey()) + ", expected String");
            }
            put(child.getValue(), key, entry.getValue(), err);
        }
    }
}
