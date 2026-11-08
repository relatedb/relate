package com.relate;

import com.sun.jna.Pointer;
import com.sun.jna.ptr.PointerByReference;

import java.util.List;
import java.util.Map;

/**
 * An open Relate database over the typed C ABI v2 (ADR-0502), bound
 * through JNA. Lifecycle: {@link #open(String)} / {@link #openReadOnly(String)}
 * a {@link Database}, {@link #connect()} a {@link Connection}, and close
 * both — {@link Database} and {@link Connection} are
 * {@link AutoCloseable}, so try-with-resources works; without
 * {@code close()} a file store cannot be reopened in the same JVM.
 */
public class Database implements AutoCloseable {

    private Pointer handle;

    private Database(Pointer handle) {
        this.handle = handle;
    }

    public static Database open(String uri) {
        PointerByReference out = new PointerByReference();
        PointerByReference err = new PointerByReference();
        NativeLib.check(NativeLib.lib().relate_database_open(uri, out, err),
                "relate_database_open", err);
        return new Database(out.getValue());
    }

    /** Load once and refuse writes and checkpoints (ADR-0136). A missing store is not created. */
    public static Database openReadOnly(String uri) {
        PointerByReference out = new PointerByReference();
        PointerByReference err = new PointerByReference();
        NativeLib.check(NativeLib.lib().relate_database_open_read_only(uri, out, err),
                "relate_database_open_read_only", err);
        return new Database(out.getValue());
    }

    public Connection connect() {
        // Canonical entry point (ADR-0220): a null name selects the default
        // "neo4j" namespace; JNA marshals a null String as NULL.
        return connect(null);
    }

    /**
     * Connect with a session namespace (ADR-0117): "neo4j", "system" or
     * "system_ontologies", matched case-insensitively. The namespace is the
     * session default for statements that carry no USE clause.
     * connect() is equivalent to connect("neo4j").
     */
    // synchronized like every handle-reading method: close() nulls the
    // handle under the same monitor, so no caller hands a stale handle to
    // the native call.
    public synchronized Connection connect(String namespace) {
        ensureOpen();
        PointerByReference out = new PointerByReference();
        PointerByReference err = new PointerByReference();
        NativeLib.check(NativeLib.lib().relate_connect_namespace(handle, namespace, out, err),
                "relate_connect_namespace", err);
        return new Connection(out.getValue());
    }

    /** Fold the WAL into a checkpoint (refused on a read-only database). */
    public synchronized void checkpoint() {
        ensureOpen();
        PointerByReference err = new PointerByReference();
        NativeLib.check(NativeLib.lib().relate_checkpoint(handle, err),
                "relate_checkpoint", err);
    }

    /**
     * Close the database. Idempotent — the second and later calls are
     * no-ops — and the only way a file store becomes reopenable in this
     * JVM. Connections opened from it should be closed first; closing the
     * database with live connections is the caller's error to make.
     */
    @Override
    public synchronized void close() {
        Pointer open = handle;
        if (open == null) {
            return;
        }
        handle = null;
        NativeLib.lib().relate_database_close(open);
    }

    private synchronized void ensureOpen() {
        if (handle == null) {
            throw new RelateException("database is closed");
        }
    }

    /**
     * One session on the database. A connection serves one thread at a
     * time (the ABI's contract); every method is synchronized to keep
     * that true. Because every failure travels with its call (ADR-0502
     * §2), a query on any executor thread reports its own error.
     */
    public static class Connection implements AutoCloseable {
        private Pointer handle;

        Connection(Pointer handle) {
            this.handle = handle;
        }

        /** Run a statement with no parameters (auto-commit outside an explicit transaction). */
        public Result query(String cypher) {
            return query(cypher, null);
        }

        /**
         * Run a statement with named parameters (the typed params builder,
         * ADR-0502 §4): one putter per Java type, no JSON on the way.
         * Values bind as their natural Cypher types — {@code Byte} /
         * {@code Short} / {@code Integer} / {@code Long} (all INTEGER),
         * {@code Float} / {@code Double} (FLOAT), {@code Boolean},
         * {@code String}, {@code null}, {@code List}, {@code Map},
         * {@code java.time.LocalDate} / {@code LocalTime} /
         * {@code OffsetTime} / {@code LocalDateTime} /
         * {@code OffsetDateTime} / {@code ZonedDateTime},
         * {@link DurationValue} and {@code java.time.Duration} (zero
         * months/days), {@link Point}, {@link VectorValue}. A
         * Java type outside that set throws
         * {@link IllegalArgumentException} naming it — never a silent
         * coercion.
         */
        public synchronized Result query(String cypher, Map<String, ?> params) {
            ensureOpen();
            NativeLib.CLib lib = NativeLib.lib();
            Pointer root = params == null ? null : Params.build(params);
            PointerByReference out = new PointerByReference();
            PointerByReference err = new PointerByReference();
            try {
                NativeLib.check(lib.relate_query_params(handle, cypher, root,
                        out, err), "relate_query_params", err);
            } finally {
                if (root != null) {
                    lib.relate_params_free(root);
                }
            }
            return ResultReader.read(out.getValue());
        }

        /**
         * Change the session namespace of this connection (ADR-0117). Names
         * match case-insensitively. Refused while an explicit transaction is
         * open ("Cannot change the namespace while a transaction is open.").
         */
        public synchronized void setNamespace(String namespace) {
            ensureOpen();
            PointerByReference err = new PointerByReference();
            NativeLib.check(NativeLib.lib().relate_conn_set_namespace(handle, namespace, err),
                    "relate_conn_set_namespace", err);
        }

        /** Open an explicit transaction (statements run inside it until commit/rollback). */
        public synchronized void begin() {
            ensureOpen();
            PointerByReference err = new PointerByReference();
            NativeLib.check(NativeLib.lib().relate_conn_begin(handle, err),
                    "relate_conn_begin", err);
        }

        /**
         * Commit the explicit transaction and return its bookmark — the
         * engine's commit sequence in the {@code "relate:<n>"} form
         * (ADR-0310), monotonically increasing across every connection of
         * the store — or {@code null} when the transaction committed
         * nothing (no write ran, or every write was a no-op): no bookmark
         * is fabricated. A failed commit (a retryable write-write conflict
         * among them) throws the typed exception instead.
         */
        public synchronized String commit() {
            ensureOpen();
            PointerByReference bookmarkOut = new PointerByReference();
            PointerByReference err = new PointerByReference();
            // status alone discriminates failure from an empty commit (ADR-0510)
            NativeLib.check(NativeLib.lib().relate_conn_commit(handle, bookmarkOut, err),
                    "relate_conn_commit", err);
            // caller-owned out-param value: copy, then free — a String mapping would leak it
            Pointer bookmark = bookmarkOut.getValue();
            if (bookmark == null) {
                return null; // committed, nothing to bookmark
            }
            String value = bookmark.getString(0, "UTF-8");
            NativeLib.lib().relate_free_string(bookmark);
            return value;
        }

        /** Roll back the explicit transaction. */
        public synchronized void rollback() {
            ensureOpen();
            PointerByReference err = new PointerByReference();
            NativeLib.check(NativeLib.lib().relate_conn_rollback(handle, err),
                    "relate_conn_rollback", err);
        }

        /** Disconnect the session. Idempotent; the second and later calls are no-ops. */
        @Override
        public synchronized void close() {
            Pointer open = handle;
            if (open == null) {
                return;
            }
            handle = null;
            NativeLib.lib().relate_disconnect(open);
        }

        private void ensureOpen() {
            if (handle == null) {
                throw new RelateException("connection is closed");
            }
        }
    }

    /**
     * One statement's whole result, read off the native handle into Java
     * values while the handle was alive. Cells are decoded through the
     * typed value walker (ADR-0502 §1) — see the value classes'
     * documentation for the Java type of each Cypher kind.
     */
    public static class Result {
        private final List<String> columns;
        private final List<List<Object>> rows;
        private final String queryType;
        private final String namespace;
        private final String bookmark;
        private final List<Notification> notifications;
        private final PlanDocument plan;

        /**
         * The one constructor, bookmark included. Package-private: the
         * only builder is {@link ResultReader}, which reads the handle —
         * results come from query calls, never from user code. The seven
         * positional parameters (three adjacent same-typed metadata
         * Strings among them) are the accepted trade-off of a single
         * caller passing named locals; a second builder would force a
         * key-addressed form.
         */
        Result(List<String> columns, List<List<Object>> rows, String queryType,
               String namespace, String bookmark, List<Notification> notifications,
               PlanDocument plan) {
            this.columns = columns;
            this.rows = rows;
            this.queryType = queryType;
            this.namespace = namespace;
            this.bookmark = bookmark;
            this.notifications = notifications;
            this.plan = plan;
        }

        public List<String> columns() { return columns; }
        public List<List<Object>> rows() { return rows; }
        /**
         * The statement's plan-time query type (ADR-0162, ADR-0285):
         * "r" (read), "w" (a write with no returned columns), "rw" (a
         * write with returned columns) or "s" (schema), the split per
         * ADR-0332. EXPLAIN and PROFILE report the wrapped statement's
         * type.
         */
        public String getQueryType() { return queryType; }
        /**
         * The statement's resolved namespace (ADR-0117, ADR-0285):
         * "neo4j", "system" or "system_ontologies". This is the USE target
         * when the statement carries a USE clause, otherwise the session
         * namespace.
         */
        public String getNamespace() { return namespace; }
        /**
         * The auto-commit statement's bookmark — the engine's commit
         * sequence in the {@code "relate:<n>"} form (ADR-0310) — or
         * {@code null} when the statement committed nothing: a read, a
         * write whose row pipeline was empty, an EXPLAIN, any statement
         * inside an explicit transaction (its {@link Connection#commit()}
         * bookmark is the one that counts), or an error result.
         */
        public String getBookmark() { return bookmark; }
        /**
         * The statement's query notifications (ADR-0363) in engine-native
         * shape: populated by EXPLAIN (and PROFILE), empty on a plain run
         * because derivation is opt-in with the plan tree. The list keeps
         * the engine's deterministic order (operator-scoped entries in plan
         * pre-order, then statement-scoped ones) and never carries a
         * {@code position}.
         */
        public List<Notification> getNotifications() { return notifications; }

        /**
         * The statement's structured plan (LABS-418, ADR-0381) as a
         * {@code relate.plan/v1} document — the operator tree with
         * per-node estimates, PROFILE metrics and each operator's
         * remedy-bearing warnings (ADR-0363) joined by operator id — or
         * {@code null} on a plain run. The typed classes are
         * {@code PlanDocument} ({@code schema}, {@code profiled},
         * {@code statement}, {@code root}), {@code PlanNode},
         * {@code PlanActual} and {@code PlanStart}.
         */
        public PlanDocument getPlan() { return plan; }
    }
}
