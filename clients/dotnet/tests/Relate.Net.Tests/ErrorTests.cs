namespace Relate.Net.Tests;

/// <summary>How relate failures surface as .NET exceptions.</summary>
public class ErrorTests
{
    [Fact]
    public void SyntaxError_IsCompileStageRelateException()
    {
        using var t = new TestDb();

        var ex = Assert.Throws<RelateException>(() => t.Connection.Query("RETRN 1"));

        Assert.Equal(ErrorStage.Compile, ex.Stage);
        Assert.Equal("Neo.ClientError.Statement.SyntaxError", ex.Code);
        Assert.False(string.IsNullOrEmpty(ex.GqlStatus));
        Assert.False(ex.IsRetryable);
        Assert.Contains("RETRN", ex.Message);
        Assert.Contains(ex.Code, ex.ToString());
    }

    [Fact]
    public void RuntimeError_IsRuntimeStageRelateException()
    {
        using var t = new TestDb();

        var ex = Assert.Throws<RelateException>(() => t.Connection.Query("RETURN 1 / 0"));

        Assert.Equal(ErrorStage.Runtime, ex.Stage);
        Assert.Equal("Neo.ClientError.Statement.ArithmeticError", ex.Code);
    }

    [Fact]
    public void MissingParameter_IsReported()
    {
        using var t = new TestDb();
        Assert.Throws<RelateException>(() => t.Connection.Query("RETURN $notProvided"));
    }

    [Fact]
    public void ConnectionRemainsUsableAfterAnError()
    {
        using var t = new TestDb();

        Assert.Throws<RelateException>(() => t.Connection.Query("THIS IS NOT CYPHER"));

        Assert.Equal(1L, t.Connection.Query("RETURN 1").Scalar<long>());
    }

    [Fact]
    public async Task AsyncQuery_SurfacesTheSameException()
    {
        using var t = new TestDb();

        var ex = await Assert.ThrowsAsync<RelateException>(() => t.Connection.QueryAsync("RETURN 1 / 0"));

        Assert.Equal(ErrorStage.Runtime, ex.Stage);
    }

    [Fact]
    public async Task NullCypher_IsArgumentNullException()
    {
        using var t = new TestDb();

        Assert.Throws<ArgumentNullException>(() => t.Connection.Query(null!));
        await Assert.ThrowsAsync<ArgumentNullException>(() => t.Connection.QueryAsync(null!));
    }

    [Fact]
    public void RelateException_HasStandardConstructors()
    {
        Assert.Equal("boom", new RelateException("boom").Message);
        Assert.IsType<InvalidOperationException>(new RelateException("x", new InvalidOperationException()).InnerException);

        var full = new RelateException("m", "Neo.X", "42000", ErrorStage.Runtime, isRetryable: true);
        Assert.Equal("Neo.X", full.Code);
        Assert.Equal("42000", full.GqlStatus);
        Assert.Equal(ErrorStage.Runtime, full.Stage);
        Assert.True(full.IsRetryable);
        Assert.Equal(string.Empty, new RelateException().Code);
    }
}
