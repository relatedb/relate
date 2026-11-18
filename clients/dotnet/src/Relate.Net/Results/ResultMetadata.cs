using System.Globalization;
using System.Text.Json;
using Relate.Net.Interop;

namespace Relate.Net;

/// <summary>The kind of statement that produced a result, classified from its plan.</summary>
public enum QueryType
{
    /// <summary>A read-only statement (<c>r</c>).</summary>
    Read = 1,

    /// <summary>A write-only statement (<c>w</c>).</summary>
    Write = 2,

    /// <summary>A statement that reads and writes (<c>rw</c>).</summary>
    ReadWrite = 3,

    /// <summary>A schema statement such as <c>CREATE INDEX</c> (<c>s</c>).</summary>
    Schema = 4,
}

/// <summary>The mutation counters of a statement.</summary>
public sealed record QueryCounters
{
    /// <summary>Counters for a statement that changed nothing.</summary>
    public static QueryCounters Empty { get; } = new();

    /// <summary>Nodes created.</summary>
    public long NodesCreated { get; init; }

    /// <summary>Nodes deleted.</summary>
    public long NodesDeleted { get; init; }

    /// <summary>Relationships created.</summary>
    public long RelationshipsCreated { get; init; }

    /// <summary>Relationships deleted.</summary>
    public long RelationshipsDeleted { get; init; }

    /// <summary>Properties set.</summary>
    public long PropertiesSet { get; init; }

    /// <summary>Labels added.</summary>
    public long LabelsAdded { get; init; }

    /// <summary>Labels removed.</summary>
    public long LabelsRemoved { get; init; }

    /// <summary>Indexes added.</summary>
    public long IndexesAdded { get; init; }

    /// <summary>Indexes removed.</summary>
    public long IndexesRemoved { get; init; }

    /// <summary>Constraints added.</summary>
    public long ConstraintsAdded { get; init; }

    /// <summary>Constraints removed.</summary>
    public long ConstraintsRemoved { get; init; }

    /// <summary>
    /// <see langword="true"/> when any counter is non-zero (OR, not sum): record equality against
    /// <see cref="Empty"/> compares all eleven counters, so no field is re-listed here.
    /// </summary>
    public bool ContainsUpdates => this != Empty;
}

/// <summary>A notification raised by the engine, e.g. a performance hint from <c>EXPLAIN</c>.</summary>
public sealed record Notification
{
    /// <summary>The Neo4j-compatible notification code.</summary>
    public string Code { get; init; } = string.Empty;

    /// <summary>The GQL status code.</summary>
    public string GqlStatus { get; init; } = string.Empty;

    /// <summary>The severity, e.g. <c>INFORMATION</c> or <c>WARNING</c>.</summary>
    public string Severity { get; init; } = string.Empty;

    /// <summary>The category, e.g. <c>PERFORMANCE</c>.</summary>
    public string Category { get; init; } = string.Empty;

    /// <summary>A short title.</summary>
    public string Title { get; init; } = string.Empty;

    /// <summary>A longer description.</summary>
    public string Description { get; init; } = string.Empty;

    /// <summary>A suggested remedy.</summary>
    public string Remedy { get; init; } = string.Empty;

    /// <summary>The id of the plan operator the notification refers to, when operator-scoped.</summary>
    public long? OperatorId { get; init; }
}

/// <summary>Engine-reported timings of a statement.</summary>
/// <param name="Elapsed">Total parse, plan and execute time (excludes client-side marshalling).</param>
/// <param name="Parse">Front-end parse and lowering.</param>
/// <param name="Plan">Binding, logical planning and admission.</param>
/// <param name="Execute">Execution (or plan rendering for <c>EXPLAIN</c>).</param>
public sealed record QueryTimings(TimeSpan Elapsed, TimeSpan Parse, TimeSpan Plan, TimeSpan Execute);

/// <summary>
/// A relate bookmark (<c>relate:&lt;n&gt;</c>): the store's commit sequence after a write.
/// Bookmarks compare by their numeric <see cref="Sequence"/>, so a later commit is always greater.
/// </summary>
public sealed class Bookmark : IEquatable<Bookmark>, IComparable<Bookmark>, IComparable
{
    private const string Prefix = "relate:";

    private Bookmark(string value, long sequence)
    {
        Value = value;
        Sequence = sequence;
    }

    /// <summary>The bookmark text, e.g. <c>relate:42</c>.</summary>
    public string Value { get; }

    /// <summary>The commit sequence number.</summary>
    public long Sequence { get; }

    /// <summary>Parses a bookmark string.</summary>
    /// <exception cref="FormatException"><paramref name="value"/> is not of the form <c>relate:&lt;n&gt;</c>.</exception>
    public static Bookmark Parse(string value) =>
        TryParse(value, out Bookmark? bookmark) ? bookmark : throw new FormatException($"'{value}' is not a relate bookmark (expected 'relate:<n>').");

    /// <summary>Tries to parse a bookmark string.</summary>
    public static bool TryParse(string? value, [System.Diagnostics.CodeAnalysis.NotNullWhen(true)] out Bookmark? bookmark)
    {
        bookmark = null;
        if (value is null || !value.StartsWith(Prefix, StringComparison.Ordinal)
            || !long.TryParse(value.AsSpan(Prefix.Length), NumberStyles.None, CultureInfo.InvariantCulture, out long sequence))
        {
            return false;
        }

        bookmark = new Bookmark(value, sequence);
        return true;
    }

    /// <inheritdoc />
    public int CompareTo(Bookmark? other) => other is null ? 1 : Sequence.CompareTo(other.Sequence);

    int IComparable.CompareTo(object? obj) => obj switch
    {
        null => 1,
        Bookmark b => CompareTo(b),
        _ => throw new ArgumentException("Object is not a Bookmark.", nameof(obj)),
    };

    /// <inheritdoc />
    public bool Equals(Bookmark? other) => other is not null && Sequence == other.Sequence;

    /// <inheritdoc />
    public override bool Equals(object? obj) => Equals(obj as Bookmark);

    /// <inheritdoc />
    public override int GetHashCode() => Sequence.GetHashCode();

    /// <inheritdoc />
    public override string ToString() => Value;

    /// <summary>Equality by sequence.</summary>
    public static bool operator ==(Bookmark? left, Bookmark? right) => left is null ? right is null : left.Equals(right);

    /// <summary>Inequality by sequence.</summary>
    public static bool operator !=(Bookmark? left, Bookmark? right) => !(left == right);

    /// <summary>Compares by sequence.</summary>
    public static bool operator <(Bookmark? left, Bookmark? right) => Compare(left, right) < 0;

    /// <summary>Compares by sequence.</summary>
    public static bool operator >(Bookmark? left, Bookmark? right) => Compare(left, right) > 0;

    /// <summary>Compares by sequence.</summary>
    public static bool operator <=(Bookmark? left, Bookmark? right) => Compare(left, right) <= 0;

    /// <summary>Compares by sequence.</summary>
    public static bool operator >=(Bookmark? left, Bookmark? right) => Compare(left, right) >= 0;

    private static int Compare(Bookmark? left, Bookmark? right) => left is null ? (right is null ? 0 : -1) : left.CompareTo(right);
}

/// <summary>
/// Decodes the native result metadata into the types above — the one home for the query-type
/// codes, the canonical counter keys, the notification fields and the timings conversion.
/// </summary>
internal static class ResultMetadataReader
{
    /// <summary>
    /// Classifies the ABI query-type code. The C ABI guarantees exactly <c>r</c>/<c>w</c>/<c>rw</c>/<c>s</c>
    /// on every good result, so an unrecognized code is contract drift.
    /// </summary>
    /// <exception cref="FormatException">The code is outside the documented vocabulary.</exception>
    internal static QueryType ReadQueryType(string? type) => type switch
    {
        "r" => QueryType.Read,
        "w" => QueryType.Write,
        "rw" => QueryType.ReadWrite,
        "s" => QueryType.Schema,
        _ => throw new FormatException($"Unknown query-type code '{type}'."),
    };

    /// <summary>
    /// Reads the mutation counters. The ABI reports the canonical eleven keys with zeros
    /// included, so a missing stats object or key is contract drift and fails loudly.
    /// </summary>
    /// <exception cref="FormatException">The stats JSON is missing or lacks a canonical key.</exception>
    internal static QueryCounters ReadCounters(string? json)
    {
        if (string.IsNullOrEmpty(json))
        {
            throw new FormatException("The result carries no stats JSON; the ABI reports the canonical counters on every successful result.");
        }

        JsonElement e = JsonValues.ParseDocument(json, "result stats JSON");
        return new QueryCounters
        {
            NodesCreated = JsonValues.RequiredInt64(e, "nodes_created", "result stats JSON"),
            NodesDeleted = JsonValues.RequiredInt64(e, "nodes_deleted", "result stats JSON"),
            RelationshipsCreated = JsonValues.RequiredInt64(e, "relationships_created", "result stats JSON"),
            RelationshipsDeleted = JsonValues.RequiredInt64(e, "relationships_deleted", "result stats JSON"),
            PropertiesSet = JsonValues.RequiredInt64(e, "properties_set", "result stats JSON"),
            LabelsAdded = JsonValues.RequiredInt64(e, "labels_added", "result stats JSON"),
            LabelsRemoved = JsonValues.RequiredInt64(e, "labels_removed", "result stats JSON"),
            IndexesAdded = JsonValues.RequiredInt64(e, "indexes_added", "result stats JSON"),
            IndexesRemoved = JsonValues.RequiredInt64(e, "indexes_removed", "result stats JSON"),
            ConstraintsAdded = JsonValues.RequiredInt64(e, "constraints_added", "result stats JSON"),
            ConstraintsRemoved = JsonValues.RequiredInt64(e, "constraints_removed", "result stats JSON"),
        };
    }

    /// <summary>
    /// Reads the notifications. The six descriptive fields are required; <c>remedy</c> is optional
    /// (the engine folds it into the description and omits the key) and <c>operator_id</c> appears
    /// only on operator-scoped notifications.
    /// </summary>
    /// <exception cref="FormatException">A required field is missing or mistyped.</exception>
    internal static IReadOnlyList<Notification> ReadNotifications(string? json)
    {
        if (string.IsNullOrEmpty(json))
        {
            return Array.Empty<Notification>();
        }

        var list = new List<Notification>();
        foreach (JsonElement n in JsonValues.ParseDocument(json, "result notifications JSON").EnumerateArray())
        {
            list.Add(new Notification
            {
                Code = JsonValues.RequiredString(n, "code", "result notifications JSON"),
                GqlStatus = JsonValues.RequiredString(n, "gql_status", "result notifications JSON"),
                Severity = JsonValues.RequiredString(n, "severity", "result notifications JSON"),
                Category = JsonValues.RequiredString(n, "category", "result notifications JSON"),
                Title = JsonValues.RequiredString(n, "title", "result notifications JSON"),
                Description = JsonValues.RequiredString(n, "description", "result notifications JSON"),
                Remedy = JsonValues.String(n, "remedy") ?? string.Empty,
                OperatorId = JsonValues.Int64(n, "operator_id"),
            });
        }

        return list.AsReadOnly();
    }

    /// <summary>Converts the engine's millisecond timings.</summary>
    internal static QueryTimings ReadTimings(double elapsedMs, double parseMs, double planMs, double executeMs) =>
        new(TimeSpan.FromMilliseconds(elapsedMs), TimeSpan.FromMilliseconds(parseMs), TimeSpan.FromMilliseconds(planMs), TimeSpan.FromMilliseconds(executeMs));

    /// <summary>Parses a bookmark string; <see langword="null"/> means the statement produced none.</summary>
    internal static Bookmark? ReadBookmark(string? bookmark) => bookmark is null ? null : Bookmark.Parse(bookmark);


}
