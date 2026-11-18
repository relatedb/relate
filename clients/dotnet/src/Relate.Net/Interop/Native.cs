using System.Runtime.InteropServices;
using System.Text;

namespace Relate.Net.Interop;

/// <summary>Helpers shared by the interop layer: library checks, strings and error handling.</summary>
internal static unsafe class Native
{
    /// <summary>The C ABI major version this wrapper is written against.</summary>
    internal const int SupportedAbiVersion = 2;

    private static readonly Lazy<int> s_abiVersion = new(LoadAndCheck, LazyThreadSafetyMode.ExecutionAndPublication);

    internal static int AbiVersion => s_abiVersion.Value;

    /// <summary>Verifies once per process that the native library loads and speaks ABI v2.</summary>
    internal static void EnsureLoaded() => _ = s_abiVersion.Value;

    private static int LoadAndCheck()
    {
        int version;
        try
        {
            version = NativeMethods.relate_abi_version();
        }
        catch (DllNotFoundException ex)
        {
            throw new DllNotFoundException(
                $"The relate native library '{NativeMethods.LibraryName}' could not be loaded. " +
                "Build it from the matching Relate checkout and place it next to Relate.Net.dll, or supply the matching runtimes/<rid>/native asset.", ex);
        }

        if (version != SupportedAbiVersion)
        {
            throw new NotSupportedException(
                $"The loaded relate native library implements C ABI version {version}, but Relate.Net requires version {SupportedAbiVersion}.");
        }

        return version;
    }

    /// <summary>Rejects truncation at the C string boundary; string values use byte lengths.</summary>
    internal static void ValidateCString(string value, string parameterName)
    {
        ArgumentNullException.ThrowIfNull(value, parameterName);
        if (value.Contains('\0'))
        {
            throw new ArgumentException("Embedded NUL is not allowed in a C-string argument.", parameterName);
        }
    }

    internal static string RequiredString(nint value) => PtrToString(value)
        ?? throw new RelateException("The native library returned a missing required string.");

    internal static int Count(long count) => count is >= 0 and <= int.MaxValue
        ? (int)count
        : throw new RelateException("The native library returned an invalid collection length.");

    /// <summary>Reads a NUL-terminated UTF-8 string owned by relate (do not free).</summary>
    internal static string? PtrToString(nint p) => p == 0 ? null : Marshal.PtrToStringUTF8(p);

    /// <summary>Reads a UTF-8 string of a known byte length owned by relate (do not free).</summary>
    internal static string PtrToString(nint p, nuint len)
    {
        if (len == 0) return string.Empty;
        if (p == 0) throw new RelateException("The native library returned a missing string buffer.");
        if (len > int.MaxValue) throw new RelateException("The native library returned an invalid string length.");
        return Encoding.UTF8.GetString((byte*)p, (int)len);
    }

    /// <summary>Reads and frees a caller-owned string (<c>relate_free_string</c>).</summary>
    internal static string? TakeString(nint p)
    {
        if (p == 0)
        {
            return null;
        }

        try
        {
            return Marshal.PtrToStringUTF8(p);
        }
        finally
        {
            NativeMethods.relate_free_string(p);
        }
    }

    /// <summary>
    /// The one free-then-decode policy for a caller-owned error slot: frees the JSON, decodes it
    /// when the status failed, and returns the decoded failure — <see langword="null"/> on success.
    /// </summary>
    internal static RelateException? Error(RelateStatus status, nint errorJson, string operation)
    {
        string? json = TakeString(errorJson);
        return status != RelateStatus.Ok ? RelateException.FromErrorJson(json, operation) : null;
    }

    /// <summary>
    /// Throws the decoded <see cref="RelateException"/> when <paramref name="status"/> is an error;
    /// always frees the caller-owned <paramref name="errorJson"/>.
    /// </summary>
    internal static void Check(RelateStatus status, nint errorJson, string operation)
    {
        RelateException? failure = Error(status, errorJson, operation);
        if (failure is not null)
        {
            throw failure;
        }
    }
}
