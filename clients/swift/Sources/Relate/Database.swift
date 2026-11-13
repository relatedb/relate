import CRelate
import Foundation

/// An open Relate store: `":memory:"`, a single-file `.rl` store, or a
/// directory holding the WAL and checkpoints (a path ending in `.rl` names
/// the single-file form, anything else a directory). Closed when the last
/// reference (including every ``Connection`` made from it) is released.
///
/// `Database` is `Sendable`: the C ABI documents a `relate_database` as
/// safe to share across threads, so the wrapper adds no lock around it.
///
/// Besides the synchronous ``connect()``, a `Database` offers managed
/// asynchronous access — `read(_:)` and `write(retries:_:)` — over
/// connections it opens and owns itself.
public final class Database: Sendable {
    /// The open store. Connections hold it too, so the store outlives the
    /// `Database` object for as long as any connection does.
    let core: DatabaseCore
    public let path: String
    public let isReadOnly: Bool
    /// The connections behind the managed asynchronous entry points. This
    /// object owns the pool; the pool's connections refer back to it
    /// without owning it, so releasing the last `Database` reference
    /// releases them.
    let pool: ConnectionPool

    /// Settings for the managed asynchronous access.
    public struct Options: Sendable, Hashable {
        /// The most reader connections `Database.read(_:)` opens.
        /// Reads beyond that wait their turn on the least busy reader. At
        /// least 1: opening a database with fewer throws
        /// ``RelateError/invalidParameter(_:)``.
        public var maxReaders: Int

        public init(maxReaders: Int = 2) {
            self.maxReaders = maxReaders
        }
    }

    /// Opens (creating when missing) a writable store at `path`.
    public convenience init(path: String, options: Options = Options()) throws(RelateError) {
        try self.init(path: path, readOnly: false, options: options)
    }

    /// Opens an existing store once, refusing writes and checkpoints (ADR-0136).
    public convenience init(readOnlyPath path: String, options: Options = Options()) throws(RelateError) {
        try self.init(path: path, readOnly: true, options: options)
    }

    /// Opens the store at a file URL: writable (created when missing) by
    /// default, or — with `readOnly` — an existing store opened once,
    /// refusing writes and checkpoints. The engine opens local paths only,
    /// so a URL that is not a `file:` URL, or a `file:` URL naming a host
    /// other than `localhost` (in any letter case, with or without the
    /// trailing dot of a fully qualified name), is refused with
    /// ``RelateError/invalidParameter(_:)`` rather than opened as whatever
    /// local path its path component happens to spell.
    public convenience init(
        url: URL, readOnly: Bool = false, options: Options = Options()
    ) throws(RelateError) {
        guard url.isFileURL else {
            throw RelateError.invalidParameter(
                "a store URL must be a file URL, got scheme \(quotedWire(url.scheme ?? ""))")
        }
        let host = url.host ?? ""
        guard ["", "localhost", "localhost."].contains(host.lowercased()) else {
            throw RelateError.invalidParameter(
                "a store URL must name a local file, got host \(quotedWire(host))")
        }
        try self.init(path: url.path, readOnly: readOnly, options: options)
    }

    /// A fresh in-memory store.
    public static func inMemory(options: Options = Options()) throws(RelateError) -> Database {
        try Database(path: ":memory:", options: options)
    }

    private init(path: String, readOnly: Bool, options: Options) throws(RelateError) {
        guard options.maxReaders >= 1 else {
            throw RelateError.invalidParameter(
                "a database needs at least one reader, got maxReaders \(options.maxReaders)")
        }
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
        let core = DatabaseCore(handle: try requireHandle(out, "relate_database_open"))
        self.core = core
        self.path = path
        self.isReadOnly = readOnly
        self.pool = ConnectionPool(maxReaders: options.maxReaders)
    }

    /// A new connection on the default namespace, `neo4j`.
    public func connect() throws(RelateError) -> Connection {
        try connect(namespace: .neo4j)
    }

    /// A new connection whose session namespace is `namespace` (ADR-0117):
    /// one of the three fixed namespaces.
    public func connect(namespace: DatabaseNamespace) throws(RelateError) -> Connection {
        try connect(namespaceNamed: namespace.rawValue)
    }

    /// A new connection whose session namespace is named `namespace`
    /// (`nil` is `neo4j`).
    @available(*, deprecated, message: "the namespaces are fixed: pass a DatabaseNamespace to connect(namespace:)")
    public func connect(namespace: String?) throws(RelateError) -> Connection {
        try connect(namespaceNamed: namespace ?? DatabaseNamespace.neo4j.rawValue)
    }

    /// A caller-made connection on the namespace named `name`.
    func connect(namespaceNamed name: String) throws(RelateError) -> Connection {
        try core.connect(namespaceNamed: name, database: self, retained: true)
    }

    /// Writes a checkpoint and truncates the WAL.
    public func checkpoint() throws(RelateError) {
        try core.checkpoint(describing: path)
    }
}

/// The open store handle, closed when its last holder — the `Database`
/// object or any ``Connection`` — is released.
///
/// `@unchecked Sendable` because it stores an `OpaquePointer`; the C ABI
/// documents a `relate_database` as safe to share across threads.
final class DatabaseCore: @unchecked Sendable {
    private let handle: OpaquePointer

    init(handle: OpaquePointer) {
        self.handle = handle
    }

    deinit {
        // A close failure here has no caller to report to; the handle is
        // freed either way (poisoned handles included).
        _ = relate_database_close(handle)
    }

    /// The one crossing that opens a session. The engine matches `name`
    /// against its fixed namespaces and refuses any other. A connection
    /// made for a caller is `retained`: it keeps `database` alive. One the
    /// managed pool opens is not, which is what keeps the pool from owning
    /// the object that owns it.
    func connect(
        namespaceNamed name: String, database: Database, retained: Bool
    ) throws(RelateError) -> Connection {
        var out: OpaquePointer?
        var err: UnsafeMutablePointer<CChar>? = nil
        let status = try withCString(name, what: "namespace") {
            relate_connect_namespace(handle, $0, &out, &err)
        }
        try check(status, err, "connecting to namespace '\(name)'")
        return Connection(
            handle: try requireHandle(out, "relate_connect_namespace"), core: self,
            database: database, retained: retained)
    }

    func checkpoint(describing path: String) throws(RelateError) {
        var err: UnsafeMutablePointer<CChar>? = nil
        try check(relate_checkpoint(handle, &err), err, "checkpointing '\(path)'")
    }
}

/// One session on a ``Database``. Not `Sendable`, matching the C ABI's
/// "do not share a connection across threads": keep each connection inside
/// one actor (or one thread) and open another for concurrent work.
public final class Connection {
    private let handle: OpaquePointer
    /// Keeps the store open for as long as this connection lives.
    let core: DatabaseCore
    /// The database this connection was made from. Holding a connection
    /// from ``Database/connect()`` keeps it open.
    public unowned let database: Database
    /// What keeps `database` alive for a caller-made connection. A
    /// connection the managed pool opened holds none: its database owns it
    /// (through the pool) and outlives it, so it only refers back.
    private let retainedDatabase: Database?

    fileprivate init(handle: OpaquePointer, core: DatabaseCore, database: Database, retained: Bool) {
        self.handle = handle
        self.core = core
        self.database = database
        self.retainedDatabase = retained ? database : nil
    }

    deinit {
        _ = relate_disconnect(handle)
    }

    /// Changes the session namespace to one of the three fixed namespaces;
    /// only allowed between transactions.
    public func setNamespace(_ namespace: DatabaseNamespace) throws(RelateError) {
        try setNamespace(named: namespace.rawValue)
    }

    /// Changes the session namespace, by name; only allowed between
    /// transactions.
    @available(*, deprecated, message: "the namespaces are fixed: pass a DatabaseNamespace to setNamespace(_:)")
    public func setNamespace(_ name: String) throws(RelateError) {
        try setNamespace(named: name)
    }

    /// The one crossing that changes the session namespace; the engine
    /// refuses a name outside its fixed namespaces.
    func setNamespace(named name: String) throws(RelateError) {
        var err: UnsafeMutablePointer<CChar>? = nil
        let status = try withCString(name, what: "namespace") {
            relate_conn_set_namespace(handle, $0, &err)
        }
        try check(status, err, "setting the namespace to '\(name)'")
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

    /// Runs one statement with Swift values as parameters: each converts
    /// through ``CypherValueConvertible`` — strings, numbers, optionals
    /// (`nil` is `null`), arrays, string-keyed dictionaries, `Date`, `URL`,
    /// `UUID` and the typed structs.
    ///
    /// Disfavored so that a dictionary of literals keeps resolving to the
    /// `[String: CypherValue]` entry point, with no conversion pass.
    @_disfavoredOverload
    @discardableResult
    public func query(_ cypher: String, _ parameters: [String: any CypherValueConvertible])
        throws(RelateError) -> QueryResult
    {
        try query(cypher, try convertParameters(parameters))
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
}
