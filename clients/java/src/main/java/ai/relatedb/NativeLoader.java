package ai.relatedb;

import java.io.IOException;
import java.io.InputStream;
import java.net.URL;
import java.nio.channels.FileChannel;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.HexFormat;
import java.util.Properties;
import java.util.function.UnaryOperator;

/**
 * Resolves the {@code relate_ffi} library JNA loads (ADR-0490). Order:
 *
 * <ol>
 *   <li>{@code RELATE_LIB} — an explicit library path, a development
 *       escape hatch only;</li>
 *   <li>the bundled resource {@code /native/<key>/<library>} for the running
 *       platform (the fat jar carries all five, a classifier jar one),
 *       extracted once into a per-version cache directory;</li>
 *   <li>otherwise a {@link RelateException} naming the detected platform
 *       and the classifier artifact to add.</li>
 * </ol>
 *
 * The system library path is never searched.
 */
final class NativeLoader {
    /** The published Maven coordinate (groupId:artifactId), spelled once. */
    static final String COORDINATE = "ai.relatedb:relate";

    /** The system property that relocates the extraction cache root. */
    static final String CACHE_PROPERTY = "relate.native.cache";

    private static final String VERSION_RESOURCE = "ai/relatedb/relate.properties";
    /** Hex characters of the content digest that name a cache directory. */
    private static final int DIGEST_PREFIX = 16;

    /**
     * The JVM's in-band "unknown" spelling for {@code user.home}: a sandboxed
     * or headless JVM reports the literal {@code "?"} rather than leaving the
     * property unset. Treated as absent — never as a directory named {@code "?"}.
     */
    private static final String UNKNOWN_USER_HOME = "?";

    private NativeLoader() {
    }

    /**
     * The library path for {@code Native.load}, resolved in the documented
     * order. Postcondition of every branch: an absolute, normalised path to
     * a regular file — JNA treats a relative value as a library
     * <em>name</em> (it searches for {@code lib<value>.dylib}), so neither
     * the override nor the extracted bundled native may hand it one.
     */
    static String libraryPath() {
        String override = nonEmpty(System.getenv(NativePlatform.OVERRIDE_ENV));
        if (override != null) {
            return overridePath(override);
        }
        NativePlatform platform = NativePlatform.current();
        String version = version();
        URL resource = NativeLoader.class.getClassLoader().getResource(platform.resourcePath());
        if (resource == null) {
            throw new RelateException(missingNativeMessage(platform, version,
                    System.getProperty("os.name"), System.getProperty("os.arch")));
        }
        Path root = cacheRoot(platform, System.getProperty(CACHE_PROPERTY),
                System.getProperty("user.home"), System::getenv);
        try {
            Path extracted = extract(resource, platform, root, version);
            if (!Files.isRegularFile(extracted)) {
                throw new RelateException("relate: the extracted native library is not a "
                        + "regular file: " + extracted);
            }
            return extracted.toAbsolutePath().normalize().toString();
        } catch (IOException e) {
            throw new RelateException("relate: cannot extract the bundled native library "
                    + platform.resourcePath() + " into " + root + ": " + e + ". " + cacheHint(), e);
        }
    }

    /**
     * The {@code RELATE_LIB} override as an absolute, existing path —
     * the dev-hatch arm of {@link #libraryPath()}'s postcondition. JNA
     * treats a relative value as a library <em>name</em> (it searches for
     * {@code lib<value>.dylib}), so the override is resolved against the
     * process working directory first; a value pointing at no regular
     * file fails loud here instead of as an opaque JNA search error.
     */
    static String overridePath(String override) {
        Path path = Paths.get(override).toAbsolutePath().normalize();
        if (!Files.isRegularFile(path)) {
            throw new RelateException("relate: " + NativePlatform.OVERRIDE_ENV
                    + " points at no regular file: " + override + " (resolved to " + path
                    + "). Build the library first: cargo build --release -p relate-ffi.");
        }
        return path.toString();
    }

    /**
     * The loud error for a classpath carrying no native for this platform;
     * a pure function of its arguments (the detection facts are passed in).
     */
    static String missingNativeMessage(NativePlatform platform, String version,
                                       String osName, String osArch) {
        return "relate: no bundled native library for platform " + platform.key()
                + " (os.name=" + osName + ", os.arch="
                + osArch + "): the classpath has no /"
                + platform.resourcePath() + ". Depend on the all-platform jar"
                + " " + COORDINATE + ":" + version + " or add the classifier artifact"
                + " " + COORDINATE + ":" + version + ":" + platform.classifier()
                + ". (" + NativePlatform.OVERRIDE_ENV + "=<path to a relate_ffi you built> overrides the"
                + " bundled library for development only.)";
    }

    /**
     * The extraction cache root: the {@value #CACHE_PROPERTY} property when
     * set, else the platform's per-user cache directory plus
     * {@code relate/native}. The per-OS cache convention keys off the
     * typed {@link NativePlatform.Os}, never grouped platform constants.
     */
    static Path cacheRoot(NativePlatform platform, String property, String userHome,
                          UnaryOperator<String> env) {
        if (nonEmpty(property) != null) {
            return Paths.get(property);
        }
        Path base = switch (platform.os()) {
            case DARWIN -> home(userHome).resolve("Library").resolve("Caches");
            // XDG Base Directory spec: $XDG_CACHE_HOME when set to an absolute
            // path, otherwise $HOME/.cache.
            case LINUX -> {
                String xdg = env.apply("XDG_CACHE_HOME");
                yield nonEmpty(xdg) != null && Paths.get(xdg).isAbsolute()
                        ? Paths.get(xdg)
                        : home(userHome).resolve(".cache");
            }
            case WIN32 -> {
                String local = env.apply("LOCALAPPDATA");
                if (nonEmpty(local) == null) {
                    throw new RelateException("relate: LOCALAPPDATA is not set, so the"
                            + " native-library cache has no home. " + cacheHint());
                }
                yield Paths.get(local);
            }
        };
        return base.resolve("relate").resolve("native");
    }

    private static Path home(String userHome) {
        if (nonEmpty(userHome) == null || UNKNOWN_USER_HOME.equals(userHome)) {
            throw new RelateException("relate: user.home is unknown, so the native-library"
                    + " cache has no home. " + cacheHint());
        }
        return Paths.get(userHome);
    }

    /** The relocation remedy shared by every cache-home failure. */
    private static String cacheHint() {
        return "Set -D" + CACHE_PROPERTY + "=<writable directory>.";
    }

    /** The value when it is a set, non-empty string; null when unset — one "unset" policy for the package. */
    static String nonEmpty(String value) {
        return value != null && !value.isEmpty() ? value : null;
    }

    /**
     * Extract {@code resource} to
     * {@code <root>/<version>/<key>-<sha256 prefix>/<library>} and return that
     * path. The resource is read exactly once — into memory, where the digest
     * that names the cache directory is computed — so every launch pays one
     * read, warm or cold. An existing file at the path is reused (checked
     * without following links, so a planted symlink is never loaded): it can
     * only have been placed by an atomic rename of a complete copy of the same
     * bytes. A missing one is written to a temp file in the same directory,
     * flushed to disk and renamed into place, so concurrent JVMs never observe
     * a partial library.
     */
    static Path extract(URL resource, NativePlatform platform, Path root, String version)
            throws IOException {
        byte[] bytes;
        try (InputStream in = resource.openStream()) {
            bytes = in.readAllBytes();
        }
        String digest = hex(sha256().digest(bytes));
        Path dir = root.resolve(version)
                .resolve(platform.key() + "-" + digest.substring(0, DIGEST_PREFIX));
        Path target = dir.resolve(platform.libraryFileName());
        if (completeFileAt(target)) {
            return target;
        }
        Files.createDirectories(dir);
        Path tmp = Files.createTempFile(dir, platform.libraryFileName() + ".", ".tmp");
        try {
            Files.write(tmp, bytes);
            try (FileChannel channel = FileChannel.open(tmp, StandardOpenOption.WRITE)) {
                channel.force(true);
            }
            try {
                Files.move(tmp, target, StandardCopyOption.ATOMIC_MOVE);
            } catch (IOException e) {
                // A concurrent JVM renamed the same bytes into place first;
                // Windows refuses to replace a library another process has
                // loaded. Any other failure leaves no target and propagates.
                if (!completeFileAt(target)) {
                    throw e;
                }
            }
        } finally {
            Files.deleteIfExists(tmp);
        }
        return target;
    }

    /** The artifact version stamped into the jar at build time. */
    static String version() {
        Properties props = new Properties();
        try (InputStream in = NativeLoader.class.getClassLoader().getResourceAsStream(VERSION_RESOURCE)) {
            if (in == null) {
                throw new RelateException("relate: the jar is missing " + VERSION_RESOURCE);
            }
            props.load(in);
        } catch (IOException e) {
            throw new RelateException("relate: cannot read " + VERSION_RESOURCE
                    + " to version the native-library cache.", e);
        }
        String version = nonEmpty(props.getProperty("version"));
        if (version == null || version.startsWith("${")) {
            throw new RelateException("relate: " + VERSION_RESOURCE
                    + " carries no build-stamped version, so the native-library cache"
                    + " cannot be named.");
        }
        return version;
    }

    /**
     * A complete regular file sits at the path — never a symlink, so a
     * planted link at the predictable cache location is never loaded. The
     * one statement of the never-follow-links policy for both extract sites.
     */
    private static boolean completeFileAt(Path target) {
        return Files.isRegularFile(target, LinkOption.NOFOLLOW_LINKS);
    }

    private static MessageDigest sha256() {
        try {
            return MessageDigest.getInstance("SHA-256");
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException("every JDK provides SHA-256", e);
        }
    }

    private static String hex(byte[] bytes) {
        return HexFormat.of().formatHex(bytes);
    }
}
