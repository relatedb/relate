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
    /// What this property reads. The observer follows it.
    let request: LiveQueryRequest<Value>

    /// A value read by `read`, which runs as ``Database/read(_:)``.
    ///
    /// Give `id` whatever `read` captures that can change (a search text, a
    /// selection): the observation starts again when `id` does.
    public init(
        initial: Value, tags: Set<String>? = nil, id: AnyHashable = AnyHashable(0),
        _ read: @escaping @Sendable (Transaction) throws -> Value
    ) {
        request = LiveQueryRequest(identity: LiveQueryIdentity(id: id, tags: tags), initial: initial, read: read)
    }

    /// The records of `statement`, each decoded as an `Element` by column
    /// name. The statement, with the values bound into it, is the query's
    /// identity; `decoder` is not, and is taken as fixed for the property.
    ///
    /// A statement whose interpolation throws (a value with no Cypher form)
    /// is reported as the query's failure.
    public init<Element: Decodable>(
        _ statement: @autoclosure () throws -> Statement, tags: Set<String>? = nil,
        using decoder: CypherValueDecoder = CypherValueDecoder()
    ) where Value == [Element] {
        let statement = Result { try statement() }
        let id = switch statement {
        case .success(let statement): AnyHashable(statement)
        case .failure(let error): AnyHashable("\(error)")
        }
        request = LiveQueryRequest(identity: LiveQueryIdentity(id: id, tags: tags), initial: []) { tx in
            try tx.run(try statement.get()).decode(Element.self, using: decoder)
        }
    }

    /// The latest value read, or the initial value before the first read.
    /// It stays while a changed query runs its first read, and after a read
    /// fails.
    public var wrappedValue: Value {
        observer.value ?? request.initial
    }

    /// Where the query stands.
    public var projectedValue: LiveQueryState {
        observer.state(of: request.identity)
    }

    /// SwiftUI calls this before each evaluation of the view's body, on
    /// the main actor; the protocol's requirement does not say so, hence
    /// the assertion.
    public nonisolated func update() {
        MainActor.assumeIsolated {
            observer.observe(request, in: database)
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
    /// The observation ended with this error: a read that threw, or no
    /// database in the environment.
    case failed(any Error)

    public var isLoading: Bool {
        if case .loading = self { true } else { false }
    }

    /// The error of a query that failed; `nil` otherwise.
    public var error: (any Error)? {
        if case .failed(let error) = self { error } else { nil }
    }
}

/// What makes a live query the query it is: its `id` and the tags it
/// follows.
///
/// Two ids are the same query when they are equal or hash alike. Equality
/// alone is not enough: a statement that binds a NaN is not equal to the
/// identical statement built on the next view update, so compared plainly
/// it would look like a new query every time and restart for ever. Its
/// hash does not change. (Two different queries hash alike once in 2^64,
/// and then the changed one is simply not restarted.)
struct LiveQueryIdentity: Equatable {
    let id: AnyHashable
    let tags: Set<String>?

    static func == (lhs: LiveQueryIdentity, rhs: LiveQueryIdentity) -> Bool {
        lhs.tags == rhs.tags && (lhs.id == rhs.id || lhs.id.hashValue == rhs.id.hashValue)
    }
}

/// What a ``LiveQuery`` reads: its identity, the value it holds before its
/// first read, and the read.
struct LiveQueryRequest<Value: Sendable & Equatable> {
    let identity: LiveQueryIdentity
    let initial: Value
    let read: @Sendable (Transaction) throws -> Value
}

/// The observation behind one ``LiveQuery`` property, kept for the view's
/// lifetime: it follows the request the property was last given.
@MainActor
final class LiveQueryObserver<Value: Sendable & Equatable>: ObservableObject {
    /// The latest value read, by whichever query read it.
    @Published private(set) var value: Value?
    /// The last query to produce a value or a failure, and the failure.
    @Published private var settled: (identity: LiveQueryIdentity, error: (any Error)?)?

    /// What the running observation follows; `nil` before the first.
    private var following: (identity: LiveQueryIdentity, database: Database?)?
    /// The running observation. Kept apart from what it follows because
    /// `deinit`, which cancels it, may only touch `Sendable` state.
    private var observation: Task<Void, Never>?

    deinit {
        observation?.cancel()
    }

    /// Where the query `identity` stands: loading until it is the one that
    /// last settled. Computed from what was published, so a view sees a
    /// changed query as loading in the same update that changed it.
    func state(of identity: LiveQueryIdentity) -> LiveQueryState {
        guard let settled, settled.identity == identity else { return .loading }
        return settled.error.map(LiveQueryState.failed) ?? .live
    }

    /// Follows `request` on `database`, starting a new observation when
    /// either differs from what is being followed. Called on every view
    /// update, so it publishes nothing itself: the observation's task does,
    /// after the update.
    func observe(_ request: LiveQueryRequest<Value>, in database: Database?) {
        if let following, following.identity == request.identity, following.database === database { return }
        following = (request.identity, database)
        observation?.cancel()
        // The task holds this object only while it publishes, so an
        // observation does not keep alive the view state it belongs to.
        observation = Task { [weak self] in
            do {
                guard let database else {
                    throw RelateError.misuse(
                        "@LiveQuery found no database in the environment: add .relateDatabase(_:) above this view")
                }
                for try await value in database.values(tags: request.identity.tags, request.read) {
                    self?.settle(request.identity, value: value)
                }
            } catch {
                self?.settle(request.identity, error: error)
            }
        }
    }

    /// What an observation reports: a value, or the error that ended it.
    /// Only a change is published, so a read that returns what the property
    /// already holds redraws nothing. An observation that was cancelled
    /// (the query changed, the view went away) reports nothing further.
    private func settle(_ identity: LiveQueryIdentity, value: Value? = nil, error: (any Error)? = nil) {
        guard !Task.isCancelled else { return }
        if let value, value != self.value { self.value = value }
        if settled?.identity != identity || (settled?.error == nil) != (error == nil) {
            settled = (identity, error)
        }
    }
}
#endif
