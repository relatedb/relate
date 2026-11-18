using Microsoft.Win32.SafeHandles;

namespace Relate.Net.Interop;

/// <summary>Owns a native <c>relate_database</c>; closes it on release.</summary>
internal sealed class DatabaseHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    public DatabaseHandle() : base(ownsHandle: true)
    {
    }

    protected override bool ReleaseHandle() => NativeMethods.relate_database_close(handle) == RelateStatus.Ok;
}

/// <summary>
/// Owns a native <c>relate_connection</c>; disconnects it on release. The connection holds a
/// reference on its database handle so the database is never closed underneath a live connection.
/// </summary>
internal sealed class ConnectionHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    private DatabaseHandle? _database;

    public ConnectionHandle() : base(ownsHandle: true)
    {
    }

    internal void AttachTo(DatabaseHandle database)
    {
        bool added = false;
        database.DangerousAddRef(ref added);
        if (!added)
        {
            // Without the reference the database can close underneath this connection, breaking
            // the class's ownership promise — fail instead of proceeding unowned.
            throw new ObjectDisposedException(nameof(RelateDatabase), "The database handle is already released.");
        }

        _database = database;
    }

    protected override bool ReleaseHandle()
    {
        bool disconnected = NativeMethods.relate_disconnect(handle) == RelateStatus.Ok;
        _database?.DangerousRelease();
        _database = null;
        return disconnected;
    }
}

/// <summary>Owns a complete native parameter tree, including when a queued query fails.</summary>
internal sealed class ParametersHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal ParametersHandle(nint value) : base(ownsHandle: true) => SetHandle(value);

    protected override bool ReleaseHandle()
    {
        NativeMethods.relate_params_free(handle);
        return true;
    }
}
