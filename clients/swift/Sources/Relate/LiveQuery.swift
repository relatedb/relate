#if canImport(SwiftUI)
import SwiftUI

// MARK: - The database in the environment

extension EnvironmentValues {
    /// The database the `@LiveQuery` properties of this view tree read.
    /// Set it once, near the root, with `relateDatabase(_:)`.
    public var relateDatabase: Database? {
        get { self[RelateDatabaseKey.self] }
        set { self[RelateDatabaseKey.self] = newValue }
    }
}

private struct RelateDatabaseKey: EnvironmentKey {
    static let defaultValue: Database? = nil
}

extension View {
    /// Gives the `@LiveQuery` properties of this view and its descendants
    /// the database they read.
    public func relateDatabase(_ database: Database?) -> some View {
        environment(\.relateDatabase, database)
    }
}

// MARK: - A live query

/// A value read from the database in the environment, kept current: the
/// view is given the value now and again whenever a commit changes it.
///
/// ```swift
/// struct Library: View {
///     @LiveQuery("MATCH (b:Book) RETURN b.title AS title ORDER BY title", tags: ["books"])
///     private var books: [Book]
///
///     var body: some View {
///         List(books, id: \.title) { Text($0.title) }
///     }
/// }
/// ```
///
/// The property holds its `initial` value (an empty array for the statement
/// form) until the first read returns. It observes through
/// `Database.values(tags:_:)`, so the same rules hold: every commit made
/// through the database is followed by a read unless `tags` narrows that.
/// A read equal to the value the property holds changes nothing. The
/// projected value (`$books`) says where the query stands.
///
/// The observation lasts as long as the view, and starts again when the
/// query becomes a different one: when the statement (or, for the closure
/// form, `id`), the tags or the database in the environment changes. A
/// query that depends on the view's inputs is built in the view's
/// initializer:
///
/// ```swift
/// init(search: String) {
///     _books = LiveQuery(try "MATCH (b:Book) WHERE b.title CONTAINS \(search) RETURN b.title AS title")
/// }
/// ```
@MainActor @propertyWrapper
public struct LiveQuery<Value: Sendable & Equatable>: DynamicProperty {
    @Environment(\.relateDatabase) private var database
    @StateObject private var observer = LiveQueryObserver<Value>()
    /// What the property holds before its first read returns.
    let initial: Value
    /// What this property reads, or why that could not be stated (a
    /// statement whose interpolation threw).
    let request: Result<LiveQueryRequest<Value>, any Error>

    /// A value read by `read`, which runs as ``Database/read(_:)``.
    ///
    /// Give `id` whatever `read` captures that can change (a search text, a
    /// selection): the observation starts again when `id` does.
    public init(
        initial: Value, tags: Set<String>? = nil, id: AnyHashable = AnyHashable(0),
        _ read: @escaping @Sendable (Transaction) throws -> Value
    ) {
        self.initial = initial
        request = .success(LiveQueryRequest(identity: LiveQueryIdentity(id: id, tags: tags), read: read))
    }

    /// The records of `statement`, each decoded as an `Element` by column
    /// name. The statement, with the values bound into it, is the query's
    /// identity; `decoder` is not, and is taken as fixed for the property.
    ///
    /// A statement whose interpolation throws (a value with no Cypher form)
    /// makes the query `.failed` with that error; nothing is read.
    public init<Element: Decodable>(
        _ statement: @autoclosure () throws -> Statement, tags: Set<String>? = nil,
        using decoder: CypherValueDecoder = CypherValueDecoder()
    ) where Value == [Element] {
        initial = []
        request = Result {
            let statement = try statement()
            return LiveQueryRequest(identity: LiveQueryIdentity(id: AnyHashable(statement), tags: tags)) { tx in
                try tx.run(statement).decode(Element.self, using: decoder)
            }
        }
    }

    /// The latest value read, or the initial value before the first read.
    /// It stays while a changed query runs its first read, and after a read
    /// fails.
    public var wrappedValue: Value {
        observer.value ?? initial
    }

    /// Where the query stands.
    public var projectedValue: LiveQueryState {
        switch request {
        case .success(let request): observer.state(of: request.key(in: database))
        case .failure(let error): .failed(error)
        }
    }

    /// SwiftUI calls this before each evaluation of the view's body, on
    /// the main actor; the protocol's requirement does not say so, hence
    /// the assertion.
    public nonisolated func update() {
        MainActor.assumeIsolated {
            switch request {
            case .success(let request): observer.observe(request, in: database)
            case .failure: observer.stop()
            }
        }
    }
}

/// Where a ``LiveQuery`` stands.
public enum LiveQueryState: Sendable {
    /// The query has yet to produce a value or a failure since it last
    /// started (the view appeared, or the query changed).
    case loading
    /// The property holds the query's latest value, and follows commits.
    case live
    /// The query ended with this error: a statement that could not be
    /// built, a read that threw, or no database in the environment.
    case failed(any Error)

    public var isLoading: Bool {
        if case .loading = self { true } else { false }
    }

    public var isLive: Bool {
        if case .live = self { true } else { false }
    }

    /// The error of a query that failed; `nil` otherwise.
    public var error: (any Error)? {
        if case .failed(let error) = self { error } else { nil }
    }
}

/// What makes a live query the query it is: a fingerprint of what it reads,
/// and the tags it follows.
///
/// The fingerprint is the hash of the query's `id` (a statement with its
/// bound values, or the closure form's `id`), not the `id` itself. A
/// statement that binds a NaN is not equal to the identical statement built
/// on the next view update, so compared by equality it would look like a
/// new query every time and restart for ever; its hash does not change.
struct LiveQueryIdentity: Equatable {
    let fingerprint: Int
    let tags: Set<String>?

    init(id: AnyHashable, tags: Set<String>?) {
        fingerprint = id.hashValue
        self.tags = tags
    }
}

/// What a ``LiveQuery`` reads: its identity and the read.
struct LiveQueryRequest<Value: Sendable & Equatable> {
    let identity: LiveQueryIdentity
    let read: @Sendable (Transaction) throws -> Value

    /// The key of this request's observation on `database`: the one place
    /// the two are paired, for the observer that runs it and for the view
    /// that asks where it stands.
    func key(in database: Database?) -> LiveQueryKey {
        LiveQueryKey(identity: identity, database: database)
    }
}

/// What one observation follows: a query on a database (or on none, when
/// the environment has none). Both the rule for starting again and the
/// state a view is shown are keyed by it.
struct LiveQueryKey: Equatable {
    let identity: LiveQueryIdentity
    let database: Database?

    static func == (lhs: LiveQueryKey, rhs: LiveQueryKey) -> Bool {
        lhs.identity == rhs.identity && lhs.database === rhs.database
    }
}

/// The observation behind one ``LiveQuery`` property, kept for the view's
/// lifetime: it follows the request the property was last given.
@MainActor
final class LiveQueryObserver<Value: Sendable & Equatable>: ObservableObject {
    /// The latest value read, by whichever query read it.
    @Published private(set) var value: Value?
    /// The last observation to produce a value or a failure, and where that
    /// left it: `.live` or `.failed`.
    @Published private var settled: (key: LiveQueryKey, state: LiveQueryState)?

    /// The running observation and the key it follows; `nil` when none is
    /// running.
    private var running: (key: LiveQueryKey, task: Task<Void, Never>)?

    deinit {
        running?.task.cancel()
    }

    /// Where the observation of `key` stands: loading until it is the one
    /// that last settled. Computed from what was published, so a view sees
    /// a changed query as loading in the same update that changed it.
    func state(of key: LiveQueryKey) -> LiveQueryState {
        guard let settled, settled.key == key else { return .loading }
        return settled.state
    }

    /// Follows `request` on `database`, starting a new observation when
    /// that is not what is being followed. Called on every view update, so
    /// it publishes nothing itself: the observation's task does, after the
    /// update.
    func observe(_ request: LiveQueryRequest<Value>, in database: Database?) {
        let key = request.key(in: database)
        guard key != running?.key else { return }
        stop()
        // The task holds this object only while it publishes, so an
        // observation does not keep alive the view state it belongs to.
        let task = Task { [weak self] in
            do {
                guard let database else {
                    throw RelateError.misuse(
                        "@LiveQuery found no database in the environment: add .relateDatabase(_:) above this view")
                }
                for try await value in database.values(tags: request.identity.tags, request.read) {
                    self?.settle(key, .success(value))
                }
            } catch {
                self?.settle(key, .failure(error))
            }
        }
        running = (key, task)
    }

    /// Stops following: the property's query can no longer be stated. The
    /// last value stays.
    func stop() {
        running?.task.cancel()
        running = nil
    }

    /// What an observation reports: a value, or the error that ended it.
    /// Only a change is published, so a read that returns what the property
    /// already holds redraws nothing. An observation that was cancelled
    /// (the query changed, the view went away) reports nothing further.
    private func settle(_ key: LiveQueryKey, _ outcome: Result<Value, any Error>) {
        guard !Task.isCancelled else { return }
        switch outcome {
        case .success(let value):
            if value != self.value { self.value = value }
            if !state(of: key).isLive { settled = (key, .live) }
        case .failure(let error):
            settled = (key, .failed(error))
        }
    }
}
#endif
