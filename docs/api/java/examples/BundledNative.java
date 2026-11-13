package ai.relatedb.examples;

import ai.relatedb.Database;

/**
 * Packaging (ADR-0490): with the all-platform jar (or this platform's
 * classifier jar) on the classpath, opening a database needs no
 * RELATE_LIB and no java.library.path — the bundled native is extracted
 * to the per-version cache on first use and loaded from there.
 *
 * <pre>
 * java -cp relate-0.1.0.jar:jna-5.14.0.jar:jackson-databind-2.17.2.jar:... \
 *      ai.relatedb.examples.BundledNative
 * </pre>
 */
public class BundledNative {

    /** The success marker the PackagedJarIT asserts when it runs this example. */
    public static final String OK = "n=1";

    public static void main(String[] args) {
        var db = Database.open(":memory:");
        var result = db.connect().query("RETURN 1 AS n");
        Object value = result.rows().get(0).get(0);
        // the one canonical hydrated-number compare (HydratedValues) —
        // Integer or Long normalize to Long, anything else fails loudly
        if (!HydratedValues.equal(value, 1L)) {
            throw new IllegalStateException("unexpected: " + result.rows());
        }
        // the guard above proves n == 1, so the marker is the honest output
        System.out.println(OK);
    }
}
