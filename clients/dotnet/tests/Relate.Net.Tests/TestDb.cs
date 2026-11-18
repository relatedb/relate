namespace Relate.Net.Tests;

/// <summary>An arbitrary enum shared by the parameter and result conversion tests.</summary>
public enum Color
{
    Red,
    Green,
}


/// <summary>A fresh in-memory database plus one connection, disposed together.</summary>
internal sealed class TestDb : IDisposable
{
    public TestDb()
    {
        Database = RelateDatabase.OpenInMemory();
        Connection = Database.Connect();
    }

    public RelateDatabase Database { get; }

    public RelateConnection Connection { get; }

    /// <summary>Sends <paramref name="value"/> as <c>$v</c> and returns what comes back.</summary>
    public object? RoundTrip(object? value) => Connection.Query("RETURN $v AS v", new { v = value })[0]["v"];

    public void Dispose() => Database.Dispose();
}

/// <summary>Creates and deletes a temporary directory for on-disk databases.</summary>
internal sealed class TempDirectory : IDisposable
{
    public string Path { get; } = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "relate-net-tests-" + Guid.NewGuid().ToString("N"));

    public void Dispose()
    {
        try
        {
            if (Directory.Exists(Path))
            {
                Directory.Delete(Path, recursive: true);
            }
        }
        catch (IOException)
        {
            // Best effort.
        }
    }
}
