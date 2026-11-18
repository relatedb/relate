namespace Relate.Net;

/// <summary>
/// An explicit transaction on a <see cref="RelateConnection"/>. Call <see cref="Commit"/> to make
/// its writes durable; disposing an uncommitted transaction rolls it back.
/// </summary>
/// <example>
/// <code>
/// using (var tx = conn.BeginTransaction())
/// {
///     tx.Query("CREATE (:Person {name: $name})", new { name = "Ada" });
///     Bookmark? bookmark = tx.Commit();
/// }
/// </code>
/// </example>
public sealed class RelateTransaction : IDisposable, IAsyncDisposable
{
    internal RelateTransaction(RelateConnection connection) => Connection = connection;

    /// <summary>The connection the transaction runs on.</summary>
    public RelateConnection Connection { get; }

    /// <summary><see langword="true"/> until the transaction is committed, rolled back or disposed.</summary>
    public bool IsActive => ReferenceEquals(Connection.CurrentTransaction, this);

    /// <summary>Runs a statement inside this transaction.</summary>
    /// <inheritdoc cref="RelateConnection.Query" />
    /// <exception cref="InvalidOperationException">The transaction is no longer active.</exception>
    public QueryResult Query(string cypher, object? parameters = null)
    {
        EnsureActive();
        return Connection.QueryTransaction(this, cypher, parameters);
    }

    /// <summary>Runs a statement inside this transaction on a thread-pool thread.</summary>
    /// <inheritdoc cref="RelateConnection.QueryAsync" />
    /// <exception cref="InvalidOperationException">The transaction is no longer active.</exception>
    public Task<QueryResult> QueryAsync(string cypher, object? parameters = null, CancellationToken cancellationToken = default)
    {
        EnsureActive();
        return Connection.QueryTransactionAsync(this, cypher, parameters, cancellationToken);
    }

    /// <summary>
    /// Commits the transaction. Returns its bookmark, or <see langword="null"/> when the
    /// transaction wrote nothing.
    /// </summary>
    /// <exception cref="RelateException">The commit failed; the transaction has been rolled back.</exception>
    /// <exception cref="InvalidOperationException">The transaction is no longer active.</exception>
    public Bookmark? Commit()
    {
        EnsureActive();
        return Connection.Commit(this);
    }

    /// <inheritdoc cref="Commit" />
    public Task<Bookmark?> CommitAsync(CancellationToken cancellationToken = default)
    {
        EnsureActive();
        return Connection.CommitAsync(this, cancellationToken);
    }

    /// <summary>Rolls the transaction back, discarding its writes.</summary>
    /// <exception cref="InvalidOperationException">The transaction is no longer active.</exception>
    public void Rollback()
    {
        EnsureActive();
        Connection.Rollback(this);
    }

    /// <inheritdoc cref="Rollback" />
    public Task RollbackAsync(CancellationToken cancellationToken = default)
    {
        EnsureActive();
        return Connection.RollbackAsync(this, cancellationToken);
    }

    /// <summary>Rolls the transaction back if it is still active. Never throws.</summary>
    public void Dispose()
    {
        if (!IsActive || Connection.IsDisposed)
        {
            return;
        }

        try
        {
            Connection.Rollback(this);
        }
        catch (Exception ex) when (ex is RelateException or ObjectDisposedException or InvalidOperationException)
        {
            // Disposal must not throw; the connection is either gone or the engine already ended the transaction.
        }
    }

    /// <inheritdoc cref="Dispose" />
    public async ValueTask DisposeAsync()
    {
        if (!IsActive || Connection.IsDisposed)
        {
            return;
        }

        try
        {
            await Connection.RollbackAsync(this, CancellationToken.None).ConfigureAwait(false);
        }
        catch (Exception ex) when (ex is RelateException or ObjectDisposedException or InvalidOperationException)
        {
            // See Dispose.
        }
    }

    private void EnsureActive()
    {
        if (!IsActive)
        {
            throw new InvalidOperationException("The transaction has already been committed or rolled back.");
        }
    }
}
