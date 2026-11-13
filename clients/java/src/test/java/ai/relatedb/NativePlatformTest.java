package ai.relatedb;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.CsvSource;

import static org.junit.jupiter.api.Assertions.*;

public class NativePlatformTest {
    @ParameterizedTest
    @CsvSource({
        // os.name as each JDK reports it, os.arch in both spellings.
        "Mac OS X, aarch64, DARWIN_ARM64",
        "Mac OS X, x86_64, DARWIN_X64",
        "Linux, aarch64, LINUX_ARM64",
        "Linux, amd64, LINUX_X64",
        "Linux, x86_64, LINUX_X64",
        "Windows 11, amd64, WIN32_X64",
        "Windows Server 2022, amd64, WIN32_X64",
    })
    void mapsEverySupportedPlatform(String osName, String osArch, NativePlatform expected) {
        assertEquals(expected, NativePlatform.detect(osName, osArch));
    }

    @Test
    void everyPlatformHasItsRowFacts() {
        assertEquals("native/darwin-arm64/librelate_ffi.dylib", NativePlatform.DARWIN_ARM64.resourcePath());
        assertEquals("native/darwin-x64/librelate_ffi.dylib", NativePlatform.DARWIN_X64.resourcePath());
        assertEquals("native/linux-arm64/librelate_ffi.so", NativePlatform.LINUX_ARM64.resourcePath());
        assertEquals("native/linux-x64/librelate_ffi.so", NativePlatform.LINUX_X64.resourcePath());
        assertEquals("native/win32-x64/relate_ffi.dll", NativePlatform.WIN32_X64.resourcePath());
        assertEquals("osx-aarch_64", NativePlatform.DARWIN_ARM64.classifier());
        assertEquals("osx-x86_64", NativePlatform.DARWIN_X64.classifier());
        assertEquals("linux-aarch_64", NativePlatform.LINUX_ARM64.classifier());
        assertEquals("linux-x86_64", NativePlatform.LINUX_X64.classifier());
        assertEquals("windows-x86_64", NativePlatform.WIN32_X64.classifier());
    }

    @ParameterizedTest
    @CsvSource({
        "Windows 11, aarch64",   // a real JVM platform with no bundled native
        "Linux, riscv64",
        "Linux, x86",
        "FreeBSD, amd64",
        "SunOS, sparcv9",
    })
    void unsupportedCombinationsFailLoudly(String osName, String osArch) {
        RelateException e = assertThrows(RelateException.class,
                () -> NativePlatform.detect(osName, osArch));
        assertTrue(e.getMessage().contains("os.name=" + osName), e.getMessage());
        assertTrue(e.getMessage().contains("os.arch=" + osArch), e.getMessage());
    }

    @Test
    void missingPropertiesFailLoudly() {
        assertThrows(RelateException.class, () -> NativePlatform.detect(null, "amd64"));
        assertThrows(RelateException.class, () -> NativePlatform.detect("Linux", null));
    }

    @Test
    void theRunningJvmIsASupportedPlatform() {
        // This suite only runs on the five platforms the build supports.
        assertNotNull(NativePlatform.current());
    }
}
