package ai.relatedb;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import static org.junit.jupiter.api.Assertions.*;

/**
 * The lifecycle the DriverSeam gaps closed (ADR-0502 §8.1): close is real,
 * a file store reopens in the same JVM, and both bookmarks (ADR-0502 §7)
 * surface — the auto-commit statement's on its result, the explicit
 * transaction's from commit().
 */
public class LifecycleTest {

    @Test
    void closeIsIdempotentAndAFollowUpCallFailsLoudly(@TempDir Path dir) throws IOException {
        Path store = dir.resolve("store");
        Database db = Database.open(store.toString());
        Database.Connection conn = db.connect();
        conn.query("CREATE (:Person {name: 'Ada'})");
        conn.close();
        db.close();
        db.close(); // idempotent
        assertEquals("database is closed",
                assertThrows(RelateException.class, () -> db.connect()).getMessage());
    }

    @Test
    void aFileStoreReopensInTheSameJvm(@TempDir Path dir) throws IOException {
        Path store = dir.resolve("store");
        try (Database db = Database.open(store.toString());
             Database.Connection conn = db.connect()) {
            conn.query("CREATE (:Person {name: 'Ada'})");
        }
        // Without close() this second open failed (the DriverSeam gap);
        // with it the store reopens and the write survived
        try (Database db = Database.open(store.toString());
             Database.Connection conn = db.connect()) {
            Database.Result r = conn.query("MATCH (p:Person) RETURN p.name AS name");
            assertEquals(List.of("Ada"), r.rows().get(0));
        }
        // A read-only reopen sees the same data (ADR-0136)
        try (Database db = Database.openReadOnly(store.toString());
             Database.Connection conn = db.connect()) {
            assertEquals(1L, conn.query("MATCH (p:Person) RETURN count(p) AS n")
                    .rows().get(0).get(0));
        }
        assertTrue(Files.isDirectory(store), "the Native VFS store is a directory");
    }

    private static long sequence(String bookmark) {
        assertTrue(bookmark.startsWith("relate:"), "bookmark form: " + bookmark);
        return Long.parseLong(bookmark.substring("relate:".length()));
    }

    @Test
    void autoCommitBookmarkRidesTheResult() {
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        assertNull(conn.query("RETURN 1 AS n").getBookmark(), "a read commits nothing");
        String bookmark = conn.query("CREATE (:Note {n: 1})").getBookmark();
        assertNotNull(bookmark, "an auto-commit write carries its bookmark");
        assertTrue(sequence(bookmark) >= 1);
        assertNull(conn.query("EXPLAIN CREATE (:Note {n: 2})").getBookmark(),
                "EXPLAIN executes nothing");
    }

    @Test
    void explicitCommitReturnsTheBookmarkAndNothingCommitsToNull() {
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        String first = conn.query("CREATE (:Note {n: 1})").getBookmark();
        conn.begin();
        conn.query("CREATE (:Note {n: 2})");
        String second = conn.commit();
        assertNotNull(second, "the transaction wrote");
        assertTrue(sequence(second) > sequence(first),
                "a later commit's bookmark sorts after an earlier one's");
        // A transaction that wrote nothing commits to null: no bookmark
        // is fabricated
        conn.begin();
        conn.query("RETURN 1 AS n");
        assertNull(conn.commit(), "committed nothing: no bookmark");
    }

    @Test
    void aStatementInsideATransactionHasNoResultBookmark() {
        Database db = Database.open(":memory:");
        Database.Connection conn = db.connect();
        conn.begin();
        assertNull(conn.query("CREATE (:Note {n: 1})").getBookmark(),
                "inside a transaction the commit's bookmark is the one that counts");
        assertNotNull(conn.commit());
    }
}
