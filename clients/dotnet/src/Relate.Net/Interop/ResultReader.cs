using static Relate.Net.Interop.NativeMethods;

namespace Relate.Net.Interop;

/// <summary>
/// Reads everything from a native result and destroys it: the column/row walk, the metadata
/// decode and the result's lifetime, assembled into a managed <see cref="QueryResult"/>. Throws
/// the statement's <see cref="RelateException"/> for an error result.
/// </summary>
internal static unsafe class ResultReader
{
    internal static QueryResult Read(nint result)
    {
        try
        {
            if (relate_result_is_error(result) != 0)
            {
                throw RelateException.FromErrorJson(Native.PtrToString(relate_result_error_json(result)), "query");
            }

            long columnCount = relate_column_count(result);
            var columns = new string[Native.Count(columnCount)];
            for (long c = 0; c < columns.Length; c++)
            {
                columns[c] = Native.RequiredString(relate_column_name(result, c));
            }

            var schema = new RecordSchema(columns);
            long rowCount = Native.Count(relate_row_count(result));
            var records = new RelateRecord[rowCount];
            for (long r = 0; r < rowCount; r++)
            {
                var values = new object?[columns.Length];
                for (long c = 0; c < columns.Length; c++)
                {
                    values[c] = ValueReader.Read(relate_value_at(result, r, c));
                }

                records[r] = new RelateRecord(schema, values);
            }

            string? bookmark = Native.PtrToString(relate_result_bookmark(result));
            return new QueryResult(
                schema.Columns,
                records,
                ResultMetadataReader.ReadQueryType(Native.PtrToString(relate_result_query_type(result))),
                Native.RequiredString(relate_result_namespace(result)),
                ResultMetadataReader.ReadCounters(Native.PtrToString(relate_result_stats(result))),
                ResultMetadataReader.ReadNotifications(Native.PtrToString(relate_result_notifications(result))),
                Native.PtrToString(relate_result_plan_json(result)),
                ResultMetadataReader.ReadBookmark(bookmark),
                ResultMetadataReader.ReadTimings(
                    relate_result_elapsed_ms(result),
                    relate_result_parse_ms(result),
                    relate_result_plan_ms(result),
                    relate_result_execute_ms(result)));
        }
        finally
        {
            relate_destroy_result(result);
        }
    }
}
