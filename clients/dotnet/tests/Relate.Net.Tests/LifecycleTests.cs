namespace Relate.Net.Tests;

public class LifecycleTests
{
    [Fact]
    public void NativeLibrary_ReportsSupportedAbiVersion()
    {
        Assert.Equal(2, RelateDatabase.NativeAbiVersion);
    }

    [Fact]
    public void OpenInMemory_ExposesLocationAndFlags()
    {
        using var db = RelateDatabase.OpenInMemory();

        Assert.Equal(RelateDatabase.InMemory, db.Location);
        Assert.False(db.IsReadOnly);
        Assert.False(db.IsDisposed);
    }

    [Fact]
    public void Open_RejectsNullOrEmptyLocation()
    {
        Assert.Throws<ArgumentNullException>(() => RelateDatabase.Open(null!));
        Assert.Throws<ArgumentException>(() => RelateDatabase.Open(string.Empty));
    }

    [Fact]
    public void OpenReadOnly_MissingStore_ThrowsRelateException()
    {
        using var dir = new TempDirectory();

        var ex = Assert.Throws<RelateException>(() => RelateDatabase.OpenReadOnly(dir.Path));
        Assert.Equal(ErrorStage.Internal, ex.Stage);
    }

    [Fact]
    public void OnDiskDatabase_PersistsAcrossReopen_AndReadOnlyRejectsWrites()
    {
        using var dir = new TempDirectory();
        using (var db = RelateDatabase.Open(dir.Path))
        using (var conn = db.Connect())
        {
            conn.Query("CREATE (:Saved {value: 42})");
            db.Checkpoint();
        }

        using var readOnly = RelateDatabase.OpenReadOnly(dir.Path);
        using var reader = readOnly.Connect();

        Assert.True(readOnly.IsReadOnly);
        Assert.Equal(42L, reader.Query("MATCH (s:Saved) RETURN s.value AS v").Scalar<long>());
        Assert.Throws<RelateException>(() => reader.Query("CREATE (:Nope)"));
        Assert.Throws<RelateException>(() => readOnly.Checkpoint());
    }

    [Fact]
    public async Task OpenAsync_And_CheckpointAsync_Work()
    {
        await using var db = await RelateDatabase.OpenAsync(RelateDatabase.InMemory);
        await db.CheckpointAsync();
        await using var conn = db.Connect();

        Assert.Equal(1L, (await conn.QueryAsync("RETURN 1 AS one")).Scalar<long>());
    }

    [Fact]
    public void Connect_DefaultsToNeo4jNamespace()
    {
        using var db = RelateDatabase.OpenInMemory();
        using var conn = db.Connect();

        Assert.Equal("neo4j", conn.Namespace);
        Assert.Same(db, conn.Database);
    }

    [Fact]
    public void Connect_WithNamespace_IsCaseInsensitive()
    {
        using var db = RelateDatabase.OpenInMemory();
        using var conn = db.Connect("SYSTEM");

        Assert.Equal("system", conn.Namespace);
        Assert.Equal("system", conn.Query("SHOW DATABASES").Namespace);
    }

    [Fact]
    public void Connect_UnknownNamespace_ThrowsWithGraphNotFound()
    {
        using var db = RelateDatabase.OpenInMemory();

        var ex = Assert.Throws<RelateException>(() => db.Connect("no_such_graph"));
        Assert.Contains("Graph not found: no_such_graph", ex.Message);
        Assert.Equal("Neo.ClientError.Database.DatabaseNotFound", ex.Code);
    }

    [Fact]
    public void ChangeNamespace_SwitchesSessionDefault()
    {
        using var t = new TestDb();

        t.Connection.ChangeNamespace("system");
        Assert.Equal("system", t.Connection.Namespace);
        Assert.Equal("system", t.Connection.Query("SHOW DATABASES").Namespace);

        t.Connection.ChangeNamespace("neo4j");
        Assert.Equal("neo4j", t.Connection.Query("RETURN 1").Namespace);
    }

    [Fact]
    public void ConnectionDispose_IsIdempotent_AndBlocksFurtherUse()
    {
        using var db = RelateDatabase.OpenInMemory();
        var conn = db.Connect();

        conn.Dispose();
        conn.Dispose();

        Assert.True(conn.IsDisposed);
        Assert.Throws<ObjectDisposedException>(() => conn.Query("RETURN 1"));
        Assert.Throws<ObjectDisposedException>(() => conn.BeginTransaction());
        Assert.Throws<ObjectDisposedException>(() => conn.ChangeNamespace("system"));
    }

    [Fact]
    public void DatabaseDispose_DisposesItsConnections()
    {
        var db = RelateDatabase.OpenInMemory();
        var a = db.Connect();
        var b = db.Connect();

        db.Dispose();
        db.Dispose();

        Assert.True(db.IsDisposed);
        Assert.True(a.IsDisposed);
        Assert.True(b.IsDisposed);
        Assert.Throws<ObjectDisposedException>(() => a.Query("RETURN 1"));
        Assert.Throws<ObjectDisposedException>(() => db.Connect());
        Assert.Throws<ObjectDisposedException>(() => db.Checkpoint());
    }

    [Fact]
    public void Connections_ShareTheSameGraph()
    {
        using var db = RelateDatabase.OpenInMemory();
        using var writer = db.Connect();
        using var reader = db.Connect();

        writer.Query("CREATE (:Shared)");

        Assert.Equal(1L, reader.Query("MATCH (n:Shared) RETURN count(n)").Scalar<long>());
    }

    [Fact]
    public void InMemoryDatabases_AreIsolatedFromEachOther()
    {
        using var first = new TestDb();
        using var second = new TestDb();

        first.Connection.Query("CREATE (:OnlyInFirst)");

        Assert.Equal(0L, second.Connection.Query("MATCH (n:OnlyInFirst) RETURN count(n)").Scalar<long>());
    }

    [Fact]
    public void Results_RemainUsableAfterTheConnectionIsClosed()
    {
        QueryResult result;
        using (var t = new TestDb())
        {
            result = t.Connection.Query("CREATE (n:Kept {name: 'x'}) RETURN n");
        }

        Assert.Equal("x", Assert.IsType<Node>(result[0]["n"]).Properties["name"]);
    }
}
