package com.relate;

import java.util.Arrays;
import java.util.Locale;
import java.util.stream.Collectors;

/**
 * The five platforms the jar can carry a bundled {@code relate_ffi} for
 * (ADR-0490) — the same set release.yml builds and the npm client ships.
 *
 * <p>A pure os/arch descriptor: one row per platform, carrying the typed
 * os/arch pair (from which the platform key — the {@code /native/<key>/}
 * resource directory, equal to the npm platform package suffix — is derived)
 * and the Maven classifier of the per-platform jar (the os-maven-plugin /
 * Gradle osdetector vocabulary, so {@code ${os.detected.classifier}}
 * selects it directly). The library file name is a per-OS fact on {@link Os};
 * per-user cache conventions live in {@link NativeLoader}, keyed on
 * {@link #os()}.
 */
enum NativePlatform {
    DARWIN_ARM64(Os.DARWIN, Arch.ARM64, "osx-aarch_64"),
    DARWIN_X64(Os.DARWIN, Arch.X64, "osx-x86_64"),
    LINUX_ARM64(Os.LINUX, Arch.ARM64, "linux-aarch_64"),
    LINUX_X64(Os.LINUX, Arch.X64, "linux-x86_64"),
    WIN32_X64(Os.WIN32, Arch.X64, "windows-x86_64");

    /**
     * The explicit-override environment variable (development only). It lives
     * on the descriptor, not the loader, so the platform error can name it
     * without the descriptor depending on its consumer.
     */
    static final String OVERRIDE_ENV = "RELATE_LIB";

    /**
     * The classpath resource directory every bundled native sits under — the
     * prefix of {@link #resourcePath()}, equal to the jar layout's {@code native}
     * target path. The one owner of the prefix, so the descriptor and the
     * jar-layout tests consult it instead of re-spelling {@code "native/"}.
     */
    static final String RESOURCE_DIR = "native/";

    private final Os os;
    private final Arch arch;
    private final String classifier;

    NativePlatform(Os os, Arch arch, String classifier) {
        this.os = os;
        this.arch = arch;
        this.classifier = classifier;
    }

    /** The platform key, e.g. {@code darwin-arm64}, derived from the os/arch pair. */
    String key() {
        return os.part + "-" + arch.part;
    }

    /** The OS family of this row — the per-OS facts key off this. */
    Os os() {
        return os;
    }

    /** The library file name the bundled resource carries (a per-OS fact). */
    String libraryFileName() {
        return os.libraryFileName;
    }

    /** The Maven classifier of this platform's jar, e.g. {@code osx-aarch_64}. */
    String classifier() {
        return classifier;
    }

    /** The classpath resource path of this platform's bundled library. */
    String resourcePath() {
        return RESOURCE_DIR + key() + "/" + libraryFileName();
    }

    /** The platform of the running JVM. */
    static NativePlatform current() {
        return detect(System.getProperty("os.name"), System.getProperty("os.arch"));
    }

    /**
     * The canonical os.name / os.arch mapping. Every combination outside the
     * five rows throws {@link RelateException} naming both inputs and the
     * supported keys — there is no nearest-match guess.
     */
    static NativePlatform detect(String osName, String osArch) {
        Os os = Os.of(osName);
        Arch arch = Arch.of(osArch);
        for (NativePlatform platform : values()) {
            // Reference-equality on typed rows: a null os/arch cannot match
            // any row, so unknown inputs fall through to the throw below.
            if (platform.os == os && platform.arch == arch) {
                return platform;
            }
        }
        throw new RelateException("relate: unsupported platform os.name=" + osName
                + ", os.arch=" + osArch + "; bundled natives exist for " + supportedKeys()
                + " only. Set " + OVERRIDE_ENV + " to a relate_ffi library"
                + " you built for this platform (development only).");
    }

    /** The supported keys, derived from the one table so the error can never drift. */
    private static String supportedKeys() {
        return Arrays.stream(values()).map(NativePlatform::key).collect(Collectors.joining(", "));
    }

    /** The os.name families the five rows cover, each with its per-OS facts. */
    enum Os {
        DARWIN("darwin", "librelate_ffi.dylib"),
        LINUX("linux", "librelate_ffi.so"),
        WIN32("win32", "relate_ffi.dll");

        final String part;
        final String libraryFileName;

        Os(String part, String libraryFileName) {
            this.part = part;
            this.libraryFileName = libraryFileName;
        }

        static Os of(String osName) {
            if (osName == null) {
                return null;
            }
            String os = osName.toLowerCase(Locale.ROOT);
            if (os.startsWith("mac os x") || os.equals("darwin")) {
                return DARWIN;
            }
            if (os.equals("linux")) {
                return LINUX;
            }
            if (os.startsWith("windows")) {
                return WIN32;
            }
            return null;
        }
    }

    /** The os.arch families the five rows cover, keyed by their key-part spelling. */
    private enum Arch {
        ARM64("arm64"), X64("x64");

        final String part;

        Arch(String part) {
            this.part = part;
        }

        static Arch of(String osArch) {
            if (osArch == null) {
                return null;
            }
            switch (osArch.toLowerCase(Locale.ROOT)) {
                case "aarch64":
                case "arm64":
                    return ARM64;
                case "amd64":
                case "x86_64":
                    return X64;
                default:
                    return null;
            }
        }
    }
}
