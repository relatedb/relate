using System.Collections;
using System.Diagnostics.CodeAnalysis;

namespace Relate.Net;

/// <summary>
/// One row of a <see cref="QueryResult"/>. Values are addressable by column name or index and
/// the record is also a read-only dictionary from column name to value.
/// </summary>
public sealed class RelateRecord : IReadOnlyDictionary<string, object?>
{
    private readonly RecordSchema _schema;
    private readonly object?[] _values;

    internal RelateRecord(RecordSchema schema, object?[] values)
    {
        _schema = schema;
        _values = values;
    }

    /// <summary>The column names, in order.</summary>
    public IReadOnlyList<string> Keys => _schema.Columns;

    /// <summary>The values, in column order.</summary>
    public IReadOnlyList<object?> Values => Array.AsReadOnly(_values);

    /// <summary>The number of columns.</summary>
    public int Count => _values.Length;

    /// <summary>Gets the value in column <paramref name="index"/>.</summary>
    public object? this[int index] => _values[index];

    /// <summary>Gets the value in the named column.</summary>
    /// <exception cref="KeyNotFoundException">There is no such column.</exception>
    public object? this[string key] => _values[IndexOf(key)];

    /// <summary>Gets the value in the named column converted to <typeparamref name="T"/>.</summary>
    /// <remarks>
    /// Besides exact type matches this performs numeric conversions (<c>long</c> to <c>int</c>,
    /// checked), list conversions (to <c>T[]</c>, <c>List&lt;T&gt;</c>, <c>IReadOnlyList&lt;T&gt;</c>),
    /// map conversions (to <c>Dictionary&lt;string, T&gt;</c>), enum/Guid parsing from strings and
    /// <see cref="CypherDuration"/> to <see cref="TimeSpan"/>.
    /// </remarks>
    /// <exception cref="KeyNotFoundException">There is no such column.</exception>
    /// <exception cref="InvalidCastException">The value cannot be converted.</exception>
    [return: MaybeNull]
    public T Get<T>(string key) => ValueConverter.Convert<T>(this[key]);

    /// <summary>Gets the value in column <paramref name="index"/> converted to <typeparamref name="T"/>.</summary>
    [return: MaybeNull]
    public T Get<T>(int index) => ValueConverter.Convert<T>(_values[index]);

    /// <summary>Returns <see langword="true"/> when the record has the named column.</summary>
    public bool ContainsKey(string key) => _schema.IndexByName.ContainsKey(key);

    /// <summary>Tries to get the value of the named column.</summary>
    public bool TryGetValue(string key, out object? value)
    {
        if (_schema.IndexByName.TryGetValue(key, out int index))
        {
            value = _values[index];
            return true;
        }

        value = null;
        return false;
    }

    /// <inheritdoc />
    public IEnumerator<KeyValuePair<string, object?>> GetEnumerator()
    {
        for (int i = 0; i < _values.Length; i++)
        {
            yield return new KeyValuePair<string, object?>(_schema.Columns[i], _values[i]);
        }
    }

    /// <inheritdoc />
    public override string ToString() => ValueFormatter.Format((IReadOnlyDictionary<string, object?>)this);

    IEnumerable<string> IReadOnlyDictionary<string, object?>.Keys => Keys;

    IEnumerable<object?> IReadOnlyDictionary<string, object?>.Values => Values;

    bool IReadOnlyDictionary<string, object?>.TryGetValue(string key, [MaybeNullWhen(false)] out object? value) => TryGetValue(key, out value);

    IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();

    private int IndexOf(string key)
    {
        ArgumentNullException.ThrowIfNull(key);
        return _schema.IndexByName.TryGetValue(key, out int index)
            ? index
            : throw new KeyNotFoundException($"The result has no column named '{key}'. Columns: {string.Join(", ", _schema.Columns)}.");
    }
}

/// <summary>Column names shared by all records of one result.</summary>
internal sealed class RecordSchema
{
    internal RecordSchema(string[] columns)
    {
        Columns = Array.AsReadOnly(columns);
        IndexByName = new Dictionary<string, int>(columns.Length, StringComparer.Ordinal);
        for (int i = 0; i < columns.Length; i++)
        {
            // The front-end rejects duplicate output names, so a duplicate column is contract
            // drift: throw at the boundary rather than silently keeping the first index.
            IndexByName.Add(columns[i], i);
        }
    }

    internal IReadOnlyList<string> Columns { get; }

    internal Dictionary<string, int> IndexByName { get; }
}
