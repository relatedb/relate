using System.Collections.Concurrent;
using Relate.Net.Interop;
using static Relate.Net.Interop.NativeMethods;

namespace Relate.Net;

/// <summary>
/// An open relate database: an in-memory store (<see cref="InMemory"/>), a native store directory
/// or a <c>file://</c> URI. The database is thread-safe; open <see cref="RelateConnection"/>s from
/// it to run Cypher. Disposing the database disposes every connection opened from it.
/// </summary>
/// <example>
/// <code>
/// using var db = RelateDatabase.OpenInMemory();
/// using var conn = db.Connect();
/// var result = conn.Query("RETURN $a + $b AS sum", new { a = 1, b = 2 });
/// long sum = result.Scalar&lt;long&gt;();
/// </code>
/// </example>
public sealed class RelateDatabase : IDisposable, IAsyncDisposable
{
    /// <summary>The location string that opens a fresh in-memory database.</summary>
    public const string InMemory = ":memory:";

    /// <summary>The default session namespace.</summary>
    public const string DefaultNamespace = "neo4j";

    private readonly DatabaseHandle _handle;
    private readonly ConcurrentDictionary<RelateConnection, byte> _connections = new();
    private int _disposed;
    private readonly object _lifecycle = new();

    private RelateDatabase(DatabaseHandle handle, string location, bool isReadOnly)
    {
        _handle = handle;
        Location = location;
        IsReadOnly = isReadOnly;
    }

    /// <summary>The C ABI version of the loaded relate native library.</summary>
    /// <exception cref="DllNotFoundException">The native library could not be loaded.</exception>
    public static int NativeAbiVersion => Native.AbiVersion;

    /// <summary>The location the database was opened from.</summary>
    public string Location { get; }

    /// <summary><see langword="true"/> when opened with <see cref="OpenReadOnly"/>.</summary>
    public bool IsReadOnly { get; }

    /// <summary><see langword="true"/> once the database has been disposed.</summary>
    public bool IsDisposed => Volatile.Read(ref _disposed) != 0;

    /// <summary>
    /// Opens (creating if needed) a database at <paramref name="location"/>: <see cref="InMemory"/>,
    /// a native store directory, or a <c>file://…</c> / <c>*.cl</c> URI.
    /// </summary>
    /// <exception cref="RelateException">The database could not be opened.</exception>
    public static RelateDatabase Open(string location) => OpenCore(location, isReadOnly: false);

    /// <summary>Opens a fresh, empty in-memory database.</summary>
    public static RelateDatabase OpenInMemory() => Open(InMemory);

    /// <summary>
    /// Opens an existing database without writing, checkpointing or creating a missing store.
    /// Reads succeed; writes and <see cref="Checkpoint"/> fail with a <see cref="RelateException"/>.
    /// </summary>
    /// <exception cref="RelateException">The database could not be opened (e.g. it does not exist).</exception>
    public static RelateDatabase OpenReadOnly(string location) => OpenCore(location, isReadOnly: true);

    /// <summary>The one open path: validate, load, call the read-only or read-write native entry point.</summary>
    private static unsafe RelateDatabase OpenCore(string location, bool isReadOnly)
    {
        ArgumentException.ThrowIfNullOrEmpty(location);
        Native.ValidateCString(location, nameof(location));
        Native.EnsureLoaded();
        nint err = 0;
        RelateStatus status;
        DatabaseHandle handle;
        if (isReadOnly)
        {
            status = relate_database_open_read_only(location, out handle, &err);
        }
        else
        {
            status = relate_database_open(location, out handle, &err);
        }

        return Opened(status, handle, err, location, isReadOnly);
    }

    /// <summary>Opens a database on a thread-pool thread; see <see cref="Open"/>.</summary>
    public static Task<RelateDatabase> OpenAsync(string location, CancellationToken cancellationToken = default)
    {
        ArgumentException.ThrowIfNullOrEmpty(location);
        Native.ValidateCString(location, nameof(location));
        return Task.Run(() => Open(location), cancellationToken);
    }

    /// <summary>
    /// Opens a connection. <paramref name="namespace"/> is the session namespace for statements
    /// without a <c>USE</c> clause: <c>neo4j</c> (default), <c>system</c> or
    /// <c>system_ontologies</c>, matched case-insensitively.
    /// </summary>
    /// <exception cref="RelateException">The namespace does not exist.</exception>
    /// <exception cref="ObjectDisposedException">The database has been disposed.</exception>
    public unsafe RelateConnection Connect(string? @namespace = null)
    {
        // One resolution decides both the engine's session namespace and the reported one.
        string resolved = @namespace ?? DefaultNamespace;
        Native.ValidateCString(resolved, nameof(@namespace));

        lock (_lifecycle)
        {
            ThrowIfDisposed();
            nint err = 0;
            RelateStatus status = relate_connect_namespace(_handle, resolved, out ConnectionHandle handle, &err);
            try
            {
                Native.Check(status, err, "connect");
                handle.AttachTo(_handle);
                var connection = new RelateConnection(this, handle, RelateConnection.NormalizeNamespace(resolved));
                _connections.TryAdd(connection, 0);
                return connection;
            }
            catch
            {
                handle.Dispose();
                throw;
            }
        }
    }

    /// <summary>Flushes the database to its store (a no-op for in-memory databases).</summary>
    /// <exception cref="RelateException">The checkpoint failed, e.g. because the database is read-only.</exception>
    public unsafe void Checkpoint()
    {
        ThrowIfDisposed();
        nint err = 0;
        RelateStatus status = relate_checkpoint(_handle, &err);
        Native.Check(status, err, "checkpoint");
    }

    /// <summary>Runs <see cref="Checkpoint"/> on a thread-pool thread.</summary>
    public Task CheckpointAsync(CancellationToken cancellationToken = default) => Task.Run(Checkpoint, cancellationToken);

    /// <summary>Disposes every open connection, then closes the database.</summary>
    public void Dispose()
    {
        lock (_lifecycle)
        {
            if (Interlocked.Exchange(ref _disposed, 1) != 0)
            {
                return;
            }
        }

        // The flag is already published: the teardown must complete (close the database handle)
        // even when a connection release throws, so a failure never leaves a half-open database.
        try
        {
            foreach (RelateConnection connection in _connections.Keys)
            {
                connection.Dispose();
            }
        }
        finally
        {
            _handle.Dispose();
        }
    }

    /// <inheritdoc cref="Dispose" />
    public async ValueTask DisposeAsync() => await Task.Run(Dispose).ConfigureAwait(false);

    /// <inheritdoc />
    public override string ToString() => $"RelateDatabase({Location}{(IsReadOnly ? ", read-only" : string.Empty)})";

    internal void Forget(RelateConnection connection) => _connections.TryRemove(connection, out _);

    private static RelateDatabase Opened(RelateStatus status, DatabaseHandle handle, nint err, string location, bool isReadOnly)
    {
        if (status != RelateStatus.Ok)
        {
            handle.Dispose();
        }

        Native.Check(status, err, "open");
        return new RelateDatabase(handle, location, isReadOnly);
    }

    private void ThrowIfDisposed() => ObjectDisposedException.ThrowIf(IsDisposed, this);
}
