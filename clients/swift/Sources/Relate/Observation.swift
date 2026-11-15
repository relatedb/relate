import Foundation
import os

// MARK: - Tracing

/// One statement, as it ran: what ``Database/setTraceHandler(_:)`` reports.
///
/// A trace carries the statement text and its parameter *names*. It never
/// carries parameter values, so tracing statements that bind their inputs
/// does not put user data in a log.
public struct QueryTrace: Sendable {
    /// The statement text as the engine received it.
    public let statement: String
    /// The names of the parameters bound, sorted.
    public let parameterNames: [String]
    /// Rows returned; zero when the statement failed.
    public let rowCount: Int
    /// The statement's classification; `nil` when it failed.
    public let queryType: QueryType?
    /// Engine-reported time: the total and its parse, plan and execute
    /// split. Zero when the statement failed.
    public let elapsedMs: Double
    public let parseMs: Double
    public let planMs: Double
    public let executeMs: Double
    /// Wall-clock time of the whole call as the caller saw it: parameter
    /// binding, the engine and copying the result out.
    public let wallMs: Double
    /// The error the statement threw, if it did. Its message is the
    /// engine's own and may quote a value the statement was given.
    public let error: RelateError?
}

extension QueryTrace {
    /// The trace of one finished statement. Parameter names only: values
    /// never reach a trace.
    init(
        _ cypher: String, _ parameters: [String: CypherValue],
        _ outcome: Result<QueryResult, RelateError>, since started: ContinuousClock.Instant
    ) {
        let wall = ContinuousClock.now - started
        let wallMs = Double(wall.components.seconds) * 1_000
            + Double(wall.components.attoseconds) / 1e15
        let names = parameters.keys.sorted()
        switch outcome {
        case .success(let result):
            self.init(
                statement: cypher, parameterNames: names, rowCount: result.rows.count,
                queryType: result.queryType, elapsedMs: result.elapsedMs, parseMs: result.parseMs,
                planMs: result.planMs, executeMs: result.executeMs, wallMs: wallMs, error: nil)
        case .failure(let error):
            self.init(
                statement: cypher, parameterNames: names, rowCount: 0, queryType: nil,
                elapsedMs: 0, parseMs: 0, planMs: 0, executeMs: 0, wallMs: wallMs, error: error)
        }
    }
}

// MARK: - Commits

/// One committed write, as ``Database/commits(bufferingPolicy:)`` reports it.
public struct Commit: Sendable, Hashable {
    /// The commit's bookmark (`"relate:<n>"`); later commits sort after
    /// earlier ones by `n`.
    public let bookmark: String
    /// The mutation counters, summed over the transaction's statements.
    public let stats: QueryStats
    /// The tags the writer attached (`db.write(tags: ["notes"]) { … }`):
    /// what an observer uses to decide whether this commit concerns it.
    /// Empty for an auto-commit statement.
    public let tags: Set<String>
}

// MARK: - The hub

/// The observers of one open store: every connection made from it — a
/// caller's or the pool's — reports here.
final class ObserverHub: Sendable {
    private struct State {
        var trace: (@Sendable (QueryTrace) -> Void)?
        var subscribers: [UUID: AsyncStream<Commit>.Continuation] = [:]
    }

    private let state = OSAllocatedUnfairLock(initialState: State())

    /// The signposter every statement's interval is emitted on. Signposts
    /// cost next to nothing unless Instruments is recording them.
    static let signposter = OSSignposter(subsystem: "ai.relatedb.relate", category: "Query")

    var traceHandler: (@Sendable (QueryTrace) -> Void)? {
        state.withLock { $0.trace }
    }

    func setTraceHandler(_ handler: (@Sendable (QueryTrace) -> Void)?) {
        state.withLock { $0.trace = handler }
    }

    func commits(bufferingPolicy: AsyncStream<Commit>.Continuation.BufferingPolicy) -> AsyncStream<Commit> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<Commit>.makeStream(bufferingPolicy: bufferingPolicy)
        state.withLock { $0.subscribers[id] = continuation }
        continuation.onTermination = { [weak self] _ in
            self?.state.withLock { _ = $0.subscribers.removeValue(forKey: id) }
        }
        return stream
    }

    /// Runs one statement under observation: the signpost interval and the
    /// trace handler bracket `run`, and its outcome is returned unchanged.
    func observing(
        _ cypher: String, _ parameters: [String: CypherValue],
        _ run: () throws(RelateError) -> QueryResult
    ) throws(RelateError) -> QueryResult {
        let trace = traceHandler
        let started = ContinuousClock.now
        let signposter = ObserverHub.signposter
        let interval = signposter.beginInterval("statement", id: signposter.makeSignpostID())
        let outcome = typedResult(run)
        switch outcome {
        case .success(let result):
            signposter.endInterval("statement", interval, "\(result.rows.count) rows")
        case .failure:
            signposter.endInterval("statement", interval, "failed")
        }
        trace?(QueryTrace(cypher, parameters, outcome, since: started))
        return try outcome.get()
    }

    /// Delivers a commit to every subscriber. The continuations are copied
    /// out under the lock and yielded to outside it.
    func publish(_ commit: Commit) {
        let subscribers = state.withLock { Array($0.subscribers.values) }
        for subscriber in subscribers { subscriber.yield(commit) }
    }

    /// Ends every stream: the store is closing.
    func finish() {
        let subscribers = state.withLock { state -> [AsyncStream<Commit>.Continuation] in
            defer { state.subscribers.removeAll() }
            return Array(state.subscribers.values)
        }
        for subscriber in subscribers { subscriber.finish() }
    }

    var subscriberCount: Int { state.withLock { $0.subscribers.count } }
}

// MARK: - Public entry points

extension Database {
    /// Sets (or, with `nil`, clears) the handler called once for every
    /// statement run on any connection of this database: a caller's own, or
    /// the ones behind `read` and `write`.
    ///
    /// The handler runs synchronously on the thread that ran the statement,
    /// after it finished, so it should be quick: log, count, or hand off.
    /// It also sees failed statements (``QueryTrace/error``).
    ///
    /// Independently of this handler, every statement is bracketed by an
    /// `os_signpost` interval (subsystem `ai.relatedb.relate`, category
    /// `Query`), visible in Instruments.
    public func setTraceHandler(_ handler: (@Sendable (QueryTrace) -> Void)?) {
        core.hub.setTraceHandler(handler)
    }

    /// A stream of the writes committed through this `Database` object:
    /// each auto-commit statement that changed the store and each explicit
    /// transaction that did.
    ///
    /// Each call returns a new stream; a stream receives the commits made
    /// after it was created, and ends when the store closes. A commit is
    /// published as soon as the engine has made it, before the trace
    /// handler hears of the statement. Commits from one connection — every
    /// `write` shares the writer — arrive in commit order; two connections
    /// committing at the same moment may arrive in either order, and their
    /// bookmarks tell which came first.
    ///
    /// Events wait in the stream until consumed. By default they wait
    /// without bound, so an observer that stops consuming while writes
    /// continue grows the backlog; pass `.bufferingNewest(n)` to keep only
    /// the latest `n` (enough for an observer that reloads on any commit).
    ///
    /// The stream is in-process and per object: a write made by another
    /// process, or through a second `Database` opened on the same store, is
    /// not reported.
    public func commits(
        bufferingPolicy: AsyncStream<Commit>.Continuation.BufferingPolicy = .unbounded
    ) -> AsyncStream<Commit> {
        core.hub.commits(bufferingPolicy: bufferingPolicy)
    }
}
