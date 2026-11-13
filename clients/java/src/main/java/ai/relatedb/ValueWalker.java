package ai.relatedb;

import com.sun.jna.Pointer;
import com.sun.jna.ptr.ByteByReference;
import com.sun.jna.ptr.DoubleByReference;
import com.sun.jna.ptr.IntByReference;
import com.sun.jna.ptr.LongByReference;

import java.nio.charset.StandardCharsets;
import java.time.LocalDate;
import java.time.LocalDateTime;
import java.time.LocalTime;
import java.time.OffsetDateTime;
import java.time.OffsetTime;
import java.time.ZoneId;
import java.time.ZoneOffset;
import java.time.ZonedDateTime;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The typed value walker (ADR-0502 §1): one switch over the value's kind
 * that builds the Java value, recursion for the containers. There is no
 * JSON and no re-typing here; the kind discriminates structurally at
 * every depth.
 *
 * <p>Every handle is owned by the result: this class only reads, copies
 * into Java values and drops the handles — nothing it returns outlives
 * {@code relate_destroy_result}. An in-kind accessor refusing is an
 * ABI contract violation and throws {@link RelateException}; a kind
 * this client does not know (a later, additive ABI) is a defined,
 * reportable case, never a silent null.
 */
final class ValueWalker {

    /** {@code enum relate_value_kind} (the v2 header) — 0 to 17, in order. */
    private static final int K_NULL = 0, K_BOOL = 1, K_INT = 2, K_FLOAT = 3, K_STRING = 4,
            K_NODE = 5, K_RELATIONSHIP = 6, K_PATH = 7, K_LIST = 8, K_MAP = 9, K_POINT = 10,
            K_DATE = 11, K_TIME = 12, K_LOCAL_TIME = 13, K_DATETIME = 14,
            K_LOCAL_DATETIME = 15, K_DURATION = 16, K_VECTOR = 17;

    private ValueWalker() {
    }

    /** Decode one value handle into its Java value; Cypher {@code null} is Java {@code null}. */
    static Object decode(Pointer v) {
        int kind = NativeLib.lib().relate_value_kind(v);
        switch (kind) {
            case -1:
                throw new RelateException("ABI contract violated: the walker returned"
                        + " a NULL value handle");
            case K_NULL:
                return null;
            case K_BOOL:
                return readBool(v);
            case K_INT:
                return readInt(v);
            case K_FLOAT:
                return readFloat(v); // NaN and +-Inf arrive as themselves
            case K_STRING:
                return readString(v);
            case K_NODE:
                return new Node(readElementId(v), readId(v), readStringList(v), readMap(v));
            case K_RELATIONSHIP:
                return new Relationship(readElementId(v), readId(v),
                        requiredId(NativeLib.lib().relate_value_start_element_id(v), "start_element_id"),
                        readStartId(v),
                        requiredId(NativeLib.lib().relate_value_end_element_id(v), "end_element_id"),
                        readEndId(v),
                        readString(v), readMap(v));
            case K_PATH:
                return new Path(readList(v)); // node, rel, ..., node (2n+1 for n hops)
            case K_LIST:
                return readList(v);
            case K_MAP:
                return readMap(v);
            case K_POINT:
                return readPoint(v);
            case K_DATE:
                return LocalDate.of(year(v), month(v),
                        day(v));
            case K_TIME:
                return OffsetTime.of(hour(v), minute(v),
                        second(v), (int) nanosecond(v),
                        ZoneOffset.ofTotalSeconds(offsetSeconds(v)));
            case K_LOCAL_TIME:
                return LocalTime.of(hour(v), minute(v),
                        second(v), (int) nanosecond(v));
            case K_DATETIME:
                return readZonedDateTime(v);
            case K_LOCAL_DATETIME:
                return LocalDateTime.of(year(v), month(v),
                        day(v), hour(v),
                        minute(v), second(v),
                        (int) nanosecond(v));
            case K_DURATION:
                return readDuration(v);
            case K_VECTOR:
                return readVector(v);
            default:
                // A later, additive ABI: defined and reportable, never a silent null.
                throw new RelateException("unsupported value kind " + kind);
        }
    }

    private static boolean readBool(Pointer v) {
        ByteByReference out = new ByteByReference();
        checkStatus(NativeLib.lib().relate_value_bool(v, out), "value_bool");
        return out.getValue() != 0;
    }

    private static long readInt(Pointer v) {
        LongByReference out = new LongByReference();
        checkStatus(NativeLib.lib().relate_value_int(v, out), "value_int");
        return out.getValue(); // a Java long holds every int64 exactly
    }

    private static double readFloat(Pointer v) {
        DoubleByReference out = new DoubleByReference();
        checkStatus(NativeLib.lib().relate_value_float(v, out), "value_float");
        return out.getValue();
    }

    /**
     * A STRING payload, or a RELATIONSHIP's type: the pointer + length
     * pair, so a string carrying an interior NUL crosses whole.
     */
    private static String readString(Pointer v) {
        LongByReference len = new LongByReference();
        Pointer p = NativeLib.lib().relate_value_string(v, len);
        if (p == null) {
            throw refused("value_string");
        }
        return utf8(p, len.getValue());
    }

    private static String readElementId(Pointer v) {
        return requiredId(NativeLib.lib().relate_value_element_id(v), "value_element_id");
    }

    /**
     * An entity's legacy numeric id (ADR-0506): the INTEGER Cypher
     * {@code id()} returns for the same entity. An entity always carries
     * one, so a refusal here is a contract violation and throws.
     */
    private static long readId(Pointer v) {
        return readInt64(v, NativeLib.lib()::relate_value_id, "value_id");
    }

    /** A RELATIONSHIP's start node's numeric id, as {@code id(startNode(r))} returns it. */
    private static long readStartId(Pointer v) {
        return readInt64(v, NativeLib.lib()::relate_value_start_id, "value_start_id");
    }

    /** A RELATIONSHIP's end node's numeric id, as {@code id(endNode(r))} returns it. */
    private static long readEndId(Pointer v) {
        return readInt64(v, NativeLib.lib()::relate_value_end_id, "value_end_id");
    }

    private static String requiredId(String id, String what) {
        if (id == null) {
            throw refused(what);
        }
        return id;
    }

    /** A LIST's items (a PATH's alternating elements share the accessor). */
    private static List<Object> readList(Pointer v) {
        long n = NativeLib.lib().relate_value_list_len(v);
        List<Object> items = new ArrayList<>(Math.toIntExact(n));
        for (long i = 0; i < n; i++) {
            items.add(decode(NativeLib.lib().relate_value_list_get(v, i)));
        }
        return items;
    }

    /** A NODE's labels: its list, each child a STRING. */
    private static List<String> readStringList(Pointer v) {
        long n = NativeLib.lib().relate_value_list_len(v);
        List<String> labels = new ArrayList<>(Math.toIntExact(n));
        for (long i = 0; i < n; i++) {
            labels.add(readString(NativeLib.lib().relate_value_list_get(v, i)));
        }
        return labels;
    }

    /**
     * A MAP's entries (a NODE's / RELATIONSHIP's properties), in engine
     * order. The lookup reuses the engine's own key pointer — the exact
     * bytes {@code map_key} returned, NUL-terminated like the accessor
     * promises — so a key without an interior NUL resolves exactly. A key
     * holding one cannot cross {@code map_get} (the C side reads the key
     * as a NUL-terminated string and would truncate there — possibly onto
     * another key's value) and throws instead. String values keep their
     * interior NULs; map keys are the one place the ABI cannot carry one.
     */
    private static Map<String, Object> readMap(Pointer v) {
        long n = NativeLib.lib().relate_value_map_len(v);
        Map<String, Object> entries = new LinkedHashMap<>();
        for (long i = 0; i < n; i++) {
            LongByReference len = new LongByReference();
            Pointer key = NativeLib.lib().relate_value_map_key(v, i, len);
            if (key == null) {
                throw refused("value_map_key");
            }
            String name = utf8(key, len.getValue());
            // map_get reads its key as a NUL-terminated string, so the
            // lookup of this key would stop at the interior NUL — and may
            // land on a different key entirely. Refuse the read (the Go
            // client's contract) instead of returning the wrong value.
            if (name.indexOf('\u0000') >= 0) {
                throw new RelateException("map key \"" + name + "\" holds an"
                        + " interior NUL and cannot be read through the walker");
            }
            Pointer child = NativeLib.lib().relate_value_map_get(v, key);
            if (child == null) {
                throw new RelateException("ABI contract violated: value_map_get refused"
                        + " the present key \"" + name + "\"");
            }
            entries.put(name, decode(child));
        }
        return entries;
    }

    private static Point readPoint(Pointer v) {
        IntByReference srid = new IntByReference();
        IntByReference dim = new IntByReference();
        DoubleByReference x = new DoubleByReference();
        DoubleByReference y = new DoubleByReference();
        DoubleByReference z = new DoubleByReference();
        checkStatus(NativeLib.lib().relate_value_point(v, srid, dim, x, y, z), "value_point");
        // z is written only when the dimension is 3 (the header's contract)
        return new Point(srid.getValue(), x.getValue(), y.getValue(),
                dim.getValue() == 3 ? z.getValue() : null);
    }

    /**
     * A DATETIME: its zone id when the value keeps one, its UTC offset
     * otherwise. ZonedDateTime applies the zone's rules at that wall clock
     * — the same resolution the engine used when it read the offset.
     */
    private static Object readZonedDateTime(Pointer v) {
        String zone = NativeLib.lib().relate_value_zone(v);
        if (zone == null) {
            throw refused("value_zone");
        }
        if (zone.isEmpty()) {
            return OffsetDateTime.of(year(v), month(v),
                    day(v), hour(v),
                    minute(v), second(v),
                    (int) nanosecond(v),
                    ZoneOffset.ofTotalSeconds(offsetSeconds(v)));
        }
        return ZonedDateTime.of(year(v), month(v),
                day(v), hour(v),
                minute(v), second(v),
                (int) nanosecond(v), ZoneId.of(zone));
    }

    private static DurationValue readDuration(Pointer v) {
        LongByReference months = new LongByReference();
        LongByReference days = new LongByReference();
        LongByReference seconds = new LongByReference();
        LongByReference nanos = new LongByReference();
        checkStatus(NativeLib.lib().relate_value_duration(v, months, days, seconds, nanos),
                "value_duration");
        return new DurationValue(months.getValue(), days.getValue(), seconds.getValue(),
                nanos.getValue());
    }

    private static VectorValue readVector(Pointer v) {
        String dtype = NativeLib.lib().relate_value_vector_dtype(v);
        if (dtype == null) {
            throw refused("value_vector_dtype");
        }
        VectorDtype known = VectorDtype.fromName(dtype);
        if (known == null) {
            throw new RelateException("unsupported vector dtype " + dtype);
        }
        long dim = NativeLib.lib().relate_value_vector_dim(v);
        List<Object> coordinates = new ArrayList<>(Math.toIntExact(dim));
        if (known.family() == VectorDtype.Family.INT) {
            LongByReference out = new LongByReference();
            for (long i = 0; i < dim; i++) {
                checkStatus(NativeLib.lib().relate_value_vector_int(v, i, out), "value_vector_int");
                coordinates.add(out.getValue());
            }
        } else {
            DoubleByReference out = new DoubleByReference();
            for (long i = 0; i < dim; i++) {
                checkStatus(NativeLib.lib().relate_value_vector_float(v, i, out), "value_vector_float");
                coordinates.add(out.getValue());
            }
        }
        return new VectorValue(dtype, coordinates);
    }

    /** One int64 out-param accessor of the walker ({@code _year}, {@code _id}, ...). */
    private interface Int64 {
        int read(Pointer v, LongByReference out);
    }

    /** One int64 read off the walker (a temporal component, an entity id). */
    private static long readInt64(Pointer v, Int64 read, String what) {
        LongByReference out = new LongByReference();
        checkStatus(read.read(v, out), what);
        return out.getValue();
    }

    /** One temporal component (year, hour, ...): an int64 read off the walker. */
    private static int readComponent(Pointer v, Int64 component, String what) {
        return Math.toIntExact(readInt64(v, component, what));
    }

    private static int year(Pointer v) {
        return readComponent(v, NativeLib.lib()::relate_value_year, "value_year");
    }

    private static int month(Pointer v) {
        return readComponent(v, NativeLib.lib()::relate_value_month, "value_month");
    }

    private static int day(Pointer v) {
        return readComponent(v, NativeLib.lib()::relate_value_day, "value_day");
    }

    private static int hour(Pointer v) {
        return readComponent(v, NativeLib.lib()::relate_value_hour, "value_hour");
    }

    private static int minute(Pointer v) {
        return readComponent(v, NativeLib.lib()::relate_value_minute, "value_minute");
    }

    private static int second(Pointer v) {
        return readComponent(v, NativeLib.lib()::relate_value_second, "value_second");
    }

    private static int nanosecond(Pointer v) {
        return readComponent(v, NativeLib.lib()::relate_value_nanosecond, "value_nanosecond");
    }

    private static int offsetSeconds(Pointer v) {
        return readComponent(v, NativeLib.lib()::relate_value_offset_seconds, "value_offset_seconds");
    }

    /** An in-kind scalar read refused: the status is the whole report, so surface it loudly. */
    private static void checkStatus(int status, String what) {
        if (status != 0) {
            throw refused(what);
        }
    }

    private static RelateException refused(String what) {
        return new RelateException("ABI contract violated: " + what
                + " refused an in-kind read");
    }

    /** Copy {@code len} bytes at {@code p} as UTF-8 — the interior NUL survives. */
    private static String utf8(Pointer p, long len) {
        return new String(p.getByteArray(0, Math.toIntExact(len)), StandardCharsets.UTF_8);
    }
}
