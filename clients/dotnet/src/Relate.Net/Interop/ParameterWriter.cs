using System.Collections;
using System.Collections.Concurrent;
using System.Reflection;
using System.Text;
using static Relate.Net.Interop.NativeMethods;

namespace Relate.Net.Interop;

/// <summary>Translates .NET parameter objects into a native relate params builder tree.</summary>
internal static unsafe class ParameterWriter
{
    private const int MaxDepth = 64;

    private static readonly ConcurrentDictionary<Type, PropertyInfo[]> s_properties = new();
    private static readonly ConcurrentDictionary<Type, (PropertyInfo Key, PropertyInfo Value)?> s_keyValueTypes = new();

    /// <summary>Builds a params root from a parameter object, or a null native handle for <see langword="null"/>.</summary>
    internal static ParametersHandle Build(object? parameters)
    {
        if (parameters is null)
        {
            return new ParametersHandle(0);
        }

        switch (Classify(parameters))
        {
            case ParameterShape.Entries entries:
                return BuildRoot(entries.Values);
            case ParameterShape.Entity:
                throw new ArgumentException(
                    "Parameters cannot be graph entities; pass an ElementId and match on elementId() instead.",
                    nameof(parameters));
            case ParameterShape.Items:
                throw new ArgumentException(
                    "Parameters must be named; a list cannot be the parameter root. Pass a dictionary or an object with public properties.",
                    nameof(parameters));
            default:
                throw new ArgumentException(
                    $"Parameters of type {parameters.GetType()} cannot be used; pass a dictionary or an object with public properties.",
                    nameof(parameters));
        }
    }

    private static ParametersHandle BuildRoot(IEnumerable<KeyValuePair<string, object?>> entries)
    {
        nint root = relate_params_new();
        if (root == 0)
        {
            throw new OutOfMemoryException("relate could not allocate a parameter builder.");
        }

        try
        {
            foreach (KeyValuePair<string, object?> entry in entries)
            {
                WriteValue(root, entry.Key, entry.Value, entry.Key, 0);
            }

            return new ParametersHandle(root);
        }
        catch
        {
            relate_params_free(root);
            throw;
        }
    }

    private static void WriteValue(nint builder, string? name, object? value, string path, int depth)
    {
        if (depth > MaxDepth)
        {
            throw new ArgumentException($"Parameter '{path}' is nested deeper than {MaxDepth} levels (is the object graph cyclic?).");
        }

        if (name is not null)
        {
            Native.ValidateCString(name, nameof(name));
        }
        nint err = 0;
        RelateStatus status;
        switch (value)
        {
            case null:
            case DBNull:
                status = relate_params_put_null(builder, name, &err);
                break;
            case bool b:
                status = relate_params_put_bool(builder, name, b ? (byte)1 : (byte)0, &err);
                break;
            case string s:
                status = PutString(builder, name, s, &err);
                break;
            case char c:
                status = PutString(builder, name, c.ToString(), &err);
                break;
            case sbyte or byte or short or ushort or int or uint or long:
                status = relate_params_put_int(builder, name, Convert.ToInt64(value, null), &err);
                break;
            case ulong ul:
                if (ul > long.MaxValue)
                {
                    throw new ArgumentOutOfRangeException(path, ul, "Cypher integers are 64-bit signed.");
                }

                status = relate_params_put_int(builder, name, (long)ul, &err);
                break;
            case float f:
                status = relate_params_put_float(builder, name, f, &err);
                break;
            case double d:
                status = relate_params_put_float(builder, name, d, &err);
                break;
            case decimal m:
                status = relate_params_put_float(builder, name, (double)m, &err);
                break;
            case Guid g:
                status = PutString(builder, name, g.ToString("D"), &err);
                break;
            case Enum e:
                status = PutString(builder, name, e.ToString(), &err);
                break;
            case DateOnly date:
                status = relate_params_put_date(builder, name, date.Year, date.Month, date.Day, &err);
                break;
            case TimeOnly time:
                status = relate_params_put_local_time(builder, name, time.Hour, time.Minute, time.Second, CypherDuration.SubSecondNanos(time.Ticks), &err);
                break;
            case DateTime dt when dt.Kind == DateTimeKind.Utc:
                status = PutDateTime(builder, name, dt, 0, null, &err);
                break;
            case DateTime dt:
                status = relate_params_put_local_datetime(
                    builder, name, dt.Year, dt.Month, dt.Day, dt.Hour, dt.Minute, dt.Second, CypherDuration.SubSecondNanos(dt.Ticks), &err);
                break;
            case DateTimeOffset dto:
                status = PutDateTime(builder, name, dto.DateTime, (long)dto.Offset.TotalSeconds, null, &err);
                break;
            case CypherZonedDateTime zoned:
                status = PutDateTime(builder, name, zoned.DateTimeOffset.DateTime, (long)zoned.DateTimeOffset.Offset.TotalSeconds, zoned.ZoneId, &err);
                break;
            case CypherOffsetTime ot:
                status = relate_params_put_time(
                    builder, name, ot.Time.Hour, ot.Time.Minute, ot.Time.Second, CypherDuration.SubSecondNanos(ot.Time.Ticks), (long)ot.Offset.TotalSeconds, null, &err);
                break;
            case TimeSpan ts:
                CypherDuration fromSpan = CypherDuration.FromTimeSpan(ts);
                status = relate_params_put_duration(builder, name, 0, 0, fromSpan.Seconds, fromSpan.Nanoseconds, &err);
                break;
            case CypherDuration dur:
                status = relate_params_put_duration(builder, name, dur.Months, dur.Days, dur.Seconds, dur.Nanoseconds, &err);
                break;
            case CypherPoint p:
                double z = p.Z.GetValueOrDefault();
                status = relate_params_put_point(builder, name, p.Srid, p.X, p.Y, p.Z.HasValue ? &z : null, &err);
                break;
            case CypherVector vec:
                status = PutVector(builder, name, vec, &err);
                break;
            default:
                // The shape is classified once, here and in Build, by the one Classify helper —
                // including the graph-entity rejection.
                status = Classify(value) switch
                {
                    ParameterShape.Entries entries => WriteMap(builder, name, entries.Values, path, depth, &err),
                    ParameterShape.Items items => WriteList(builder, name, items.Values, path, depth, &err),
                    ParameterShape.Entity => throw new ArgumentException(
                        $"Parameter '{path}' is a graph entity; entities cannot be sent as parameters. Pass its ElementId and match on elementId() instead."),
                    _ => throw new NotSupportedException($"Parameter '{path}' has unsupported type {value.GetType()}."),
                };

                break;
        }

        // The shared free-then-decode policy; this path only wraps the decoded failure.
        RelateException? failure = Native.Error(status, err, "params");
        if (failure is not null)
        {
            throw new ArgumentException($"relate rejected parameter '{path}': {failure.Message}", failure);
        }
    }

    /// <summary>
    /// The one shape classification of a parameter object as a closed set of types — each carrying
    /// what it matched, no discriminant beside nullable payloads.
    /// </summary>
    private abstract record ParameterShape
    {
        private ParameterShape()
        {
        }

        /// <summary>A map-like or property-bag shape, serialized as a Cypher MAP.</summary>
        internal sealed record Entries(IEnumerable<KeyValuePair<string, object?>> Values) : ParameterShape;

        /// <summary>A list-like shape, serialized as a Cypher LIST.</summary>
        internal sealed record Items(IEnumerable Values) : ParameterShape;

        /// <summary>A graph entity, which cannot be a parameter.</summary>
        internal sealed record Entity : ParameterShape;

        /// <summary>Anything the writer cannot serialize.</summary>
        internal sealed record Unsupported : ParameterShape;
    }

    private static ParameterShape Classify(object value)
    {
        if (value is Node or Relationship or GraphPath)
        {
            return new ParameterShape.Entity();
        }

        IEnumerable<KeyValuePair<string, object?>>? entries = GetMapEntries(value);
        if (entries is not null)
        {
            return new ParameterShape.Entries(entries);
        }

        if (value is IEnumerable items and not string)
        {
            return new ParameterShape.Items(items);
        }

        entries = GetObjectEntries(value);
        return entries is not null ? new ParameterShape.Entries(entries) : new ParameterShape.Unsupported();
    }

    private static RelateStatus WriteMap(nint builder, string? name, IEnumerable<KeyValuePair<string, object?>> entries, string path, int depth, nint* err)
    {
        RelateStatus status = relate_params_put_map(builder, name, out nint child, err);
        if (status == RelateStatus.Ok)
        {
            foreach (KeyValuePair<string, object?> entry in entries)
            {
                WriteValue(child, entry.Key, entry.Value, $"{path}.{entry.Key}", depth + 1);
            }
        }

        return status;
    }

    private static RelateStatus WriteList(nint builder, string? name, IEnumerable items, string path, int depth, nint* err)
    {
        RelateStatus status = relate_params_put_list(builder, name, out nint child, err);
        if (status == RelateStatus.Ok)
        {
            int index = 0;
            foreach (object? item in items)
            {
                WriteValue(child, null, item, $"{path}[{index++}]", depth + 1);
            }
        }

        return status;
    }

    private static RelateStatus PutString(nint builder, string? name, string value, nint* err)
    {
        int max = Encoding.UTF8.GetMaxByteCount(value.Length);
        Span<byte> buffer = max <= 512 ? stackalloc byte[512] : new byte[Encoding.UTF8.GetByteCount(value)];
        int len = Encoding.UTF8.GetBytes(value, buffer);
        fixed (byte* p = buffer)
        {
            return relate_params_put_string(builder, name, len == 0 ? null : p, (nuint)len, err);
        }
    }

    private static RelateStatus PutDateTime(nint builder, string? name, DateTime wallClock, long offsetSeconds, string? zone, nint* err)
    {
        if (zone is not null)
        {
            Native.ValidateCString(zone, nameof(zone));
        }
        return relate_params_put_datetime(
            builder, name, wallClock.Year, wallClock.Month, wallClock.Day, wallClock.Hour, wallClock.Minute, wallClock.Second,
            CypherDuration.SubSecondNanos(wallClock.Ticks), offsetSeconds, zone, err);
    }

    private static RelateStatus PutVector(nint builder, string? name, CypherVector vector, nint* err)
    {
        string dtype = vector.ElementTypeName;
        if (vector.IsInteger)
        {
            // The stored array pinned directly; the ABI copies during the call, so no defensive copy.
            long[] values = vector.IntegerCoordinateArray;
            fixed (long* p = values)
            {
                return relate_params_put_vector_int(builder, name, dtype, p, (nuint)values.Length, err);
            }
        }
        else
        {
            double[] values = vector.FloatCoordinateArray;
            fixed (double* p = values)
            {
                return relate_params_put_vector_float(builder, name, dtype, p, (nuint)values.Length, err);
            }
        }
    }


    /// <summary>Returns the entries of a string-keyed dictionary-like object, or <see langword="null"/>.</summary>
    private static IEnumerable<KeyValuePair<string, object?>>? GetMapEntries(object value)
    {
        switch (value)
        {
            case IEnumerable<KeyValuePair<string, object?>> typed:
                return typed;
            case IDictionary dictionary:
                return EnumerateDictionary(dictionary);
        }

        // Any IEnumerable<KeyValuePair<string, T>> (e.g. an F# Map<string, int>).
        if (value is IEnumerable enumerable && GetKeyValueType(value.GetType()) is { } kv)
        {
            return EnumerateKeyValuePairs(enumerable, kv.Key, kv.Value);
        }

        return null;
    }

    private static IEnumerable<KeyValuePair<string, object?>> EnumerateDictionary(IDictionary dictionary)
    {
        foreach (DictionaryEntry entry in dictionary)
        {
            yield return new(entry.Key as string ?? throw new ArgumentException("Parameter map keys must be strings."), entry.Value);
        }
    }

    private static IEnumerable<KeyValuePair<string, object?>> EnumerateKeyValuePairs(IEnumerable items, PropertyInfo key, PropertyInfo value)
    {
        foreach (object? item in items)
        {
            yield return new((string)key.GetValue(item)!, value.GetValue(item));
        }
    }

    private static (PropertyInfo Key, PropertyInfo Value)? GetKeyValueType(Type type) => s_keyValueTypes.GetOrAdd(type, static t =>
    {
        foreach (Type iface in t.GetInterfaces())
        {
            if (iface.IsGenericType && iface.GetGenericTypeDefinition() == typeof(IEnumerable<>))
            {
                Type element = iface.GetGenericArguments()[0];
                if (element.IsGenericType
                    && element.GetGenericTypeDefinition() == typeof(KeyValuePair<,>)
                    && element.GetGenericArguments()[0] == typeof(string))
                {
                    return (element.GetProperty("Key")!, element.GetProperty("Value")!);
                }
            }
        }

        return null;
    });

    /// <summary>Returns the public readable instance properties of a POCO / anonymous object / record.</summary>
    private static IEnumerable<KeyValuePair<string, object?>>? GetObjectEntries(object value)
    {
        Type type = value.GetType();
        // A string is the one IEnumerable Classify does not route to Items; it is never a property bag.
        if (type.IsPrimitive || value is string)
        {
            return null;
        }

        PropertyInfo[] properties = s_properties.GetOrAdd(type, static t => t
            .GetProperties(BindingFlags.Public | BindingFlags.Instance)
            .Where(p => p.CanRead && p.GetIndexParameters().Length == 0)
            .ToArray());

        return properties.Length == 0 ? null : properties.Select(p => new KeyValuePair<string, object?>(p.Name, p.GetValue(value)));
    }
}
