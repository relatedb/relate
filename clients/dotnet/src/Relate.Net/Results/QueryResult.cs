using System.Collections;
using System.Diagnostics.CodeAnalysis;

namespace Relate.Net;

/// <summary>
/// The fully materialized result of one statement: its records plus metadata (columns, counters,
/// bookmark, notifications, plan and timings). A result holds no native resources and stays
/// valid after its connection is closed.
/// </summary>
public sealed class QueryResult : IReadOnlyList<RelateRecord>
{
    private readonly RelateRecord[] _records;

    internal QueryResult(
        IReadOnlyList<string> columns,
        RelateRecord[] records,
        QueryType queryType,
        string @namespace,
        QueryCounters counters,
        IReadOnlyList<Notification> notifications,
        string? planJson,
        Bookmark? bookmark,
        QueryTimings timings)
    {
        Columns = columns;
        _records = records;
        QueryType = queryType;
        Namespace = @namespace;
        Counters = counters;
        Notifications = notifications;
        PlanJson = planJson;
        Bookmark = bookmark;
        Timings = timings;
    }

    /// <summary>The column names, in order.</summary>
    public IReadOnlyList<string> Columns { get; }

    /// <summary>The number of records.</summary>
    public int Count => _records.Length;

    /// <summary>Gets the record at <paramref name="index"/>.</summary>
    public RelateRecord this[int index] => _records[index];

    /// <summary>The statement type (read, write, read-write or schema).</summary>
    public QueryType QueryType { get; }

    /// <summary>The namespace the statement ran in (its <c>USE</c> target or the session namespace).</summary>
    public string Namespace { get; }

    /// <summary>The mutation counters.</summary>
    public QueryCounters Counters { get; }

    /// <summary>Notifications; populated by <c>EXPLAIN</c> and <c>PROFILE</c>.</summary>
    public IReadOnlyList<Notification> Notifications { get; }

    /// <summary>The structured plan as a <c>relate.plan/v1</c> JSON document; only for <c>EXPLAIN</c>/<c>PROFILE</c>.</summary>
    public string? PlanJson { get; }

    /// <summary>
    /// The bookmark of an auto-commit write; <see langword="null"/> for reads, statements inside an
    /// explicit transaction (see <see cref="RelateTransaction.Commit"/>) and writes that changed nothing.
    /// </summary>
    public Bookmark? Bookmark { get; }

    /// <summary>Engine-reported timings.</summary>
    public QueryTimings Timings { get; }

    /// <summary>
    /// Returns the first column of the first record converted to <typeparamref name="T"/>, e.g.
    /// for <c>MATCH (n) RETURN count(n)</c>.
    /// </summary>
    /// <exception cref="InvalidOperationException">The result has no records or no columns.</exception>
    [return: MaybeNull]
    public T Scalar<T>()
    {
        if (_records.Length == 0 || Columns.Count == 0)
        {
            throw new InvalidOperationException("The result has no records to read a scalar from.");
        }

        return _records[0].Get<T>(0);
    }

    /// <inheritdoc />
    public IEnumerator<RelateRecord> GetEnumerator() => ((IEnumerable<RelateRecord>)_records).GetEnumerator();

    IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
}
