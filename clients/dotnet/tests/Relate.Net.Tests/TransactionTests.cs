namespace Relate.Net.Tests;

public class TransactionTests
{
    private static long Count(RelateConnection conn, string label) =>
        conn.Query($"MATCH (n:{label}) RETURN count(n)").Scalar<long>();

    [Fact]
    public void Commit_MakesWritesVisible_AndReturnsBookmark()
    {
        using var t = new TestDb();
        using var other = t.Database.Connect();

        using var tx = t.Connection.BeginTransaction();
        tx.Query("CREATE (:Tx {n: $n})", new { n = 1 });

        Assert.True(tx.IsActive);
        Assert.Same(tx, t.Connection.CurrentTransaction);
        Assert.Equal(0L, Count(other, "Tx"));

        var bookmark = tx.Commit();

        Assert.NotNull(bookmark);
        Assert.False(tx.IsActive);
        Assert.Null(t.Connection.CurrentTransaction);
        Assert.Equal(1L, Count(other, "Tx"));
    }

    [Fact]
    public void StatementsInsideTransaction_HaveNoBookmarkOfTheirOwn()
    {
        using var t = new TestDb();
        using var tx = t.Connection.BeginTransaction();

        Assert.Null(tx.Query("CREATE (:Tx)").Bookmark);
        Assert.NotNull(tx.Commit());
    }

    [Fact]
    public void Commit_WithoutWrites_ReturnsNullBookmark()
    {
        using var t = new TestDb();
        using var tx = t.Connection.BeginTransaction();
        tx.Query("RETURN 1");

        Assert.Null(tx.Commit());
    }

    [Fact]
    public void Rollback_DiscardsWrites()
    {
        using var t = new TestDb();
        using (var tx = t.Connection.BeginTransaction())
        {
            tx.Query("CREATE (:Gone)");
            tx.Rollback();
            Assert.False(tx.IsActive);
        }

        Assert.Equal(0L, Count(t.Connection, "Gone"));
    }

    [Fact]
    public void Dispose_WithoutCommit_RollsBack()
    {
        using var t = new TestDb();
        using (var tx = t.Connection.BeginTransaction())
        {
            tx.Query("CREATE (:Gone)");
        }

        Assert.Null(t.Connection.CurrentTransaction);
        Assert.Equal(0L, Count(t.Connection, "Gone"));
    }

    [Fact]
    public void Dispose_AfterFailedStatement_RestoresTheConnection()
    {
        using var t = new TestDb();
        using (var tx = t.Connection.BeginTransaction())
        {
            Assert.Throws<RelateException>(() => tx.Query("RETURN 1 / 0"));
        }

        Assert.Equal(1L, t.Connection.Query("RETURN 1").Scalar<long>());
    }

    [Fact]
    public void Commit_AfterFailedStatement_ThrowsAndLeavesConnectionUsable()
    {
        using var t = new TestDb();
        var tx = t.Connection.BeginTransaction();
        tx.Query("CREATE (:Partial)");
        Assert.Throws<RelateException>(() => tx.Query("RETURN 1 / 0"));

        Assert.Throws<RelateException>(() => tx.Commit());
        Assert.False(tx.IsActive);
        Assert.Null(t.Connection.CurrentTransaction);
        Assert.Equal(0L, Count(t.Connection, "Partial"));
        tx.Dispose();
    }

    [Fact]
    public void StatementsOnTheConnection_JoinTheOpenTransaction()
    {
        using var t = new TestDb();
        using (var tx = t.Connection.BeginTransaction())
        {
            t.Connection.Query("CREATE (:ViaConnection)");
            tx.Rollback();
        }

        Assert.Equal(0L, Count(t.Connection, "ViaConnection"));
    }

    [Fact]
    public void NestedBegin_IsRejected()
    {
        using var t = new TestDb();
        using var tx = t.Connection.BeginTransaction();

        Assert.Throws<InvalidOperationException>(() => t.Connection.BeginTransaction());
    }

    [Fact]
    public void CompletedTransaction_RejectsFurtherUse()
    {
        using var t = new TestDb();
        var tx = t.Connection.BeginTransaction();
        tx.Commit();

        Assert.Throws<InvalidOperationException>(() => tx.Commit());
        Assert.Throws<InvalidOperationException>(() => tx.Rollback());
        Assert.Throws<InvalidOperationException>(() => tx.Query("RETURN 1"));
        tx.Dispose(); // no-op, never throws
    }

    [Fact]
    public void ChangeNamespace_IsRejectedWhileTransactionIsOpen()
    {
        using var t = new TestDb();
        using var tx = t.Connection.BeginTransaction();

        var ex = Assert.Throws<RelateException>(() => t.Connection.ChangeNamespace("system"));

        Assert.Contains("transaction is open", ex.Message);
        Assert.Equal("neo4j", t.Connection.Namespace);
    }

    [Fact]
    public void DisposingConnection_EndsTheTransaction()
    {
        using var db = RelateDatabase.OpenInMemory();
        var conn = db.Connect();
        var tx = conn.BeginTransaction();
        tx.Query("CREATE (:Gone)");

        conn.Dispose();

        Assert.False(tx.IsActive);
        tx.Dispose(); // does not throw even though the connection is gone
        using var check = db.Connect();
        Assert.Equal(0L, Count(check, "Gone"));
    }

    [Fact]
    public async Task AsyncTransaction_CommitsAndRollsBack()
    {
        using var t = new TestDb();

        await using (var tx = await t.Connection.BeginTransactionAsync())
        {
            await tx.QueryAsync("CREATE (:AsyncKept)");
            Assert.NotNull(await tx.CommitAsync());
        }

        await using (var tx = await t.Connection.BeginTransactionAsync())
        {
            await tx.QueryAsync("CREATE (:AsyncGone)");
            await tx.RollbackAsync();
        }

        await using (var tx = await t.Connection.BeginTransactionAsync())
        {
            await tx.QueryAsync("CREATE (:AsyncGone)");
        }

        Assert.Equal(1L, Count(t.Connection, "AsyncKept"));
        Assert.Equal(0L, Count(t.Connection, "AsyncGone"));
    }

    [Fact]
    public void CommitBookmarks_AreOrderedAcrossConnections()
    {
        using var t = new TestDb();
        using var other = t.Database.Connect();

        Bookmark? first;
        using (var tx = t.Connection.BeginTransaction())
        {
            tx.Query("CREATE (:One)");
            first = tx.Commit();
        }

        var second = other.Query("CREATE (:Two)").Bookmark;

        Assert.True(second > first);
    }
}
