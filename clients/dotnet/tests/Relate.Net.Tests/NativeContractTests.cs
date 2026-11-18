using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.RegularExpressions;

namespace Relate.Net.Tests;

public class NativeContractTests
{
    [Fact]
    public void Bindings_CoverCurrentHeaderAndResolveInCurrentLibrary()
    {
        var assembly = typeof(RelateDatabase).Assembly;
        var bindings = assembly.GetType("Relate.Net.Interop.NativeMethods", throwOnError: true)!;
        var imports = bindings.GetMethods(BindingFlags.Static | BindingFlags.NonPublic)
            .Where(m => m.GetCustomAttribute<LibraryImportAttribute>() is not null)
            .Select(m => m.Name).OrderBy(n => n).ToArray();
        string header = File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "relate.h"));
        var declared = Regex.Matches(header,
            @"(?m)^(?:int|void|relate_status|relate_value|relate_params|const char \*|long long|uint8_t|double)\s*(relate_\w+)\(")
            .Select(m => m.Groups[1].Value).OrderBy(n => n).ToArray();
        Assert.NotEmpty(declared);
        Assert.Equal(declared, imports);
        string name = OperatingSystem.IsWindows() ? "relate_ffi.dll"
            : OperatingSystem.IsMacOS() ? "librelate_ffi.dylib" : "librelate_ffi.so";
        nint library = NativeLibrary.Load(Path.Combine(AppContext.BaseDirectory, name));
        try
        {
            foreach (string symbol in imports)
                Assert.True(NativeLibrary.TryGetExport(library, symbol, out _), $"Missing native symbol: {symbol}");
        }
        finally
        {
            NativeLibrary.Free(library);
        }
    }
}
