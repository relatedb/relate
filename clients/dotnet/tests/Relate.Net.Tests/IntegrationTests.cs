namespace Relate.Net.Tests;

/// <summary>Boundary validation (embedded NUL, paths, keys) and value round-trips through the engine.</summary>
public class IntegrationTests
{
    [Fact]
    public void Query_RejectsEmbeddedNulInsteadOfExecutingPrefix()
    {
        using var t = new TestDb();
        Assert.Throws<ArgumentException>(() => t.Connection.Query("CREATE (:Truncated)\0 invalid"));
        Assert.Equal(0L, t.Connection.Query("MATCH (n:Truncated) RETURN count(n)").Scalar<long>());
    }

    [Fact]
    public void DatabasePath_RejectsEmbeddedNul()
    {
        Assert.Throws<ArgumentException>(() => RelateDatabase.Open(":memory:\0ignored"));
        Assert.Throws<ArgumentException>(() => RelateDatabase.OpenReadOnly(":memory:\0ignored"));
    }

    [Fact]
    public void Namespace_RejectsEmbeddedNul()
    {
        using var t = new TestDb();
        Assert.Throws<ArgumentException>(() => t.Database.Connect("neo4j\0ignored"));
        Assert.Throws<ArgumentException>(() => t.Connection.ChangeNamespace("system\0ignored"));
        Assert.Equal("neo4j", t.Connection.Namespace);
    }

    [Fact]
    public void ParameterKey_RejectsEmbeddedNul()
    {
        using var t = new TestDb();
        Assert.Throws<ArgumentException>(() => t.Connection.Query("RETURN $v", new Dictionary<string, object?> { ["v\0ignored"] = 42 }));
        Assert.Throws<ArgumentException>(() => t.RoundTrip(new Dictionary<string, object?> { ["v\0ignored"] = 42 }));
    }

    [Fact]
    public void StringValues_PreserveNulAndUnicode()
    {
        using var t = new TestDb();
        Assert.Equal("Grüße\0世界", t.RoundTrip("Grüße\0世界"));
        Assert.Equal(string.Empty, t.RoundTrip(string.Empty));
    }

    [Fact]
    public void NonFiniteNumbers_RoundTrip()
    {
        using var t = new TestDb();
        Assert.True(double.IsNaN(Assert.IsType<double>(t.RoundTrip(double.NaN))));
        Assert.Equal(double.PositiveInfinity, t.RoundTrip(double.PositiveInfinity));
        Assert.Equal(double.NegativeInfinity, t.RoundTrip(double.NegativeInfinity));
    }
}
