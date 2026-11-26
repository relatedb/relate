using Relate.Net.Interop;
using static Relate.Net.Interop.NativeMethods;

namespace Relate.Net;

/// <summary>
/// A session on a <see cref="RelateDatabase"/> that runs Cypher statements, optionally inside an
/// explicit <see cref="RelateTransaction"/>.
/// </summary>
/// <remarks>
/// A connection is safe to share between threads: calls are serialized (one statement at a time).
/// For parallelism open one connection per worker. Disposing the connection rolls back an open
/// transaction.
/// </remarks>
public sealed class RelateConnection : IDisposable, IAsyncDisposable
{
    private readonly ConnectionHandle _handle;
    private readonly SemaphoreSlim _gate = new(1, 1);
    private string _namespace;
    private volatile RelateTransaction? _transaction;
    private volatile bool _disposed;

    internal RelateConnection(RelateDatabase database, ConnectionHandle handle, string @namespace)
    {
        Database = database;
        _handle = handle;
        _namespace = @namespace;
    }

    /// <summary>The database this connection belongs to.</summary>
    public RelateDatabase Database { get; }

    /// <summary>The session namespace used by statements without a <c>USE</c> clause.</summary>
    public string Namespace => _namespace;

    /// <summary>The open explicit transaction, or <see langword="null"/>.</summary>
    public RelateTransaction? CurrentTransaction => _transaction;

    /// <summary><see langword="true"/> once the connection has been disposed.</summary>
    public bool IsDisposed => _disposed;

    /// <summary>
    /// Runs a Cypher statement and returns its fully materialized result.
    /// </summary>
    /// <param name="cypher">The Cypher statement.</param>
    /// <param name="parameters">
    /// Named parameters (<c>$name</c>): any <c>IDictionary</c> / <c>IReadOnlyDictionary</c> with
    /// string keys, an anonymous object, or any object whose public properties become parameters.
    /// </param>
    /// <exception cref="RelateException">The statement failed (syntax, runtime or engine error).</exception>
    /// <exception cref="ArgumentException">A parameter value cannot be sent to relate.</exception>
    /// <exception cref="ObjectDisposedException">The connection has been disposed.</exception>
    public QueryResult Query(string cypher, object? parameters = null)
    {
        Native.ValidateCString(cypher, nameof(cypher));
        return QueryPrepared(cypher, parameters, _transaction);
    }

    /// <summary>
    /// Runs a Cypher statement on a thread-pool thread. Cancellation is honoured until the statement
    /// starts; a running statement completes.
    /// </summary>
    /// <inheritdoc cref="Query" />
    public Task<QueryResult> QueryAsync(string cypher, object? parameters = null, CancellationToken cancellationToken = default)
    {
        Native.ValidateCString(cypher, nameof(cypher));
        return QueryPreparedAsync(cypher, parameters, _transaction, cancellationToken);
    }

    /// <summary>
    /// Changes the session namespace (<c>neo4j</c>, <c>system</c> or <c>system_ontologies</c>).
    /// Not allowed while a transaction is open.
    /// </summary>
    /// <exception cref="RelateException">The namespace does not exist or a transaction is open.</exception>
    public unsafe void ChangeNamespace(string? @namespace)
    {
        // Null resolves to the default, as Connect does; an empty string defers to the engine's
        // 42N00 — one resolution and validation policy for both session-namespace writers.
        @namespace ??= RelateDatabase.DefaultNamespace;
        Native.ValidateCString(@namespace, nameof(@namespace));
        Exclusive(() =>
        {
            nint err = 0;
            Native.Check(relate_conn_set_namespace(_handle, @namespace, &err), err, "set namespace");
            _namespace = NormalizeNamespace(@namespace);
        });
    }

    /// <summary>
    /// Bounds each following statement on this connection to <paramref name="timeout"/> (ADR-0692).
    /// A statement still running past it is cancelled and the call throws a <see cref="RelateException"/>
    /// whose code is <c>Neo.ClientError.Transaction.TransactionTimedOutClientConfiguration</c>; the
    /// connection stays usable. <see cref="TimeSpan.Zero"/> clears the bound (the default: none).
    /// </summary>
    /// <exception cref="ArgumentOutOfRangeException">The timeout is negative.</exception>
    public unsafe void SetStatementTimeout(TimeSpan timeout)
    {
        ArgumentOutOfRangeException.ThrowIfLessThan(timeout, TimeSpan.Zero);
        var milliseconds = checked((ulong)Math.Ceiling(timeout.TotalMilliseconds));
        Exclusive(() =>
        {
            nint err = 0;
            Native.Check(relate_conn_set_statement_timeout(_handle, milliseconds, &err), err, "set statement timeout");
        });
    }

    /// <summary>The session namespace in its stored form (lowercase); the one normalization home.</summary>
    internal static string NormalizeNamespace(string @namespace) => @namespace.ToLowerInvariant();

    /// <summary>
    /// Begins an explicit transaction. Statements run on this connection (or through the returned
    /// transaction) belong to it until it is committed, rolled back or disposed.
    /// </summary>
    /// <exception cref="InvalidOperationException">A transaction is already open on this connection.</exception>
    public RelateTransaction BeginTransaction() => Exclusive(BeginCore);

    /// <inheritdoc cref="BeginTransaction" />
    public Task<RelateTransaction> BeginTransactionAsync(CancellationToken cancellationToken = default) =>
        ExclusiveAsync(BeginCore, cancellationToken);

    /// <summary>Closes the connection, rolling back any open transaction.</summary>
    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _gate.Wait();
        try
        {
            if (_disposed)
            {
                return;
            }

            _disposed = true;
            _transaction = null;
            _handle.Dispose();
        }
        finally
        {
            _gate.Release();
            // The registry removal always runs, so a throwing teardown does not leave a disposed
            // connection registered against its database.
            Database.Forget(this);
        }
    }

    /// <inheritdoc cref="Dispose" />
    public async ValueTask DisposeAsync()
    {
        await Task.Run(Dispose).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public override string ToString() => $"RelateConnection({Database.Location}, {Namespace})";

    internal QueryResult QueryTransaction(RelateTransaction transaction, string cypher, object? parameters)
    {
        Native.ValidateCString(cypher, nameof(cypher));
        return QueryPrepared(cypher, parameters, transaction);
    }

    internal Task<QueryResult> QueryTransactionAsync(RelateTransaction transaction, string cypher, object? parameters, CancellationToken cancellationToken)
    {
        Native.ValidateCString(cypher, nameof(cypher));
        return QueryPreparedAsync(cypher, parameters, transaction, cancellationToken);
    }

    internal Bookmark? Commit(RelateTransaction transaction) => Exclusive(() => CommitCore(transaction));

    private unsafe Bookmark? CommitCore(RelateTransaction transaction)
    {
        EnsureCurrent(transaction);
        nint bookmark = 0;
        nint err = 0;
        RelateStatus status = relate_conn_commit(_handle, &bookmark, &err);
        string? text = Native.TakeString(bookmark);
        RelateException? commitFailure = Native.Error(status, err, "commit");
        if (commitFailure is null)
        {
            _transaction = null;
            return ResultMetadataReader.ReadBookmark(text);
        }

        // The commit failed: compensate with a rollback so no half-open native transaction
        // survives, then surface both failures. The reference is held until both statuses are
        // known, so a failed rollback is reported instead of silently forgotten.
        nint rollbackErr = 0;
        RelateStatus rollbackStatus = relate_conn_rollback(_handle, &rollbackErr);
        RelateException? rollbackFailure = Native.Error(rollbackStatus, rollbackErr, "rollback after a failed commit");
        if (rollbackFailure is not null)
        {
            // Same policy as RollbackCore: the reference stays set when the rollback did not
            // complete, so Dispose or a retry can address the still-open native transaction.
            throw new RelateException(
                $"{commitFailure.Message} The compensating rollback failed as well: {rollbackFailure.Message}",
                commitFailure.Code, commitFailure.GqlStatus, commitFailure.Stage, commitFailure.IsRetryable,
                innerException: commitFailure);
        }

        _transaction = null;
        throw commitFailure;
    }

    internal Task<Bookmark?> CommitAsync(RelateTransaction transaction, CancellationToken cancellationToken) =>
        ExclusiveAsync(() => CommitCore(transaction), cancellationToken);

    internal void Rollback(RelateTransaction transaction) => Exclusive(() => RollbackCore(transaction));

    private unsafe void RollbackCore(RelateTransaction transaction)
    {
        EnsureCurrent(transaction);
        nint err = 0;
        RelateStatus status = relate_conn_rollback(_handle, &err);
        RelateException? failure = Native.Error(status, err, "rollback");
        if (failure is not null)
        {
            // The reference stays set when the rollback fails, so Dispose or a retry can still
            // address the open native transaction (mirrors CommitCore's ordering).
            throw failure;
        }

        _transaction = null;
    }

    internal Task RollbackAsync(RelateTransaction transaction, CancellationToken cancellationToken) =>
        ExclusiveAsync(() => RollbackCore(transaction), cancellationToken);

    private unsafe RelateTransaction BeginCore()
    {
        if (_transaction is not null)
        {
            throw new InvalidOperationException("A transaction is already open on this connection; commit or roll it back first.");
        }

        nint err = 0;
        Native.Check(relate_conn_begin(_handle, &err), err, "begin");
        _transaction = new RelateTransaction(this);
        return _transaction;
    }

    private QueryResult QueryPrepared(string cypher, object? parameters, RelateTransaction? transaction)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        // Getters and enumerators are user code: never run them under the connection gate.
        using var builder = ParameterWriter.Build(parameters);
        return Exclusive(() => QueryCore(cypher, builder, transaction));
    }

    private async Task<QueryResult> QueryPreparedAsync(string cypher, object? parameters, RelateTransaction? transaction, CancellationToken cancellationToken)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        using var builder = await Task.Run(() => ParameterWriter.Build(parameters), cancellationToken).ConfigureAwait(false);
        return await ExclusiveAsync(() => QueryCore(cypher, builder, transaction), cancellationToken).ConfigureAwait(false);
    }

    private unsafe QueryResult QueryCore(string cypher, ParametersHandle builder, RelateTransaction? transaction)
    {
        EnsureCurrent(transaction);
        nint err = 0;
        RelateStatus status = relate_query_params(_handle, cypher, builder, out nint result, &err);
        Native.Check(status, err, "query");
        return ResultReader.Read(result);
    }

    private void EnsureCurrent(RelateTransaction? transaction)
    {
        if (!ReferenceEquals(_transaction, transaction))
        {
            throw new InvalidOperationException("The connection transaction changed before this operation could run.");
        }
    }

    private void Exclusive(Action action) => Exclusive(() =>
    {
        action();
        return true;
    });

    private T Exclusive<T>(Func<T> action)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        _gate.Wait();
        try
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            return action();
        }
        finally
        {
            _gate.Release();
        }
    }

    private Task ExclusiveAsync(Action action, CancellationToken cancellationToken) =>
        ExclusiveAsync(
            () =>
            {
                action();
                return true;
            },
            cancellationToken);

    private async Task<T> ExclusiveAsync<T>(Func<T> action, CancellationToken cancellationToken)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        await _gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            cancellationToken.ThrowIfCancellationRequested();
            return await Task.Run(action, cancellationToken).ConfigureAwait(false);
        }
        finally
        {
            _gate.Release();
        }
    }
}
