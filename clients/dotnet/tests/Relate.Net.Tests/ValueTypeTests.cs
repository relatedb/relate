namespace Relate.Net.Tests;

/// <summary>Pure .NET behaviour of the public value types (no database needed).</summary>
public class ValueTypeTests
{
    [Fact]
    public void Bookmark_ParsesAndComparesNumerically()
    {
        var nine = Bookmark.Parse("relate:9");
        var ten = Bookmark.Parse("relate:10");

        Assert.Equal(9, nine.Sequence);
        Assert.Equal("relate:10", ten.ToString());
        Assert.True(ten > nine);
        Assert.True(nine < ten);
        Assert.True(nine <= Bookmark.Parse("relate:9"));
        Assert.Equal(nine, Bookmark.Parse("relate:9"));
        Assert.True(nine == Bookmark.Parse("relate:9"));
        Assert.Equal(new[] { nine, ten }, new[] { ten, nine }.OrderBy(b => b));
    }

    [Theory]
    [InlineData("")]
    [InlineData("relate:")]
    [InlineData("relate:-1")]
    [InlineData("neo4j:5")]
    [InlineData("relate:abc")]
    public void Bookmark_RejectsInvalidText(string text)
    {
        Assert.False(Bookmark.TryParse(text, out _));
        Assert.Throws<FormatException>(() => Bookmark.Parse(text));
    }

    [Fact]
    public void CypherPoint_FactoriesSetSridAndDimension()
    {
        Assert.Equal((CypherPoint.CartesianSrid, 2), (CypherPoint.Cartesian(1, 2).Srid, CypherPoint.Cartesian(1, 2).Dimension));
        Assert.Equal((CypherPoint.Cartesian3DSrid, 3), (CypherPoint.Cartesian(1, 2, 3).Srid, CypherPoint.Cartesian(1, 2, 3).Dimension));
        Assert.Equal(CypherPoint.Wgs84Srid, CypherPoint.Wgs84(1, 2).Srid);
        Assert.Equal(CypherPoint.Wgs843DSrid, CypherPoint.Wgs84(1, 2, 3).Srid);
    }

    [Fact]
    public void CypherDuration_ConvertsFromTimeSpan()
    {
        Assert.Equal(new CypherDuration(0, 0, 1, 500_000_000), CypherDuration.FromTimeSpan(TimeSpan.FromMilliseconds(1500)));
        Assert.Equal(TimeSpan.FromMilliseconds(-1500), CypherDuration.FromTimeSpan(TimeSpan.FromMilliseconds(-1500)).ToTimeSpan());
    }

    [Fact]
    public void CypherVector_ValidatesElementTypeFamily()
    {
        Assert.Throws<ArgumentException>(() => CypherVector.FromDoubles(new[] { 1.0 }, VectorElementType.Integer8));
        Assert.Throws<ArgumentException>(() => CypherVector.FromInt64s(new[] { 1L }, VectorElementType.Float32));
        Assert.Equal("FLOAT32", CypherVector.FromSingles(new[] { 1f }).ElementTypeName);
        Assert.Equal(3, CypherVector.FromInt32s(new[] { 1, 2, 3 }).Dimension);
    }

    [Fact]
    public void GraphPath_ValidatesShape()
    {
        var empty = new Dictionary<string, object?>();
        var node = new Node("n0", 0, Array.Empty<string>(), empty);

        Assert.Throws<ArgumentException>(() => new GraphPath(new[] { node }, new[] { new Relationship("r0", 0, "R", "n0", 0, "n1", 1, empty) }));
        Assert.Equal(0, new GraphPath(new[] { node }, Array.Empty<Relationship>()).Length);
    }

    [Fact]
    public void ContainsUpdates_DoesNotSumCounters()
    {
        Assert.True(new QueryCounters { NodesCreated = long.MinValue, NodesDeleted = long.MinValue }.ContainsUpdates);
        Assert.True(new QueryCounters { NodesCreated = 1, NodesDeleted = -1 }.ContainsUpdates);
        Assert.False(QueryCounters.Empty.ContainsUpdates);
    }
}
