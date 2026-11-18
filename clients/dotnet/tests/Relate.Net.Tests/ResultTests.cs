namespace Relate.Net.Tests;

/// <summary>The <see cref="QueryResult"/> / <see cref="RelateRecord"/> API and result metadata.</summary>
public class ResultTests
{
    [Fact]
    public void Columns_And_Records_AreExposedInOrder()
    {
        using var t = new TestDb();
        var result = t.Connection.Query("UNWIND [3, 1, 2] AS x RETURN x, x * 10 AS tenfold");

        Assert.Equal(new[] { "x", "tenfold" }, result.Columns);
        Assert.Equal(3, result.Count);
        Assert.Equal(new[] { 3L, 1L, 2L }, result.Select(r => r.Get<long>("x")));
        Assert.Equal(30L, result[0][1]);
    }

    [Fact]
    public void Result_SupportsLinq()
    {
        using var t = new TestDb();
        var names = t.Connection.Query("UNWIND ['b', 'a', 'c'] AS name RETURN name")
            .Select(r => r.Get<string>("name"))
            .OrderBy(n => n)
            .ToList();

        Assert.Equal(new[] { "a", "b", "c" }, names);
    }

    [Fact]
    public void EmptyResult_HasColumnsButNoRecords()
    {
        using var t = new TestDb();
        var result = t.Connection.Query("MATCH (n:Nothing) RETURN n");

        Assert.Empty(result);
        Assert.Equal(new[] { "n" }, result.Columns);
        Assert.Throws<InvalidOperationException>(() => result.Scalar<object>());
    }

    [Fact]
    public void Record_IsAReadOnlyDictionary()
    {
        using var t = new TestDb();
        var record = t.Connection.Query("RETURN 1 AS a, 'two' AS b")[0];

        IReadOnlyDictionary<string, object?> dict = record;

        Assert.Equal(2, dict.Count);
        Assert.True(dict.ContainsKey("a"));
        Assert.False(dict.ContainsKey("A"));
        Assert.True(dict.TryGetValue("b", out var b));
        Assert.Equal("two", b);
        Assert.False(dict.TryGetValue("zzz", out _));
        Assert.Equal(new[] { "a", "b" }, dict.Keys);
        Assert.Equal(new object?[] { 1L, "two" }, dict.Values);
        Assert.Equal(new[] { "a", "b" }, dict.Select(kv => kv.Key));
    }

    [Fact]
    public void Record_MissingColumn_ThrowsKeyNotFoundWithColumnList()
    {
        using var t = new TestDb();
        var record = t.Connection.Query("RETURN 1 AS a")[0];

        var ex = Assert.Throws<KeyNotFoundException>(() => record["b"]);
        Assert.Contains("Columns: a", ex.Message);
    }

    [Fact]
    public void Get_PerformsIdiomaticConversions()
    {
        using var t = new TestDb();
        var r = t.Connection.Query(
            "RETURN 42 AS i, [1, 2, 3] AS list, {a: 1, b: 2} AS map, 'Green' AS color, null AS nothing, " +
            "duration('PT90S') AS dur, datetime('2024-06-01T12:00[Europe/Oslo]') AS zoned, 7 AS small")[0];

        Assert.Equal(42, r.Get<int>("i"));
        Assert.Equal(42.0, r.Get<double>("i"));
        Assert.Equal("42", r.Get<string>("i"));
        Assert.Equal(new[] { 1, 2, 3 }, r.Get<int[]>("list"));
        Assert.Equal(new List<long> { 1, 2, 3 }, r.Get<List<long>>("list"));
        Assert.Equal(new long[] { 1, 2, 3 }, r.Get<IReadOnlyList<long>>("list"));
        Assert.Equal(new Dictionary<string, int> { ["a"] = 1, ["b"] = 2 }, r.Get<Dictionary<string, int>>("map"));
        Assert.Equal(Color.Green, r.Get<Color>("color"));
        Assert.Null(r.Get<string?>("nothing"));
        Assert.Null(r.Get<int?>("nothing"));
        Assert.Equal(TimeSpan.FromSeconds(90), r.Get<TimeSpan>("dur"));
        Assert.Equal(new DateTimeOffset(2024, 6, 1, 12, 0, 0, TimeSpan.FromHours(2)), r.Get<DateTimeOffset>("zoned"));
        Assert.Equal((byte)7, r.Get<byte>("small"));
    }

    [Fact]
    public void Get_InvalidConversions_Throw()
    {
        using var t = new TestDb();
        var r = t.Connection.Query("RETURN null AS nothing, 'text' AS s, 300 AS big")[0];

        Assert.Throws<InvalidCastException>(() => r.Get<int>("nothing"));
        Assert.Throws<InvalidCastException>(() => r.Get<Node>("s"));
        Assert.Throws<OverflowException>(() => r.Get<byte>("big"));
    }

    [Fact]
    public void Scalar_ReadsFirstColumnOfFirstRecord()
    {
        using var t = new TestDb();
        Assert.Equal(6, t.Connection.Query("RETURN 1 + 2 + 3 AS sum, 'ignored' AS other").Scalar<int>());
    }

    [Theory]
    [InlineData("RETURN 1", QueryType.Read)]
    [InlineData("CREATE (:Q)", QueryType.Write)]
    [InlineData("CREATE (n:Q) RETURN n", QueryType.ReadWrite)]
    [InlineData("CREATE INDEX FOR (p:Person) ON (p.name)", QueryType.Schema)]
    public void QueryType_IsReported(string cypher, QueryType expected)
    {
        using var t = new TestDb();
        Assert.Equal(expected, t.Connection.Query(cypher).QueryType);
    }

    [Fact]
    public void Counters_ReportMutations()
    {
        using var t = new TestDb();

        var created = t.Connection.Query("CREATE (a:A:B {x: 1, y: 2})-[:R]->(b:A)").Counters;
        Assert.Equal(2, created.NodesCreated);
        Assert.Equal(1, created.RelationshipsCreated);
        Assert.Equal(2, created.PropertiesSet);
        Assert.Equal(3, created.LabelsAdded);
        Assert.True(created.ContainsUpdates);

        var deleted = t.Connection.Query("MATCH (n:A) DETACH DELETE n").Counters;
        Assert.Equal(2, deleted.NodesDeleted);
        Assert.Equal(1, deleted.RelationshipsDeleted);

        var read = t.Connection.Query("RETURN 1").Counters;
        Assert.False(read.ContainsUpdates);
        Assert.Equal(QueryCounters.Empty, read);

        Assert.Equal(1, t.Connection.Query("CREATE INDEX FOR (p:Person) ON (p.name)").Counters.IndexesAdded);
    }

    [Fact]
    public void AutoCommitWrites_ReturnIncreasingBookmarks_ReadsReturnNone()
    {
        using var t = new TestDb();

        var first = t.Connection.Query("CREATE (:B)").Bookmark;
        var second = t.Connection.Query("CREATE (:B)").Bookmark;

        Assert.NotNull(first);
        Assert.NotNull(second);
        Assert.True(second > first);
        Assert.Null(t.Connection.Query("MATCH (b:B) RETURN b").Bookmark);
    }

    [Fact]
    public void Explain_ReturnsPlanAndNotifications()
    {
        using var t = new TestDb();
        var result = t.Connection.Query("EXPLAIN MATCH (a:Person), (b:Person) RETURN a, b");

        Assert.NotNull(result.PlanJson);
        Assert.Contains("relate.plan/v1", result.PlanJson);
        var notification = Assert.Single(result.Notifications, n => n.Code == "Neo.ClientNotification.Statement.CartesianProduct");
        Assert.Equal("PERFORMANCE", notification.Category);
        Assert.NotNull(notification.OperatorId);
        Assert.False(string.IsNullOrEmpty(notification.Remedy));
    }

    [Fact]
    public void PlainQuery_HasNoPlanAndNoNotifications()
    {
        using var t = new TestDb();
        var result = t.Connection.Query("RETURN 1");

        Assert.Null(result.PlanJson);
        Assert.Empty(result.Notifications);
    }

    [Fact]
    public void Timings_AreConsistent()
    {
        using var t = new TestDb();
        var timings = t.Connection.Query("UNWIND range(1, 1000) AS x RETURN sum(x)").Timings;

        Assert.True(timings.Elapsed >= TimeSpan.Zero);
        Assert.True(timings.Parse + timings.Plan + timings.Execute <= timings.Elapsed + TimeSpan.FromTicks(3));
    }

    [Fact]
    public void Namespace_ReflectsUseClause()
    {
        using var t = new TestDb();
        var result = t.Connection.Query("USE system SHOW DATABASES YIELD name RETURN name");

        Assert.Equal("system", result.Namespace);
        Assert.Contains("neo4j", result.Select(r => r.Get<string>("name")));
        Assert.Equal("neo4j", t.Connection.Namespace);
    }

    [Fact]
    public void ArrayConversion_RejectsMultidimensionalTargetBeforeConvertingItems()
    {
        using var t = new TestDb();
        Assert.Throws<InvalidCastException>(() => t.Connection.Query("RETURN [$v]", new { v = long.MaxValue }).Scalar<int[,]>());
    }
}
