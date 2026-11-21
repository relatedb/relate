using System.Collections;
using System.Globalization;

namespace Relate.Net;

/// <summary>Formats values Cypher-style for <c>ToString()</c> diagnostics.</summary>
internal static class ValueFormatter
{
    internal static string Format(object? value) => value switch
    {
        null => "null",
        string s => $"'{s.Replace("'", "\\'", StringComparison.Ordinal)}'",
        bool b => b ? "true" : "false",
        IReadOnlyDictionary<string, object?> map => "{" + string.Join(", ", map.Select(e => $"{e.Key}: {Format(e.Value)}")) + "}",
        IEnumerable list and not string => "[" + string.Join(", ", list.Cast<object?>().Select(Format)) + "]",
        IFormattable f => f.ToString(null, CultureInfo.InvariantCulture),
        _ => value.ToString() ?? string.Empty,
    };
}
