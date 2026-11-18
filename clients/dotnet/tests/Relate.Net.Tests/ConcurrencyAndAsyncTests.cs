namespace Relate.Net.Tests;

public class ConcurrencyAndAsyncTests
{
    [Fact]
    public async Task QueryAsync_ReturnsResult()
    {
        using var t = new TestDb();
        var result = await t.Connection.QueryAsync("RETURN $x * 2 AS y", new { x = 21 });

        Assert.Equal(42L, result.Scalar<long>());
    }

    [Fact]
    public async Task QueryAsync_WithCanceledToken_DoesNotRun()
    {
        using var t = new TestDb();
        using var cts = new CancellationTokenSource();
        cts.Cancel();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => t.Connection.QueryAsync("CREATE (:Never)", cancellationToken: cts.Token));

        Assert.Equal(0L, t.Connection.Query("MATCH (n:Never) RETURN count(n)").Scalar<long>());
    }

    [Fact]
    public async Task SharedConnection_SerializesConcurrentCallers()
    {
        using var t = new TestDb();

        var tasks = Enumerable.Range(0, 64)
            .Select(i => i % 2 == 0
                ? Task.Run(() => t.Connection.Query("CREATE (:C {i: $i}) RETURN $i AS i", new { i }).Scalar<long>())
                : t.Connection.QueryAsync("CREATE (:C {i: $i}) RETURN $i AS i", new { i }).ContinueWith(r => r.Result.Scalar<long>()))
            .ToArray();

        long[] results = await Task.WhenAll(tasks);

        Assert.Equal(Enumerable.Range(0, 64).Sum(), results.Sum());
        Assert.Equal(64L, t.Connection.Query("MATCH (c:C) RETURN count(c)").Scalar<long>());
    }

    [Fact]
    public async Task SeparateConnections_RunInParallel()
    {
        using var db = RelateDatabase.OpenInMemory();

        var workers = Enumerable.Range(0, 8).Select(w => Task.Run(() =>
        {
            using var conn = db.Connect();
            for (int i = 0; i < 25; i++)
            {
                conn.Query("CREATE (:W {worker: $w, i: $i})", new { w, i });
            }
        }));

        await Task.WhenAll(workers);

        using var check = db.Connect();
        Assert.Equal(200L, check.Query("MATCH (n:W) RETURN count(n)").Scalar<long>());
    }

    [Fact]
    public async Task DisposeAsync_ClosesConnection()
    {
        using var db = RelateDatabase.OpenInMemory();
        var conn = db.Connect();

        await conn.DisposeAsync();

        Assert.True(conn.IsDisposed);
        await Assert.ThrowsAsync<ObjectDisposedException>(() => conn.QueryAsync("RETURN 1"));
    }

    [Theory]
    [InlineData(false, false)]
    [InlineData(false, true)]
    [InlineData(true, false)]
    [InlineData(true, true)]
    public async Task PreparingQuery_CannotEscapeCompletedTransaction(bool commit, bool viaTransaction)
    {
        using var t = new TestDb();
        using var tx = t.Connection.BeginTransaction();
        using var parameters = new BlockingParameters();
        var pending = viaTransaction
            ? tx.QueryAsync("CREATE (:Escaped {value: $Value})", parameters)
            : t.Connection.QueryAsync("CREATE (:Escaped {value: $Value})", parameters);
        try
        {
            await parameters.Entered.Task.WaitAsync(TimeSpan.FromSeconds(10));
            // Preparation is outside the gate; transaction completion must be rechecked afterwards.
            if (commit) await tx.CommitAsync();
            else await tx.RollbackAsync();
            parameters.Release.Set();
            await Assert.ThrowsAsync<InvalidOperationException>(() => pending);
            Assert.Equal(0L, t.Connection.Query("MATCH (n:Escaped) RETURN count(n)").Scalar<long>());
        }
        finally
        {
            parameters.Release.Set();
        }
    }

    [Fact]
    public async Task PreparingAutoCommitQuery_CannotEnterNewTransaction()
    {
        using var t = new TestDb();
        using var parameters = new BlockingParameters();
        var pending = t.Connection.QueryAsync("CREATE (:Escaped {value: $Value})", parameters);
        try
        {
            await parameters.Entered.Task.WaitAsync(TimeSpan.FromSeconds(10));
            using var tx = t.Connection.BeginTransaction();
            parameters.Release.Set();
            await Assert.ThrowsAsync<InvalidOperationException>(() => pending);
            tx.Commit();
            Assert.Equal(0L, t.Connection.Query("MATCH (n:Escaped) RETURN count(n)").Scalar<long>());
        }
        finally
        {
            parameters.Release.Set();
        }
    }

    [Fact]
    public async Task ParameterGetter_CanQuerySameConnectionWithoutDeadlock()
    {
        // Dispose after completion so a regressed deadlock fails by timeout, rather
        // than blocking test cleanup on the same semaphore.
        var t = new TestDb();
        var result = await Task.Run(() => t.Connection.Query("RETURN $Value",
            new CallbackParameters(() => t.Connection.Query("RETURN 42").Scalar<long>())))
            .WaitAsync(TimeSpan.FromSeconds(10));
        t.Dispose();
        Assert.Equal(42L, result.Scalar<long>());
    }

    [Fact]
    public async Task ParameterGetter_CanDisposeConnectionWithoutDeadlock()
    {
        var t = new TestDb();
        var pending = Task.Run(() => t.Connection.Query("RETURN $Value",
            new CallbackParameters(() => { t.Connection.Dispose(); return 1; })));
        await Assert.ThrowsAsync<ObjectDisposedException>(() => pending.WaitAsync(TimeSpan.FromSeconds(10)));
        t.Dispose();
    }

    [Fact]
    public async Task Connect_RacingDispose_DoesNotLeaveLiveConnections()
    {
        for (int i = 0; i < 100; i++)
        {
            using var db = RelateDatabase.OpenInMemory();
            using var start = new ManualResetEventSlim();
            var connect = Task.Run(() =>
            {
                start.Wait();
                try { return db.Connect(); }
                catch (ObjectDisposedException) { return null; }
            });
            var dispose = Task.Run(() => { start.Wait(); db.Dispose(); });
            start.Set();
            await Task.WhenAll(connect, dispose);
            var connection = await connect;
            Assert.True(connection is null || connection.IsDisposed);
        }
    }

    private sealed class CallbackParameters(Func<long> callback)
    {
        public long Value => callback();
    }

    private sealed class BlockingParameters : IDisposable
    {
        internal TaskCompletionSource Entered { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal ManualResetEventSlim Release { get; } = new();
        public int Value
        {
            get
            {
                Entered.TrySetResult();
                if (!Release.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException("Test gate was not released.");
                return 1;
            }
        }
        public void Dispose() => Release.Dispose();
    }
}
