package ai.relatedb;

import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.List;
import java.util.Map;
import java.util.stream.Collectors;
import java.util.stream.Stream;
import java.util.zip.ZipEntry;
import ai.relatedb.examples.BundledNative;
import ai.relatedb.examples.RelateE2E;
import java.util.Objects;
import java.util.zip.ZipFile;
import java.util.zip.ZipOutputStream;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Runs against the jars `mvn package` built (ADR-0490): their listings, and
 * child JVMs that load the bundled native with no RELATE_LIB, no
 * java.library.path and no loader-path environment.
 */
public class PackagedJarIT {
    private static final String SKILL = ".agents/skills/relate/SKILL.md";
    private static final Path BUILD_DIR = Paths.get(requiredProperty("relate.it.buildDir"));
    private static final String FINAL_NAME = requiredProperty("relate.it.finalName");
    private static final NativesMode MODE =
            NativesMode.parse(requiredProperty("relate.it.nativesMode"));

    /** "staged" (all five natives, release) or "host" (the build host's only). */
    private enum NativesMode {
        STAGED, HOST;

        static NativesMode parse(String value) {
            return switch (value) {
                case "staged" -> STAGED;
                case "host" -> HOST;
                default -> throw new IllegalStateException("relate.it.nativesMode is " + value);
            };
        }
    }

    @TempDir
    Path tmp;

    private static Path fatJar() {
        return BUILD_DIR.resolve(FINAL_NAME + ".jar");
    }

    private static Path classifierJar(NativePlatform platform) {
        return BUILD_DIR.resolve(FINAL_NAME + "-" + platform.classifier() + ".jar");
    }

    /** The platforms whose natives this build bundles. */
    private static List<NativePlatform> bundledPlatforms() {
        return switch (MODE) {
            case STAGED -> List.of(NativePlatform.values());
            case HOST -> List.of(NativePlatform.current());
        };
    }

    /** The jar's entry names, in archive order — the one listing helper both call sites share. */
    private static List<String> entryNames(ZipFile zip) {
        return zip.stream().map(ZipEntry::getName).collect(Collectors.toList());
    }

    private static List<String> entries(Path jar) throws IOException {
        try (ZipFile zip = new ZipFile(jar.toFile())) {
            return entryNames(zip);
        }
    }

    private static List<String> nativeEntries(List<String> entries) {
        return entries.stream()
                .filter(n -> n.startsWith(NativePlatform.RESOURCE_DIR) && !n.endsWith("/"))
                .sorted()
                .collect(Collectors.toList());
    }

    @Test
    void fatJarCarriesSkillVersionStampAndEveryBundledNative() throws IOException {
        List<String> entries = entries(fatJar());
        assertTrue(entries.contains(SKILL), "skill missing from " + entries);
        assertTrue(entries.contains("ai/relatedb/relate.properties"));
        assertTrue(entries.contains("ai/relatedb/Database.class"));
        List<String> expected = bundledPlatforms().stream()
                .map(NativePlatform::resourcePath).sorted().collect(Collectors.toList());
        assertEquals(expected, nativeEntries(entries));
    }

    @Test
    void eachClassifierJarCarriesExactlyItsOwnNative() throws IOException {
        for (NativePlatform platform : bundledPlatforms()) {
            Path jar = classifierJar(platform);
            assertTrue(Files.isRegularFile(jar), "missing classifier jar " + jar);
            List<String> entries = entries(jar);
            assertTrue(entries.contains(SKILL), jar + " lacks the skill");
            assertTrue(entries.contains("ai/relatedb/Database.class"), jar + " lacks the classes");
            assertEquals(List.of(platform.resourcePath()), nativeEntries(entries), jar.toString());
        }
    }

    @Test
    void fatJarLoadsTheBundledNative() throws Exception {
        Path cache = tmp.resolve("cache");
        Run run = start(fatJar(), cache).finish();
        assertEquals(0, run.exit(), run.output());
        assertTrue(run.output().contains(BundledNative.OK), run.output());
        assertEquals(1, extractedLibraries(cache).size(), "the native was extracted into the cache");
    }

    @Test
    void theEndToEndExampleRunsOffThePackagedJar() throws Exception {
        // The JBang-shaped docs example (RelateE2E): queries, graph
        // entities, a date property and an explicit transaction, driven
        // entirely by the jar under test in a clean child JVM.
        Run run = start(fatJar(), tmp.resolve("cache"), RelateE2E.class).finish();
        assertEquals(0, run.exit(), run.output());
        assertTrue(run.output().contains(RelateE2E.OK), run.output());
    }

    @Test
    void classifierJarLoadsTheBundledNative() throws Exception {
        Path cache = tmp.resolve("cache");
        Run run = start(classifierJar(NativePlatform.current()), cache).finish();
        assertEquals(0, run.exit(), run.output());
        assertTrue(run.output().contains(BundledNative.OK), run.output());
    }

    @Test
    void twoConcurrentJvmsShareOneExtraction() throws Exception {
        Path cache = tmp.resolve("cache");
        Child a = start(fatJar(), cache);
        Child b = start(fatJar(), cache);
        Run ra = a.finish();
        Run rb = b.finish();
        assertEquals(0, ra.exit(), ra.output());
        assertEquals(0, rb.exit(), rb.output());
        List<Path> files = extractedLibraries(cache);
        assertEquals(1, files.size(), "one library, no temp files: " + files);
        assertEquals(NativePlatform.current().libraryFileName(), files.get(0).getFileName().toString());
        // A third JVM reuses the cached file rather than rewriting it.
        long before = Files.getLastModifiedTime(files.get(0)).toMillis();
        assertEquals(0, start(fatJar(), cache).finish().exit());
        assertEquals(before, Files.getLastModifiedTime(files.get(0)).toMillis());
    }

    @Test
    void aClasspathWithoutThisPlatformsNativeFailsNamingTheClassifier() throws Exception {
        // The fat jar minus native/** stands in for a wrong-platform
        // classifier jar.
        Path stripped = tmp.resolve("stripped.jar");
        try (ZipFile in = new ZipFile(fatJar().toFile());
             ZipOutputStream out = new ZipOutputStream(Files.newOutputStream(stripped))) {
            for (String name : entryNames(in)) {
                if (name.startsWith(NativePlatform.RESOURCE_DIR)) {
                    continue;
                }
                out.putNextEntry(new ZipEntry(name));
                try (InputStream data = in.getInputStream(in.getEntry(name))) {
                    data.transferTo(out);
                }
                out.closeEntry();
            }
        }
        Run run = start(stripped, tmp.resolve("cache")).finish();
        NativePlatform platform = NativePlatform.current();
        assertNotEquals(0, run.exit(), run.output());
        assertTrue(run.output().contains("no bundled native library for platform " + platform.key()), run.output());
        assertTrue(run.output().contains(":" + platform.classifier()), run.output());
    }

    private static List<Path> extractedLibraries(Path cache) throws IOException {
        if (!Files.isDirectory(cache)) {
            return List.of();
        }
        try (Stream<Path> walk = Files.walk(cache)) {
            return walk.filter(Files::isRegularFile).collect(Collectors.toList());
        }
    }

    /** A child JVM running the BundledNative docs example on {@code jar}. */
    private static Child start(Path jar, Path cache) throws IOException {
        return start(jar, cache, BundledNative.class);
    }

    /** A child JVM running one of the docs examples' mains on {@code jar} — the type owns the link to the class. */
    private static Child start(Path jar, Path cache, Class<?> mainClass) throws IOException {
        String classpath = String.join(File.pathSeparator,
                jar.toString(),
                requiredProperty("relate.it.runtimeClasspath"),
                requiredProperty("relate.it.testClasses"));
        ProcessBuilder pb = new ProcessBuilder(
                Paths.get(System.getProperty("java.home"), "bin", "java").toString(),
                "-Djava.library.path=",
                "-Drelate.native.cache=" + cache,
                "-cp", classpath,
                mainClass.getName());
        Map<String, String> env = pb.environment();
        env.remove("RELATE_LIB");
        env.remove("DYLD_LIBRARY_PATH");
        env.remove("LD_LIBRARY_PATH");
        pb.redirectErrorStream(true);
        return new Child(pb.start());
    }

    /** The harness's required inputs are an explicit, loud boundary — never a silent "null" classpath entry. */
    private static String requiredProperty(String name) {
        return Objects.requireNonNull(System.getProperty(name),
                name + " must be set — run the IT through mvn verify, which configures it");
    }

    private record Child(Process process) {
        Run finish() throws IOException, InterruptedException {
            process.getOutputStream().close(); // the example reads no stdin
            String output = new String(process.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
            return new Run(process.waitFor(), output);
        }
    }

    private record Run(int exit, String output) {
    }
}
