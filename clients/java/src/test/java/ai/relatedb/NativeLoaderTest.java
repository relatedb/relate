package ai.relatedb;

import java.io.InputStream;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.attribute.FileTime;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.stream.Stream;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import static org.junit.jupiter.api.Assertions.*;

public class NativeLoaderTest {
    @TempDir
    Path tmp;

    private URL fakeLibrary(String name, String contents) throws Exception {
        Path src = tmp.resolve(name);
        Files.writeString(src, contents);
        return src.toUri().toURL();
    }

    @Test
    void extractsIntoAVersionedContentAddressedDirectory() throws Exception {
        URL lib = fakeLibrary("src.dylib", "native bytes");
        Path root = tmp.resolve("cache");
        Path out = NativeLoader.extract(lib, NativePlatform.DARWIN_ARM64, root, "1.2.3");

        assertEquals("librelate_ffi.dylib", out.getFileName().toString());
        assertEquals(root.resolve("1.2.3"), out.getParent().getParent());
        assertTrue(out.getParent().getFileName().toString().matches("darwin-arm64-[0-9a-f]{16}"),
                out.toString());
        assertEquals("native bytes", Files.readString(out));
    }

    @Test
    void reExtractionReusesTheCachedFile() throws Exception {
        URL lib = fakeLibrary("src.so", "native bytes");
        Path root = tmp.resolve("cache");
        Path first = NativeLoader.extract(lib, NativePlatform.LINUX_X64, root, "1.0.0");
        FileTime stamp = FileTime.fromMillis(1_000_000L);
        Files.setLastModifiedTime(first, stamp);

        Path second = NativeLoader.extract(lib, NativePlatform.LINUX_X64, root, "1.0.0");
        assertEquals(first, second);
        assertEquals(stamp, Files.getLastModifiedTime(second), "the cached file was not rewritten");
    }

    @Test
    void changedBytesUnderOneVersionGetTheirOwnDirectory() throws Exception {
        // A -SNAPSHOT rebuild keeps the version but changes the library.
        Path root = tmp.resolve("cache");
        Path a = NativeLoader.extract(fakeLibrary("a.so", "build one"), NativePlatform.LINUX_X64, root, "1.0.0-SNAPSHOT");
        Path b = NativeLoader.extract(fakeLibrary("b.so", "build two"), NativePlatform.LINUX_X64, root, "1.0.0-SNAPSHOT");
        assertNotEquals(a, b);
        assertEquals("build one", Files.readString(a));
        assertEquals("build two", Files.readString(b));
    }

    @Test
    void concurrentExtractionsAgreeAndLeaveNoTempFiles() throws Exception {
        URL lib = fakeLibrary("src.dll", "x".repeat(1 << 20));
        Path root = tmp.resolve("cache");
        int threads = 8;
        CountDownLatch start = new CountDownLatch(1);
        ExecutorService pool = Executors.newFixedThreadPool(threads);
        try {
            List<Future<Path>> results = new ArrayList<>();
            for (int i = 0; i < threads; i++) {
                results.add(pool.submit(() -> {
                    start.await();
                    return NativeLoader.extract(lib, NativePlatform.WIN32_X64, root, "2.0.0");
                }));
            }
            start.countDown();
            Path expected = results.get(0).get();
            for (Future<Path> f : results) {
                assertEquals(expected, f.get());
            }
            assertEquals(1 << 20, Files.size(expected));
            try (Stream<Path> files = Files.list(expected.getParent())) {
                assertEquals(List.of(expected), files.toList(), "only the library remains");
            }
        } finally {
            pool.shutdownNow();
        }
    }

    @Test
    void cacheRootPropertyWins() {
        Path root = NativeLoader.cacheRoot(NativePlatform.LINUX_X64, "/opt/cl-cache", "/home/u", name -> null);
        assertEquals(Paths.get("/opt/cl-cache"), root);
    }

    @Test
    void cacheRootFollowsEachPlatformsConvention() {
        Map<String, String> env = Map.of("XDG_CACHE_HOME", "/xdg", "LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local");
        assertEquals(Paths.get("/Users/u/Library/Caches/relate/native"),
                NativeLoader.cacheRoot(NativePlatform.DARWIN_ARM64, null, "/Users/u", env::get));
        assertEquals(Paths.get("/xdg/relate/native"),
                NativeLoader.cacheRoot(NativePlatform.LINUX_ARM64, null, "/home/u", env::get));
        assertEquals(Paths.get("/home/u/.cache/relate/native"),
                NativeLoader.cacheRoot(NativePlatform.LINUX_X64, null, "/home/u", name -> null));
        // XDG Base Directory spec: a relative XDG_CACHE_HOME is invalid and ignored.
        assertEquals(Paths.get("/home/u/.cache/relate/native"),
                NativeLoader.cacheRoot(NativePlatform.LINUX_X64, null, "/home/u", name -> "relative"));
        assertEquals(Paths.get("C:\\Users\\u\\AppData\\Local", "relate", "native"),
                NativeLoader.cacheRoot(NativePlatform.WIN32_X64, null, "C:\\Users\\u", env::get));
    }

    @Test
    void cacheRootWithoutAHomeFailsLoudlyNamingTheProperty() {
        RelateException noHome = assertThrows(RelateException.class,
                () -> NativeLoader.cacheRoot(NativePlatform.DARWIN_X64, null, "?", name -> null));
        assertTrue(noHome.getMessage().contains("-Drelate.native.cache"), noHome.getMessage());
        RelateException noLocalAppData = assertThrows(RelateException.class,
                () -> NativeLoader.cacheRoot(NativePlatform.WIN32_X64, null, "C:\\Users\\u", name -> null));
        assertTrue(noLocalAppData.getMessage().contains("-Drelate.native.cache"), noLocalAppData.getMessage());
    }

    @Test
    void missingNativeErrorNamesPlatformAndClassifier() {
        String msg = NativeLoader.missingNativeMessage(NativePlatform.LINUX_ARM64, "9.9.9");
        assertTrue(msg.contains("platform linux-arm64"), msg);
        assertTrue(msg.contains("/native/linux-arm64/librelate_ffi.so"), msg);
        assertTrue(msg.contains("ai.relatedb:relate:9.9.9:linux-aarch_64"), msg);
        assertTrue(msg.contains("RELATE_LIB"), msg);
    }

    @Test
    void versionIsBuildStamped() {
        String version = NativeLoader.version();
        assertFalse(version.isEmpty());
        assertFalse(version.contains("${"), version);
    }

    @Test
    void overridePathResolvesRelativeAgainstTheWorkingDirectory() throws Exception {
        Path lib = tmp.resolve("librelate_ffi.dylib");
        Files.writeString(lib, "native bytes");
        Path cwd = Path.of("").toAbsolutePath();
        String relative = cwd.relativize(lib).toString();
        // A value that JNA would mangle into a library *name* resolves to
        // the absolute path of the file.
        assertEquals(lib.toAbsolutePath().normalize().toString(),
                NativeLoader.overridePath(relative));
        assertEquals(lib.toAbsolutePath().normalize().toString(),
                NativeLoader.overridePath(lib.toString()));
    }

    @Test
    void overridePathFailsLoudOnAMissingFile() {
        RelateException e = assertThrows(RelateException.class,
                () -> NativeLoader.overridePath("no/such/librelate_ffi.dylib"));
        assertTrue(e.getMessage().contains("RELATE_LIB"), e.getMessage());
        assertTrue(e.getMessage().contains("cargo build --release -p relate-ffi"), e.getMessage());
    }

    @Test
    void theBuildBundlesTheHostNative() {
        // The host-native (local) or staged-natives (release) build profile
        // places this platform's library on the classpath.
        assertNotNull(getClass().getClassLoader().getResource(NativePlatform.current().resourcePath()),
                "missing " + NativePlatform.current().resourcePath());
    }

    @Test
    void theLibrarySkillIsAClasspathResource() throws Exception {
        URL skill = getClass().getClassLoader().getResource(".agents/skills/relate/SKILL.md");
        assertNotNull(skill, "library skill missing");
        String text;
        try (InputStream in = skill.openStream()) {
            text = new String(in.readAllBytes(), StandardCharsets.UTF_8);
        }
        assertTrue(text.startsWith("---\nname: relate\n"), text.substring(0, 40));
        assertTrue(text.contains("import ai.relatedb.Database;"));
    }
}
