using System.Text.Json;

namespace Relate.Net.Interop;

/// <summary>
/// The one JSON scalar-extractor policy: probe a property by name and value kind, returning
/// <see langword="null"/> when absent or of another kind. Call sites choose their default.
/// </summary>
internal static class JsonValues
{
    /// <summary>A string property, or <see langword="null"/> when absent or not a string.</summary>
    internal static string? String(JsonElement element, string name) =>
        element.TryGetProperty(name, out JsonElement value) && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;

    /// <summary>An integer property, or <see langword="null"/> when absent or not representable as a long.</summary>
    internal static long? Int64(JsonElement element, string name) =>
        element.TryGetProperty(name, out JsonElement value)
        && value.ValueKind == JsonValueKind.Number
        && value.TryGetInt64(out long integer)
            ? integer
            : null;

    /// <summary>A boolean property, or <see langword="null"/> when absent or not a boolean.</summary>
    internal static bool? Boolean(JsonElement element, string name) =>
        element.TryGetProperty(name, out JsonElement value)
            && value.ValueKind is JsonValueKind.True or JsonValueKind.False
                ? value.GetBoolean()
                : null;

    /// <summary>
    /// The one required-field policy on top of the extractor: an absent or mistyped field is
    /// contract drift, so fail loudly naming the field and the document it was expected in.
    /// </summary>
    internal static string RequiredString(JsonElement element, string name, string document) =>
        String(element, name)
            ?? throw new FormatException($"The {document} lacks the required string field '{name}'.");

    /// <summary>See <see cref="RequiredString"/>: the integer variant.</summary>
    internal static long RequiredInt64(JsonElement element, string name, string document) =>
        Int64(element, name)
            ?? throw new FormatException($"The {document} lacks the required number field '{name}'.");

    /// <summary>See <see cref="RequiredString"/>: the boolean variant.</summary>
    internal static bool RequiredBoolean(JsonElement element, string name, string document) =>
        Boolean(element, name)
            ?? throw new FormatException($"The {document} lacks the required boolean field '{name}'.");

    /// <summary>
    /// The one JSON document parse boundary: malformed ABI JSON is contract drift and fails as a
    /// <see cref="FormatException"/> naming the document.
    /// </summary>
    internal static JsonElement ParseDocument(string json, string document)
    {
        try
        {
            return JsonDocument.Parse(json).RootElement.Clone();
        }
        catch (JsonException ex)
        {
            throw new FormatException($"The {document} is not valid JSON.", ex);
        }
    }
}
