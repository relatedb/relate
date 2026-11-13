package ai.relatedb;

import com.sun.jna.Library;
import com.sun.jna.Pointer;
import com.sun.jna.ptr.ByteByReference;
import com.sun.jna.ptr.DoubleByReference;
import com.sun.jna.ptr.IntByReference;
import com.sun.jna.ptr.LongByReference;
import com.sun.jna.ptr.PointerByReference;

/**
 * The one JNA binding of the typed C ABI v2 (ADR-0502) and the one error
 * channel helper every call site shares. Every handle (database,
 * connection, result, value, params builder) is an opaque {@link Pointer};
 * {@code char **} out-params are {@link PointerByReference}, read and freed
 * by {@link #takeError(String, PointerByReference)} on failure.
 *
 * <p>Loaded lazily, not in a static initializer, so a loading failure
 * reaches the caller unwrapped on every call instead of hiding behind
 * {@code ExceptionInInitializerError} / {@code NoClassDefFoundError} after
 * the first. The ABI version is checked at load: a library that is not v2
 * fails immediately with a clear incompatible-library error, not later at
 * a missing walker symbol.
 */
final class NativeLib {

    /** The ABI major version this client speaks (ADR-0502 §8). */
    private static final int ABI_VERSION = 2;

    /**
     * The C ABI, as JNA sees it. Caller-owned {@code char *} values handed
     * through an out-param ({@code relate_conn_commit}'s
     * {@code bookmark_out}) arrive as a {@link Pointer} inside the
     * {@link PointerByReference} — the caller copies then frees them with
     * {@code relate_free_string}; a {@link String} mapping would make
     * JNA copy the string and leak the native one. Result-owned
     * {@code const char *} returns (column
     * names, error json, bookmarks, element ids, zones, dtypes) may be
     * {@link String}: the result owns them and frees them at
     * {@code relate_destroy_result}. Strings that can carry an
     * interior NUL ({@code relate_value_string}, {@code _map_key})
     * are {@link Pointer} plus a length out-param.
     */
    interface CLib extends Library {
        int relate_abi_version();
        void relate_free_string(Pointer s);

        int relate_database_open(String uri, PointerByReference out, PointerByReference errorJson);
        int relate_database_open_read_only(String uri, PointerByReference out, PointerByReference errorJson);
        int relate_database_close(Pointer db);
        int relate_connect_namespace(Pointer db, String name, PointerByReference out, PointerByReference errorJson);
        int relate_conn_set_namespace(Pointer conn, String name, PointerByReference errorJson);
        int relate_disconnect(Pointer conn);
        int relate_query_params(Pointer conn, String cypher, Pointer params,
                PointerByReference out, PointerByReference errorJson);
        int relate_conn_begin(Pointer conn, PointerByReference errorJson);
        int relate_conn_rollback(Pointer conn, PointerByReference errorJson);
        int relate_checkpoint(Pointer db, PointerByReference errorJson);
        int relate_conn_commit(Pointer conn, PointerByReference bookmarkOut,
                PointerByReference errorJson);

        byte relate_result_is_error(Pointer result);
        String relate_result_error_json(Pointer result);
        String relate_result_bookmark(Pointer result);
        long relate_column_count(Pointer result);
        long relate_row_count(Pointer result);
        String relate_column_name(Pointer result, long col);
        String relate_result_query_type(Pointer result);
        String relate_result_namespace(Pointer result);
        String relate_result_notifications(Pointer result);
        String relate_result_plan_json(Pointer result);
        void relate_destroy_result(Pointer result);

        Pointer relate_value_at(Pointer result, long row, long col);
        int relate_value_kind(Pointer v);
        int relate_value_bool(Pointer v, ByteByReference out);
        int relate_value_int(Pointer v, LongByReference out);
        int relate_value_float(Pointer v, DoubleByReference out);
        Pointer relate_value_string(Pointer v, LongByReference len);
        String relate_value_element_id(Pointer v);
        String relate_value_start_element_id(Pointer v);
        String relate_value_end_element_id(Pointer v);
        int relate_value_id(Pointer v, LongByReference out);
        int relate_value_start_id(Pointer v, LongByReference out);
        int relate_value_end_id(Pointer v, LongByReference out);
        long relate_value_list_len(Pointer v);
        Pointer relate_value_list_get(Pointer v, long i);
        long relate_value_map_len(Pointer v);
        Pointer relate_value_map_key(Pointer v, long i, LongByReference len);
        Pointer relate_value_map_get(Pointer v, Pointer key);
        int relate_value_year(Pointer v, LongByReference out);
        int relate_value_month(Pointer v, LongByReference out);
        int relate_value_day(Pointer v, LongByReference out);
        int relate_value_hour(Pointer v, LongByReference out);
        int relate_value_minute(Pointer v, LongByReference out);
        int relate_value_second(Pointer v, LongByReference out);
        int relate_value_nanosecond(Pointer v, LongByReference out);
        int relate_value_offset_seconds(Pointer v, LongByReference out);
        String relate_value_zone(Pointer v);
        int relate_value_duration(Pointer v, LongByReference months, LongByReference days,
                LongByReference seconds, LongByReference nanos);
        int relate_value_point(Pointer v, IntByReference srid, IntByReference dim,
                DoubleByReference x, DoubleByReference y, DoubleByReference z);
        String relate_value_vector_dtype(Pointer v);
        long relate_value_vector_dim(Pointer v);
        int relate_value_vector_float(Pointer v, long i, DoubleByReference out);
        int relate_value_vector_int(Pointer v, long i, LongByReference out);

        Pointer relate_params_new();
        void relate_params_free(Pointer p);
        int relate_params_put_null(Pointer p, String name, PointerByReference errorJson);
        int relate_params_put_bool(Pointer p, String name, byte v, PointerByReference errorJson);
        int relate_params_put_int(Pointer p, String name, long v, PointerByReference errorJson);
        int relate_params_put_float(Pointer p, String name, double v, PointerByReference errorJson);
        int relate_params_put_string(Pointer p, String name, byte[] v, long len, PointerByReference errorJson);
        int relate_params_put_date(Pointer p, String name, long year, long month, long day,
                PointerByReference errorJson);
        int relate_params_put_time(Pointer p, String name, long hour, long minute, long second,
                long nanosecond, long offsetSeconds, String zone, PointerByReference errorJson);
        int relate_params_put_local_time(Pointer p, String name, long hour, long minute, long second,
                long nanosecond, PointerByReference errorJson);
        int relate_params_put_datetime(Pointer p, String name, long year, long month, long day,
                long hour, long minute, long second, long nanosecond, long offsetSeconds, String zone,
                PointerByReference errorJson);
        int relate_params_put_local_datetime(Pointer p, String name, long year, long month, long day,
                long hour, long minute, long second, long nanosecond, PointerByReference errorJson);
        int relate_params_put_duration(Pointer p, String name, long months, long days, long seconds,
                long nanos, PointerByReference errorJson);
        int relate_params_put_point(Pointer p, String name, int srid, double x, double y,
                double[] z, PointerByReference errorJson);
        int relate_params_put_vector_float(Pointer p, String name, String dtype, double[] values,
                long dim, PointerByReference errorJson);
        int relate_params_put_vector_int(Pointer p, String name, String dtype, long[] values,
                long dim, PointerByReference errorJson);
        int relate_params_put_list(Pointer p, String name, PointerByReference child,
                PointerByReference errorJson);
        int relate_params_put_map(Pointer p, String name, PointerByReference child,
                PointerByReference errorJson);
    }

    private static CLib lib;

    static synchronized CLib lib() {
        if (lib == null) {
            String path = NativeLoader.libraryPath();
            CLib loaded;
            try {
                loaded = com.sun.jna.Native.load(path, CLib.class);
            } catch (UnsatisfiedLinkError e) {
                // A v1 library resolves no v2 symbol (it carries no
                // relate_abi_version at all); JNA reports the first
                // missing symbol. Name the likely cause.
                throw new RelateException("incompatible librelate_ffi at " + path
                        + " (this client needs the typed ABI v2): " + e.getMessage(), e);
            }
            int version = loaded.relate_abi_version();
            if (version != ABI_VERSION) {
                throw new RelateException("incompatible librelate_ffi at " + path
                        + ": ABI version " + version + ", this client needs " + ABI_VERSION);
            }
            lib = loaded;
        }
        return lib;
    }

    /** Throw the typed exception for a non-OK status; no-op on success. */
    static void check(int status, String what, PointerByReference errorJson) {
        if (status != 0) {
            throw takeError(what, errorJson);
        }
    }

    /**
     * Copy, free and classify a caller-owned {@code error_json} out-param
     * into the typed exception (code + GQLSTATUS drive the class,
     * ADR-0502 §3). The out-param starts NULL (JNA zero-initialises it)
     * and is written only on failure, so a non-null value here is always
     * this call's error.
     */
    static RelateException takeError(String what, PointerByReference errorJson) {
        Pointer p = errorJson.getValue();
        if (p == null) {
            return new RelateException(what + " failed with no error detail");
        }
        String json = p.getString(0, "UTF-8");
        lib().relate_free_string(p);
        return RelateException.fromErrorJson(json, what);
    }

    private NativeLib() {
    }
}
