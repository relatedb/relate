using System.Runtime.InteropServices;

namespace Relate.Net.Interop;

/// <summary><c>relate_status</c>.</summary>
internal enum RelateStatus
{
    Ok = 0,
    Error = 1,
}

/// <summary><c>enum relate_value_kind</c>.</summary>
internal enum NativeValueKind
{
    Null = 0,
    Bool = 1,
    Int = 2,
    Float = 3,
    String = 4,
    Node = 5,
    Relationship = 6,
    Path = 7,
    List = 8,
    Map = 9,
    Point = 10,
    Date = 11,
    Time = 12,
    LocalTime = 13,
    DateTime = 14,
    LocalDateTime = 15,
    Duration = 16,
    Vector = 17,
}

/// <summary>
/// Raw bindings to the relate C ABI v2 (<c>relate.h</c>). Opaque handles are <see cref="nint"/>
/// except for databases and connections, which are wrapped in <see cref="SafeHandle"/>s.
/// <c>char **error_json</c> and <c>char **bookmark_out</c> out-params are declared as pointers so
/// the caller can initialize them to <c>NULL</c>.
/// </summary>
internal static unsafe partial class NativeMethods
{
    internal const string LibraryName = "relate_ffi";

    // ---- library -------------------------------------------------------------------------

    [LibraryImport(LibraryName)]
    internal static partial int relate_abi_version();

    [LibraryImport(LibraryName)]
    internal static partial void relate_free_string(nint s);

    // ---- database / connection lifecycle ----------------------------------------------------

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_database_open(string uri, out DatabaseHandle db, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_database_open_read_only(string uri, out DatabaseHandle db, nint* errorJson);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_database_close(nint db);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_connect_namespace(DatabaseHandle db, string? name, out ConnectionHandle conn, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_conn_set_namespace(ConnectionHandle conn, string name, nint* errorJson);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_disconnect(nint conn);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_checkpoint(DatabaseHandle db, nint* errorJson);

    // ---- statements & transactions ----------------------------------------------------------

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_query_params(ConnectionHandle conn, string cypher, ParametersHandle parameters, out nint result, nint* errorJson);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_conn_begin(ConnectionHandle conn, nint* errorJson);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_conn_rollback(ConnectionHandle conn, nint* errorJson);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_conn_commit(ConnectionHandle conn, nint* bookmarkOut, nint* errorJson);

    // ---- results ------------------------------------------------------------------------------

    [LibraryImport(LibraryName)]
    internal static partial void relate_destroy_result(nint result);

    [LibraryImport(LibraryName)]
    internal static partial byte relate_result_is_error(nint result);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_result_error_json(nint result);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_result_bookmark(nint result);

    [LibraryImport(LibraryName)]
    internal static partial long relate_column_count(nint result);

    [LibraryImport(LibraryName)]
    internal static partial long relate_row_count(nint result);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_column_name(nint result, long col);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_result_query_type(nint result);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_result_namespace(nint result);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_result_stats(nint result);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_result_notifications(nint result);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_result_plan_json(nint result);

    [LibraryImport(LibraryName)]
    internal static partial double relate_result_elapsed_ms(nint result);

    [LibraryImport(LibraryName)]
    internal static partial double relate_result_parse_ms(nint result);

    [LibraryImport(LibraryName)]
    internal static partial double relate_result_plan_ms(nint result);

    [LibraryImport(LibraryName)]
    internal static partial double relate_result_execute_ms(nint result);

    // ---- value walker -------------------------------------------------------------------------

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_at(nint result, long row, long col);

    [LibraryImport(LibraryName)]
    internal static partial int relate_value_kind(nint v);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_bool(nint v, out byte value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_int(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_float(nint v, out double value);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_string(nint v, out nuint len);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_element_id(nint v);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_start_element_id(nint v);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_end_element_id(nint v);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_id(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_start_id(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_end_id(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial long relate_value_list_len(nint v);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_list_get(nint v, long i);

    [LibraryImport(LibraryName)]
    internal static partial long relate_value_map_len(nint v);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_map_key(nint v, long i, out nuint len);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_map_get(nint v, nint key);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_year(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_month(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_day(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_hour(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_minute(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_second(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_nanosecond(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_offset_seconds(nint v, out long value);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_zone(nint v);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_duration(nint v, out long months, out long days, out long seconds, out long nanos);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_point(nint v, out int srid, out int dim, out double x, out double y, out double z);

    [LibraryImport(LibraryName)]
    internal static partial nint relate_value_vector_dtype(nint v);

    [LibraryImport(LibraryName)]
    internal static partial long relate_value_vector_dim(nint v);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_vector_float(nint v, long i, out double value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_vector_int(nint v, long i, out long value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_is_nan(nint v, out byte value);

    [LibraryImport(LibraryName)]
    internal static partial RelateStatus relate_value_is_inf(nint v, out byte value);

    // ---- params builder ---------------------------------------------------------------------

    [LibraryImport(LibraryName)]
    internal static partial nint relate_params_new();

    [LibraryImport(LibraryName)]
    internal static partial void relate_params_free(nint p);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_null(nint p, string? name, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_bool(nint p, string? name, byte v, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_int(nint p, string? name, long v, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_float(nint p, string? name, double v, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_string(nint p, string? name, byte* v, nuint len, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_date(nint p, string? name, long year, long month, long day, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_time(nint p, string? name, long hour, long minute, long second, long nanosecond, long offsetSeconds, string? zone, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_local_time(nint p, string? name, long hour, long minute, long second, long nanosecond, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_datetime(nint p, string? name, long year, long month, long day, long hour, long minute, long second, long nanosecond, long offsetSeconds, string? zone, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_local_datetime(nint p, string? name, long year, long month, long day, long hour, long minute, long second, long nanosecond, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_duration(nint p, string? name, long months, long days, long seconds, long nanos, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_point(nint p, string? name, int srid, double x, double y, double* z, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_vector_float(nint p, string? name, string dtype, double* values, nuint dim, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_vector_int(nint p, string? name, string dtype, long* values, nuint dim, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_list(nint p, string? name, out nint child, nint* errorJson);

    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RelateStatus relate_params_put_map(nint p, string? name, out nint child, nint* errorJson);
}
