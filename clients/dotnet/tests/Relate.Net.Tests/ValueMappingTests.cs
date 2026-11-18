namespace Relate.Net.Tests;

/// <summary>Cypher values produced by the engine and the .NET types they are mapped to.</summary>
public class ValueMappingTests
{
    [Fact]
    public void Scalars_MapToPrimitiveTypes()
    {
        using var t = new TestDb();
        var r = t.Connection.Query("RETURN 1 AS i, 2.5 AS f, 'x' AS s, true AS b, null AS n")[0];

        Assert.IsType<long>(r["i"]);
        Assert.IsType<double>(r["f"]);
        Assert.IsType<string>(r["s"]);
        Assert.IsType<bool>(r["b"]);
        Assert.Null(r["n"]);
    }

    [Fact]
    public void ListsAndMaps_MapToReadOnlyCollections()
    {
        using var t = new TestDb();
        var r = t.Connection.Query("RETURN [1, 'a', [true]] AS l, {b: 2, a: {c: null}} AS m")[0];

        var list = Assert.IsAssignableFrom<IReadOnlyList<object?>>(r["l"]);
        var map = Assert.IsAssignableFrom<IReadOnlyDictionary<string, object?>>(r["m"]);

        Assert.Equal(1L, list[0]);
        Assert.Equal("a", list[1]);
        Assert.Equal(new object[] { true }, (IEnumerable<object?>)list[2]!);
        Assert.Equal(2L, map["b"]);
        Assert.Null(Assert.IsAssignableFrom<IReadOnlyDictionary<string, object?>>(map["a"])["c"]);
        Assert.Throws<NotSupportedException>(() => ((IList<object?>)list).Add(1));
    }

    [Fact]
    public void Nodes_ExposeIdsLabelsAndProperties()
    {
        using var t = new TestDb();
        var r = t.Connection.Query("CREATE (n:Person:Dev {name: 'Ada', age: 36}) RETURN n, elementId(n) AS eid, id(n) AS nid")[0];

        var node = r.Get<Node>("n");
        Assert.NotNull(node);

        Assert.Equal(r.Get<string>("eid"), node.ElementId);
        Assert.Equal(r.Get<long>("nid"), node.Id);
        Assert.Equal(new[] { "Person", "Dev" }, node.Labels);
        Assert.True(node.HasLabel("Dev"));
        Assert.False(node.HasLabel("dev"));
        Assert.Equal("Ada", node["name"]);
        Assert.Equal(36, node.Get<int>("age"));
        Assert.Throws<KeyNotFoundException>(() => node["missing"]);
    }

    [Fact]
    public void ElementId_RoundTripsThroughParameters()
    {
        using var t = new TestDb();
        var created = t.Connection.Query("CREATE (n:Lookup {v: 1}) RETURN n")[0].Get<Node>("n");
        Assert.NotNull(created);

        var found = t.Connection.Query("MATCH (n) WHERE elementId(n) = $id RETURN n", new { id = created.ElementId })[0].Get<Node>("n");
        Assert.NotNull(found);

        Assert.Equal(created, found);
        Assert.Equal(created.GetHashCode(), found.GetHashCode());
    }

    [Fact]
    public void Relationships_ExposeTypeEndpointsAndProperties()
    {
        using var t = new TestDb();
        var r = t.Connection.Query("CREATE (a:P)-[k:KNOWS {since: 2020}]->(b:P) RETURN a, k, b")[0];

        var a = r.Get<Node>("a");
        Assert.NotNull(a);
        var b = r.Get<Node>("b");
        Assert.NotNull(b);
        var rel = r.Get<Relationship>("k");
        Assert.NotNull(rel);

        Assert.Equal("KNOWS", rel.Type);
        Assert.Equal(a.ElementId, rel.StartNodeElementId);
        Assert.Equal(b.ElementId, rel.EndNodeElementId);
        Assert.Equal(a.Id, rel.StartNodeId);
        Assert.Equal(b.Id, rel.EndNodeId);
        Assert.Equal(2020L, rel["since"]);
    }

    [Fact]
    public void Paths_SplitIntoNodesAndRelationships()
    {
        using var t = new TestDb();
        t.Connection.Query("CREATE (:Stop {n: 1})-[:NEXT]->(:Stop {n: 2})-[:NEXT]->(:Stop {n: 3})");

        var path = t.Connection.Query("MATCH p = (:Stop {n: 1})-[:NEXT*]->(:Stop {n: 3}) RETURN p")[0].Get<GraphPath>("p");
        Assert.NotNull(path);

        Assert.Equal(2, path.Length);
        Assert.Equal(3, path.Nodes.Count);
        Assert.Equal(1L, path.Start["n"]);
        Assert.Equal(3L, path.End["n"]);
        Assert.All(path.Relationships, rel => Assert.Equal("NEXT", rel.Type));
        Assert.Equal(path.Nodes[1].ElementId, path.Relationships[0].EndNodeElementId);
    }

    [Fact]
    public void CypherTemporals_MapToDotNetTypes()
    {
        using var t = new TestDb();
        var r = t.Connection.Query(
            "RETURN date('2024-02-29') AS d, localtime('12:34:56.1234567') AS lt, time('12:34:56+02:00') AS t, " +
            "localdatetime('2024-01-02T03:04:05') AS ldt, datetime('2024-01-02T03:04:05-05:00') AS dt, " +
            "datetime('2024-06-01T12:00[Europe/Stockholm]') AS zdt")[0];

        Assert.Equal(new DateOnly(2024, 2, 29), r["d"]);
        Assert.Equal(new TimeOnly(12, 34, 56).Add(TimeSpan.FromTicks(1234567)), r["lt"]);
        Assert.Equal(new CypherOffsetTime(new TimeOnly(12, 34, 56), TimeSpan.FromHours(2)), r["t"]);
        Assert.Equal(new DateTime(2024, 1, 2, 3, 4, 5), r["ldt"]);
        Assert.Equal(new DateTimeOffset(2024, 1, 2, 3, 4, 5, TimeSpan.FromHours(-5)), r["dt"]);

        var zoned = Assert.IsType<CypherZonedDateTime>(r["zdt"]);
        Assert.Equal("Europe/Stockholm", zoned.ZoneId);
        Assert.Equal(new DateTimeOffset(2024, 6, 1, 12, 0, 0, TimeSpan.FromHours(2)), zoned.DateTimeOffset);
        DateTimeOffset implicitConversion = zoned;
        Assert.Equal(zoned.DateTimeOffset, implicitConversion);
    }

    [Fact]
    public void Nanoseconds_AreTruncatedTo100nsTicks()
    {
        using var t = new TestDb();
        var value = t.Connection.Query("RETURN localtime('00:00:00.123456789') AS v").Scalar<TimeOnly>();

        Assert.Equal(1234567, value.Ticks);
    }

    [Fact]
    public void Durations_KeepAllComponents()
    {
        using var t = new TestDb();
        var d = t.Connection.Query("RETURN duration('P1Y2M3DT4H5.5S') AS d").Scalar<CypherDuration>();

        Assert.Equal(new CypherDuration(14, 3, 4 * 3600 + 5, 500_000_000), d);
        Assert.Throws<InvalidOperationException>(() => d.ToTimeSpan());
        Assert.Equal(new TimeSpan(1, 2, 0, 3), new CypherDuration(0, 1, 7203, 0).ToTimeSpan());
    }

    [Fact]
    public void Points_MapToCypherPoint()
    {
        using var t = new TestDb();
        var r = t.Connection.Query("RETURN point({x: 1, y: 2}) AS c, point({longitude: 12.5, latitude: 55.5, height: 7}) AS g")[0];

        Assert.Equal(CypherPoint.Cartesian(1, 2), r["c"]);
        var g = r.Get<CypherPoint>("g");
        Assert.Equal(CypherPoint.Wgs843DSrid, g.Srid);
        Assert.Equal(3, g.Dimension);
        Assert.Equal(7.0, g.Z);
    }

    [Fact]
    public void Vectors_MapToCypherVector()
    {
        using var t = new TestDb();
        var r = t.Connection.Query("RETURN vector([1.5, 2.5], 2, FLOAT32) AS f, vector([1, 2, 3], 3, INTEGER8) AS i")[0];

        var f = r.Get<CypherVector>("f");
        Assert.NotNull(f);
        var i = r.Get<CypherVector>("i");
        Assert.NotNull(i);

        Assert.Equal(VectorElementType.Float32, f.ElementType);
        Assert.Equal(new[] { 1.5f, 2.5f }, f.ToSingleArray());
        Assert.True(i.IsInteger);
        Assert.Equal(VectorElementType.Integer8, i.ElementType);
        Assert.Equal(new long[] { 1, 2, 3 }, i.ToInt64Array());
        Assert.Throws<InvalidOperationException>(() => f.ToInt64Array());
    }

    [Fact]
    public void SpecialFloats_AreReturnedFaithfully()
    {
        using var t = new TestDb();
        var r = t.Connection.Query("RETURN 0.0 / 0.0 AS nan, 1.0 / 0.0 AS inf, -1.0 / 0.0 AS ninf")[0];

        Assert.True(double.IsNaN(r.Get<double>("nan")));
        Assert.Equal(double.PositiveInfinity, r["inf"]);
        Assert.Equal(double.NegativeInfinity, r["ninf"]);
    }
}
