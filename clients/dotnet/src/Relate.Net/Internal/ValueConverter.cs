using System.Collections;
using System.Diagnostics.CodeAnalysis;
using System.Globalization;

namespace Relate.Net;

/// <summary>Converts materialized relate values to a requested .NET type for <c>Get&lt;T&gt;</c>.</summary>
internal static class ValueConverter
{
    /// <summary>
    /// Converts to <typeparamref name="T"/>; the result may be <see langword="null"/> for a Cypher
    /// null read into a nullable target — the contract <see cref="RelateRecord.Get{T}(string)"/> states.
    /// </summary>
    [return: MaybeNull]
    internal static T Convert<T>(object? value) => (T)Convert(value, typeof(T))!;

    internal static object? Convert(object? value, Type target)
    {
        if (value is null)
        {
            if (!target.IsValueType || Nullable.GetUnderlyingType(target) is not null)
            {
                return null;
            }

            throw new InvalidCastException($"Cannot convert a Cypher null to the non-nullable type {target}.");
        }

        // One instance check, after the Nullable unwrap: a boxed T already satisfies Nullable<T>.
        Type type = Nullable.GetUnderlyingType(target) ?? target;
        if (type.IsInstanceOfType(value))
        {
            return value;
        }

        switch (value)
        {
            case CypherZonedDateTime zoned when type == typeof(DateTimeOffset):
                return zoned.DateTimeOffset;
            case CypherDuration duration when type == typeof(TimeSpan):
                return duration.ToTimeSpan();
            case CypherVector vector when TryVectorArray(vector, type, out object? vectorArray):
                return vectorArray;
            case string s when type == typeof(Guid):
                return Guid.Parse(s);
            case string s when type.IsEnum:
                return Enum.Parse(type, s, ignoreCase: true);
            case long l when type.IsEnum:
                return Enum.ToObject(type, l);
            case string s when type == typeof(char) && s.Length == 1:
                return s[0];
            // A string has no conversion left — its Guid, enum and single-char targets are
            // handled above — so it is terminal here: a string-to-numeric Get<T> throws
            // rather than silently parsing.
            case string:
                throw new InvalidCastException($"Cannot convert a string to {target}.");
        }

        // The string target has its own rendering before ChangeType.
        if (type == typeof(string))
        {
            return System.Convert.ToString(value, CultureInfo.InvariantCulture);
        }

        if (value is IConvertible && typeof(IConvertible).IsAssignableFrom(type))
        {
            return System.Convert.ChangeType(value, type, CultureInfo.InvariantCulture);
        }

        if (value is IReadOnlyDictionary<string, object?> map && TryConvertMap(map, type, out object? convertedMap))
        {
            return convertedMap;
        }

        if (value is IReadOnlyList<object?> list && TryConvertList(list, type, out object? convertedList))
        {
            return convertedList;
        }

        throw new InvalidCastException($"Cannot convert a value of type {value.GetType()} to {target}.");
    }

    /// <summary>
    /// A <see cref="CypherVector"/> as the array target asks for it — the one place the three vector
    /// array targets are spelled; <see langword="false"/> for any other target. The matcher
    /// vocabulary of <see cref="TryConvertList"/> and <see cref="TryConvertMap"/>.
    /// </summary>
    private static bool TryVectorArray(CypherVector vector, Type type, out object? result)
    {
        if (type == typeof(double[]))
        {
            result = vector.ToDoubleArray();
            return true;
        }

        if (type == typeof(float[]))
        {
            result = vector.ToSingleArray();
            return true;
        }

        if (type == typeof(long[]))
        {
            result = vector.ToInt64Array();
            return true;
        }

        result = null;
        return false;
    }

    private static bool TryConvertList(IReadOnlyList<object?> list, Type type, out object? result)
    {
        Type? element = ElementTypeOf(type);
        if (element is null)
        {
            result = null;
            return false;
        }

        var typed = (IList)Activator.CreateInstance(typeof(List<>).MakeGenericType(element), list.Count)!;
        foreach (object? item in list)
        {
            typed.Add(Convert(item, element));
        }

        // One fill loop; an array target copies the filled list into its final shape.
        result = type.IsSZArray ? ToArray(typed, element) : typed;
        return true;
    }

    private static Array ToArray(IList typed, Type element)
    {
        Array array = Array.CreateInstance(element, typed.Count);
        for (int i = 0; i < typed.Count; i++)
        {
            array.SetValue(typed[i], i);
        }

        return array;
    }

    /// <summary>
    /// The element type of a list target: an SZ array's element, or the argument of the six
    /// list-ish generic interfaces; <see langword="null"/> when the target is not a list.
    /// </summary>
    private static Type? ElementTypeOf(Type type)
    {
        if (type.IsSZArray)
        {
            return type.GetElementType()!;
        }

        if (type.IsGenericType)
        {
            Type definition = type.GetGenericTypeDefinition();
            if (definition == typeof(List<>) || definition == typeof(IList<>) || definition == typeof(ICollection<>)
                || definition == typeof(IEnumerable<>) || definition == typeof(IReadOnlyList<>) || definition == typeof(IReadOnlyCollection<>))
            {
                return type.GetGenericArguments()[0];
            }
        }

        return null;
    }

    private static bool TryConvertMap(IReadOnlyDictionary<string, object?> map, Type type, out object? result)
    {
        result = null;
        if (!type.IsGenericType)
        {
            return false;
        }

        Type definition = type.GetGenericTypeDefinition();
        Type[] args = type.GetGenericArguments();
        if ((definition != typeof(Dictionary<,>) && definition != typeof(IDictionary<,>) && definition != typeof(IReadOnlyDictionary<,>))
            || args[0] != typeof(string))
        {
            return false;
        }

        var typed = (IDictionary)Activator.CreateInstance(typeof(Dictionary<,>).MakeGenericType(typeof(string), args[1]))!;
        foreach (KeyValuePair<string, object?> entry in map)
        {
            typed.Add(entry.Key, Convert(entry.Value, args[1]));
        }

        result = typed;
        return true;
    }
}
