import CRelate
import Foundation

/// An open Relate store: `":memory:"`, a single-file `.rl` store, or a
/// directory holding the WAL and checkpoints (a path ending in `.rl` names
/// the single-file form, anything else a directory). Closed when the last
/// reference (including every ``Connection`` made from it) is released.
///
/// `Database` is `Sendable`: the C ABI documents a `relate_database` as
/// safe to share across threads, so the wrapper adds no lock of its own.
public final class Database: @unchecked Sendable {
    private let handle: OpaquePointer
    public let path: String
    public let isReadOnly: Bool

    /// Opens (creating when missing) a writable store at `path`.
    public convenience init(path: String) throws(RelateError) {
        try self.init(path: path, readOnly: false)
    }

    /// Opens an existing store once, refusing writes and checkpoints (ADR-0136).
    public convenience init(readOnlyPath path: String) throws(RelateError) {
        try self.init(path: path, readOnly: true)
    }

    /// Opens the store at a file URL: writable (created when missing) by
    /// default, or — with `readOnly` — an existing store opened once,
    /// refusing writes and checkpoints. The engine opens local paths only,
    /// so a URL that is not a `file:` URL, or a `file:` URL naming a host
    /// other than `localhost`, is refused with
    /// ``RelateError/invalidParameter(_:)`` rather than opened as whatever
    /// local path its path component happens to spell.
    public convenience init(url: URL, readOnly: Bool = false) throws(RelateError) {
        guard url.isFileURL else {
            throw RelateError.invalidParameter(
                "a store URL must be a file URL, got scheme \(quotedWire(url.scheme ?? ""))")
        }
        if let host = url.host, !host.isEmpty, host != "localhost" {
            throw RelateError.invalidParameter(
                "a store URL must name a local file, got host \(quotedWire(host))")
        }
        try self.init(path: url.path, readOnly: readOnly)
    }

    /// A fresh in-memory store.
    public static func inMemory() throws(RelateError) -> Database {
        try Database(path: ":memory:")
    }

    private init(path: String, readOnly: Bool) throws(RelateError) {
        // The package links the library at build time, so a v1 library
        // already failed to link; this is the startup assert the v2
        // contract still asks for.
        guard relate_abi_version() == 2 else {
            throw RelateError.decoding(
                "unsupported Relate C ABI version \(relate_abi_version()) (need 2)")
        }
        var out: OpaquePointer?
        var err: UnsafeMutablePointer<CChar>? = nil
        let status = try withCString(path, what: "path") { uri in
            readOnly
                ? relate_database_open_read_only(uri, &out, &err)
                : relate_database_open(uri, &out, &err)
        }
        try check(status, err, "opening the database at '\(path)'")
        self.handle = try requireHandle(out, "relate_database_open")
        self.path = path
        self.isReadOnly = readOnly
    }

    deinit {
        // A close failure here has no caller to report to; the handle is
        // freed either way (poisoned handles included).
        _ = relate_database_close(handle)
    }

    /// A new connection on the default namespace `neo4j`.
    public func connect() throws(RelateError) -> Connection {
        try connect(namespace: nil)
    }

    /// A new connection whose session namespace is `namespace` (ADR-0117),
    /// typed: one of the three fixed namespaces.
    public func connect(namespace: DatabaseNamespace) throws(RelateError) -> Connection {
        try connect(namespace: namespace.rawValue)
    }

    /// A new connection whose session namespace is `namespace` (ADR-0117).
    public func connect(namespace: String?) throws(RelateError) -> Connection {
        var out: OpaquePointer?
        var err: UnsafeMutablePointer<CChar>? = nil
        let status = try withOptionalCString(namespace, what: "namespace") {
            relate_connect_namespace(handle, $0, &out, &err)
        }
        try check(status, err, "connecting to namespace '\(namespace ?? "neo4j")'")
        return Connection(handle: try requireHandle(out, "relate_connect_namespace"), database: self)
    }

    /// Writes a checkpoint and truncates the WAL.
    public func checkpoint() throws(RelateError) {
        var err: UnsafeMutablePointer<CChar>? = nil
        try check(
            relate_checkpoint(handle, &err), err, "checkpointing '\(path)'")
    }
}

/// One session on a ``Database``. Not `Sendable`, matching the C ABI's
/// "do not share a connection across threads": keep each connection inside
/// one actor (or one thread) and open another for concurrent work.
public final class Connection {
    private let handle: OpaquePointer
    /// Keeps the database open for as long as this connection lives.
    public let database: Database

    fileprivate init(handle: OpaquePointer, database: Database) {
        self.handle = handle
        self.database = database
    }

    deinit {
        _ = relate_disconnect(handle)
    }

    /// Changes the session namespace; only allowed between transactions.
    public func setNamespace(_ name: String) throws(RelateError) {
        var err: UnsafeMutablePointer<CChar>? = nil
        let status = try withCString(name, what: "namespace") {
            relate_conn_set_namespace(handle, $0, &err)
        }
        try check(status, err, "setting the namespace to '\(name)'")
    }

    /// Changes the session namespace to one of the three fixed namespaces;
    /// only allowed between transactions.
    public func setNamespace(_ namespace: DatabaseNamespace) throws(RelateError) {
        try setNamespace(namespace.rawValue)
    }

    /// Runs one statement. An empty `parameters` binds none; otherwise each
    /// entry crosses through the typed params builder (ADR-0502 §4) — no
    /// JSON on the way, so temporals, points, vectors, non-finite floats and
    /// full-range integers all bind.
    @discardableResult
    public func query(_ cypher: String, _ parameters: [String: CypherValue] = [:])
        throws(RelateError) -> QueryResult
    {
        let params: OpaquePointer? = try parameters.isEmpty ? nil : bindParameters(parameters)
        defer { if let params { relate_params_free(params) } }
        var out: OpaquePointer?
        var err: UnsafeMutablePointer<CChar>? = nil
        let status = try withCString(cypher, what: "query") { q in
            relate_query_params(handle, q, params, &out, &err)
        }
        try check(status, err, "running the statement")
        let result = try requireHandle(out, "relate_query_params")
        defer { relate_destroy_result(result) }
        // A statement failure rides the result (ADR-0502 §2): the call
        // returns OK, so this check is what tells a failing statement from
        // an empty one. The json is result-owned — no free, unlike the
        // out-param channel `takeError` serves.
        if relate_result_is_error(result) != 0 {
            throw engineError(from: relate_result_error_json(result), what: "the error result")
        }
        return try QueryResult.materialize(result)
    }

    public func begin() throws(RelateError) {
        var err: UnsafeMutablePointer<CChar>? = nil
        try check(relate_conn_begin(handle, &err), err, "beginning the transaction")
    }

    /// Commits the explicit transaction and returns its bookmark
    /// (`"relate:<n>"`, the engine's monotonic commit sequence) — `nil`
    /// when the transaction committed nothing (no write ran, or every write
    /// was a no-op); no bookmark is fabricated. A failed commit throws the
    /// typed engine error — e.g. a retryable write conflict (ADR-0439).
    @discardableResult
    public func commit() throws(RelateError) -> String? {
        var bookmark: UnsafeMutablePointer<CChar>? = nil
        var err: UnsafeMutablePointer<CChar>? = nil
        try check(
            relate_conn_commit(handle, &bookmark, &err), err, "committing the transaction")
        // OK: a non-NULL *bookmark_out is the caller-owned bookmark; NULL
        // means the transaction committed nothing (ADR-0510).
        guard let bookmark else { return nil }
        defer { relate_free_string(bookmark) }
        return try required(bookmark, "the commit's bookmark")
    }

    public func rollback() throws(RelateError) {
        var err: UnsafeMutablePointer<CChar>? = nil
        try check(relate_conn_rollback(handle, &err), err, "rolling back the transaction")
    }

    /// Runs `body` in an explicit transaction: commits when it returns,
    /// rolls back and rethrows when it throws — and likewise when the commit
    /// itself throws.
    @discardableResult
    public func transaction<T>(_ body: (Connection) throws -> T) throws -> T {
        try begin()
        let value: T
        do {
            value = try body(self)
            try commit()
        } catch {
            // The first error (the body's, or the commit's) is the one worth
            // surfacing; a rollback failure after it would only mask the
            // cause, and a throwing commit must not leave the connection
            // inside the failed transaction (LABS-957).
            try? rollback()
            throw error
        }
        return value
    }
}
