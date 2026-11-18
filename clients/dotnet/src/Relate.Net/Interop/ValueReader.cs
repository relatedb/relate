using System.Collections.ObjectModel;
using static Relate.Net.Interop.NativeMethods;

namespace Relate.Net.Interop;

/// <summary>Materializes relate value handles into .NET objects (see the type map in the user guide).</summary>
internal static class ValueReader
{
    private static readonly IReadOnlyDictionary<string, object?> s_emptyMap =
        new ReadOnlyDictionary<string, object?>(new Dictionary<string, object?>());

    internal static object? Read(nint v)
    {
        if (v == 0)
        {
            throw new RelateException("The native library returned a missing value handle.");
        }

        int kind = relate_value_kind(v);
        switch ((NativeValueKind)kind)
        {
            case NativeValueKind.Null:
                return null;
            case NativeValueKind.Bool:
                Ok(relate_value_bool(v, out byte b), "bool");
                return b != 0;
            case NativeValueKind.Int:
                Ok(relate_value_int(v, out long l), "int");
                return l;
            case NativeValueKind.Float:
                Ok(relate_value_float(v, out double d), "float");
                return d;
            case NativeValueKind.String:
                return ReadString(v);
            case NativeValueKind.Node:
                return ReadNode(v);
            case NativeValueKind.Relationship:
                return ReadRelationship(v);
            case NativeValueKind.Path:
                return ReadPath(v);
            case NativeValueKind.List:
                return ReadList(v);
            case NativeValueKind.Map:
                return ReadMap(v);
            case NativeValueKind.Point:
                Ok(relate_value_point(v, out int srid, out int dim, out double x, out double y, out double z), "point");
                if (dim is not (2 or 3))
                {
                    throw new RelateException($"The native library returned a point dimension {dim}; the ABI documents 2 or 3.");
                }

                return new CypherPoint(srid, x, y, dim == 3 ? z : null);
            case NativeValueKind.Date:
                return ToDate(v);
            case NativeValueKind.LocalTime:
                return ToTimeOnly(v);
            case NativeValueKind.Time:
                return new CypherOffsetTime(ToTimeOnly(v), TimeSpan.FromSeconds(Int(v, relate_value_offset_seconds, "offset")));
            case NativeValueKind.LocalDateTime:
                return ToLocalDateTime(v);
            case NativeValueKind.DateTime:
                return ToDateTime(v);
            case NativeValueKind.Duration:
                Ok(relate_value_duration(v, out long months, out long days, out long seconds, out long nanos), "duration");
                return new CypherDuration(months, days, seconds, nanos);
            case NativeValueKind.Vector:
                return ReadVector(v);
            default:
                return new UnsupportedValue(kind);
        }
    }

    private static string ReadString(nint v) => Native.PtrToString(relate_value_string(v, out nuint len), len);

    private static IReadOnlyList<object?> ReadList(nint v) => Array.AsReadOnly(ReadChildren(v, Read));

    /// <summary>Every child of a list value, expressed through the one index-addressed walk.</summary>
    private static T[] ReadChildren<T>(nint v, Func<nint, T> read) =>
        ReadItems(relate_value_list_len(v), i => read(relate_value_list_get(v, i)));

    private static IReadOnlyDictionary<string, object?> ReadMap(nint v)
    {
        // Native.Count throws on any negative length, so zero is the only empty case.
        int count = Native.Count(relate_value_map_len(v));
        if (count == 0)
        {
            return s_emptyMap;
        }

        var map = new Dictionary<string, object?>(count, StringComparer.Ordinal);
        for (long i = 0; i < count; i++)
        {
            nint key = relate_value_map_key(v, i, out nuint len);
            if (key == 0)
            {
                throw new RelateException("The native library returned a missing map key.");
            }

            // The key pointer is NUL-terminated, so it can be handed straight back to map_get.
            map[Native.PtrToString(key, len)] = Read(relate_value_map_get(v, key));
        }

        return new ReadOnlyDictionary<string, object?>(map);
    }

    private static Node ReadNode(nint v) => new(
        Native.RequiredString(relate_value_element_id(v)),
        Int(v, relate_value_id, "id"),
        Array.AsReadOnly(ReadChildren(v, ReadString)),
        ReadMap(v));

    private static Relationship ReadRelationship(nint v) => new(
        Native.RequiredString(relate_value_element_id(v)),
        Int(v, relate_value_id, "id"),
        ReadString(v),
        Native.RequiredString(relate_value_start_element_id(v)),
        Int(v, relate_value_start_id, "start id"),
        Native.RequiredString(relate_value_end_element_id(v)),
        Int(v, relate_value_end_id, "end id"),
        ReadMap(v));

    private static GraphPath ReadPath(nint v)
    {
        // The ABI documents PATH children as strictly alternating node, relationship, node —
        // read by index parity and fail loudly when an element is not what its position promises.
        object?[] elements = ReadChildren(v, Read);
        if (elements.Length == 0 || elements.Length % 2 == 0)
        {
            throw new RelateException($"A path alternates node, relationship, node; got {elements.Length} elements.");
        }

        var nodes = new Node[(elements.Length / 2) + 1];
        var rels = new Relationship[elements.Length / 2];
        for (int i = 0; i < elements.Length; i += 2)
        {
            nodes[i / 2] = elements[i] as Node
                ?? throw new RelateException($"The path element at index {i} is not a node.");
            if (i + 1 < elements.Length)
            {
                rels[i / 2] = elements[i + 1] as Relationship
                    ?? throw new RelateException($"The path element at index {i + 1} is not a relationship.");
            }
        }

        return new GraphPath(nodes, rels);
    }

    private static CypherVector ReadVector(nint v)
    {
        string? dtype = Native.PtrToString(relate_value_vector_dtype(v));
        VectorElementType type = CypherVector.FromDType(dtype)
            ?? throw new RelateException($"Unknown vector coordinate type '{dtype}'.");
        long dim = relate_value_vector_dim(v);
        // One index-addressed walk for both coordinate kinds; only the accessor differs.
        return CypherVector.IsIntegerType(type)
            ? CypherVector.FromNative(type, ReadItems(dim, i =>
            {
                Ok(relate_value_vector_int(v, i, out long x), "vector");
                return x;
            }))
            : CypherVector.FromNative(type, ReadItems(dim, i =>
            {
                Ok(relate_value_vector_float(v, i, out double x), "vector");
                return x;
            }));
    }

    /// <summary>The one index-addressed read walk: <paramref name="count"/> items read through <paramref name="read"/>.</summary>
    private static T[] ReadItems<T>(long count, Func<long, T> read)
    {
        var items = new T[Native.Count(count)];
        for (long i = 0; i < items.Length; i++)
        {
            items[i] = read(i);
        }

        return items;
    }

    private static DateOnly ToDate(nint v) => new(
        Int32In(v, relate_value_year, "year", 1, 9999),
        Int32In(v, relate_value_month, "month", 1, 12),
        Int32In(v, relate_value_day, "day", 1, 31));

    private static TimeOnly ToTimeOnly(nint v) => new(TimeTicks(v));

    private static long TimeTicks(nint v)
    {
        long h = Int(v, relate_value_hour, "hour");
        long mi = Int(v, relate_value_minute, "minute");
        long s = Int(v, relate_value_second, "second");
        long ns = Int(v, relate_value_nanosecond, "nanosecond");
        return CypherDuration.TicksFromSecondsAndNanos((h * 60 + mi) * 60 + s, ns);
    }

    private static DateTime ToLocalDateTime(nint v) =>
        ToDate(v).ToDateTime(TimeOnly.MinValue, DateTimeKind.Unspecified).AddTicks(TimeTicks(v));

    private static object ToDateTime(nint v)
    {
        DateTime wallClock = ToLocalDateTime(v);
        long offsetSeconds = Int(v, relate_value_offset_seconds, "offset");
        if (offsetSeconds % 60 != 0)
        {
            throw new OverflowException(
                $"The datetime offset of {offsetSeconds} seconds is not a whole number of minutes and cannot be represented by DateTimeOffset.");
        }

        var dto = new DateTimeOffset(wallClock, TimeSpan.FromSeconds(offsetSeconds));
        string? zone = Native.PtrToString(relate_value_zone(v));
        // Note: no ?: here - CypherZonedDateTime converts implicitly to DateTimeOffset and would lose its zone.
        if (string.IsNullOrEmpty(zone))
        {
            return dto;
        }

        return new CypherZonedDateTime(dto, zone);
    }

    /// <summary>Reads an ABI integer already proven in range, so no narrowing cast can truncate drift.</summary>
    private static int Int32In(nint v, IntAccessor accessor, string what, int min, int max)
    {
        long value = Int(v, accessor, what);
        return value >= min && value <= max
            ? (int)value
            : throw new OverflowException($"The {what} {value} is outside the supported range ({min}-{max}).");
    }

    private delegate RelateStatus IntAccessor(nint v, out long value);

    private static long Int(nint v, IntAccessor accessor, string what)
    {
        Ok(accessor(v, out long value), what);
        return value;
    }

    private static void Ok(RelateStatus status, string what)
    {
        if (status != RelateStatus.Ok)
        {
            throw new RelateException($"relate refused to read the {what} component of a value.");
        }
    }
}
