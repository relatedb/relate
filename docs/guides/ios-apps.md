# Relate in an iOS app

This guide covers the decisions every app that embeds Relate makes: where
the store lives, how it gets there, how views show and change its data, and
what to do when the app goes to the background. It uses the Swift client
([spec](../api/swift/spec.md)); the snippets follow one small library app.
The repository's [`examples/ios-snotel`](../../examples/ios-snotel/) is a
complete app on the same API; it loads its data from bundled CSV where this
guide copies a store.

## Add the package

```swift
.package(url: "https://github.com/relatedb/relate", from: "<release>")
// target: .product(name: "Relate", package: "relate")
```

The engine is a prebuilt static library linked into the app. There is no
server and no network.

## Put the store in Application Support

`Database.defaultStoreURL(named:)` is the place for a store the app manages:
Application Support, which is backed up and which the user never browses.
A name ending in `.rl` is a single-file store.

An app that ships data copies it there on first launch. A store inside the
app bundle cannot be opened where it lies, because the bundle is read-only
and a store writes beside itself.

```swift
func openLibrary() async throws -> Database {
    let storeURL = try Database.defaultStoreURL(named: "library.rl")
    if let bundled = Bundle.main.url(forResource: "books", withExtension: "rl") {
        try Database.provision(at: storeURL, from: bundled)
    }
    let db = try Database(url: storeURL)
    try await schema.migrate(db)
    return db
}
```

`provision` copies only when nothing is at the target, so it is safe on every
launch. The copy is atomic, writable, and on iOS protected with
`completeUntilFirstUserAuthentication`, which lets the app write while the
device is locked after its first unlock. An app that starts empty skips
`provision`; `Database(url:)` creates the store.

## Declare the schema once

A `Migrator` holds what this build expects of the store. Index and
constraint statements run on every launch and must be idempotent; versioned
steps run once each, in order, and a step commits together with the record
of its version.

```swift
let schema: Migrator = {
    var migrator = Migrator()
    migrator.schema = [
        "CREATE CONSTRAINT book_key IF NOT EXISTS FOR (b:Book) REQUIRE b.key IS UNIQUE",
        "CREATE POINT INDEX place_location IF NOT EXISTS FOR (p:Place) ON (p.location)",
    ]
    migrator.register(version: 1, ["MATCH (b:Book) WHERE b.status IS NULL SET b.status = 'want'"])
    return migrator
}()
```

## Open once and share

A `Database` is `Sendable`. Open it once at launch, keep the one object, and
put it in the SwiftUI environment at the root of the view tree. It owns the
connections behind every read and write: one writer and a few readers, each
on its own queue.

```swift
@main
struct LibraryApp: App {
    @State private var db: Database?
    @State private var failure: String?

    var body: some Scene {
        WindowGroup {
            Group {
                if let db {
                    LibraryView().relateDatabase(db)
                } else if let failure {
                    Text(failure)
                } else {
                    ProgressView()
                }
            }
            .task {
                do { db = try await openLibrary() } catch { failure = error.localizedDescription }
            }
        }
    }
}
```

Show the views that read only once the database is open: a live query that
finds no database in its environment reports that as its error.

Do not open the same store twice. A second writable open fails while the
first is alive.

## Show data with a live query

A view declares the statement it shows. `@LiveQuery` runs it against the
database in the environment, off the main thread, and SwiftUI redraws the
view when a commit changes what the statement returns. There is no reload
to call and no view model to hold the rows.

```swift
struct Book: Decodable, Sendable, Identifiable, Equatable {
    let key: String
    let title: String
    let rating: Int?
    let finishedAt: Date?
    var id: String { key }
}

struct LibraryView: View {
    @LiveQuery(
        """
        MATCH (b:Book)
        RETURN b.key AS key, b.title AS title, b.rating AS rating, b.finished_at AS finishedAt
        ORDER BY b.title
        """)
    private var books: [Book]

    var body: some View {
        List(books) { book in
            Text(book.title)
        }
        .overlay {
            if $books.isLoading {
                ProgressView()
            } else if let error = $books.error {
                Text(error.localizedDescription)
            }
        }
    }
}
```

Three things to notice:

- **Records decode.** Each record becomes a `Book` by column name. Decoding
  is strict: a column of the wrong kind is an error naming the column, and
  it arrives in `$books.error`, not as a crash.
- **The property starts empty.** `books` is `[]` until the first read
  returns, and `$books.isLoading` says so. After that the last value stays
  on screen while a later read runs. It also stays after a read fails,
  which ends the query (see [the limits](#limits-to-design-around)).
- **A read keeps nothing.** The statement runs in a transaction that is
  rolled back; a write statement in a live query is refused. Reads do not
  wait for a write in progress.

## Queries that depend on input

A query that depends on the view's input is built in the view's
initializer. The statement, with the values bound into it, is the query's
identity: when it changes, the live query starts again on the new one.

```swift
struct SearchResults: View {
    @LiveQuery private var books: [Book]

    init(search: String) {
        _books = LiveQuery(
            try """
            MATCH (b:Book) WHERE toLower(b.title) CONTAINS toLower(\(search))
            RETURN b.key AS key, b.title AS title, b.rating AS rating, b.finished_at AS finishedAt
            ORDER BY b.title LIMIT 50
            """)
    }

    var body: some View {
        List(books) { Text($0.title) }
    }
}
```

**Interpolation binds.** `\(search)` becomes a parameter; it is never part
of the statement text. Binding can fail (a value with no Cypher form), which
is why the literal is marked `try`; the initializer does not have to throw,
because a statement that cannot be built is reported through `$books.error`
like any other failure.

A value that is not a list of records uses the closure form, which runs any
read and names its own starting value:

```swift
struct LibraryCount: View {
    @LiveQuery(initial: 0, { tx in try tx.run("MATCH (b:Book) RETURN count(b)").scalar(as: Int.self) })
    private var count: Int

    var body: some View {
        Text("\(count) books")
    }
}
```

## Write from an action

A write is an `async` call on the database. A view takes the database from
the environment; nothing has to tell the views that show the data.

```swift
struct FinishButton: View {
    let book: Book
    @Environment(\.relateDatabase) private var db

    var body: some View {
        Button("Finished") {
            Task { try? await finish(book, rating: 5, in: db) }
        }
    }
}

func finish(_ book: Book, rating: Int, at date: Date = Date(), in db: Database?) async throws {
    try await db?.write(tags: ["books"]) { tx in
        try tx.run("MATCH (b:Book {key: \(book.key)}) SET b.rating = \(rating), b.finished_at = \(date)")
    }
}
```

A `write` is one transaction. If the closure throws, nothing it did is kept.
When it commits, the live queries on screen are read again, and a view whose
answer changed is redrawn.

## Which commits re-run a query

By default a live query is read again after **every** commit made through
the database, and a result equal to the one on screen changes nothing. That
is always correct, and for the statements a screen shows it is cheap.

The engine does not report which labels a commit touched, so narrowing is
the app's job: `tags:` on a query makes it follow only commits that carry
one of those tags.

```swift
struct TaggedLibrary: View {
    @LiveQuery("MATCH (b:Book) RETURN b.key AS key, b.title AS title, b.rating AS rating, b.finished_at AS finishedAt", tags: ["books"])
    private var books: [Book]

    var body: some View {
        List(books) { Text($0.title) }
    }
}
```

Use tags only where every writer of that data sets them. A write that
forgets its tag (`finish` above tags `"books"`; an untagged write would not)
leaves a tagged query stale until the next tagged commit.

## Outside a view

The same observation is available without SwiftUI. `values` is an
asynchronous sequence of what a read returns: the current value first, then
a fresh one after each commit that changed it. It subscribes before its
first read, so no write made after the call is missed.

```swift
@MainActor @Observable
final class ShelfModel {
    private(set) var titles: [String] = []

    /// Run it for as long as the model is in use: `.task { await model.follow(db) }`.
    func follow(_ db: Database) async {
        let latest = db.values { tx in
            try tx.run("MATCH (b:Book) RETURN b.title AS title ORDER BY title").map {
                try $0.get("title", as: String.self)
            }
        }
        do {
            for try await titles in latest { self.titles = titles }
        } catch {
            titles = []
        }
    }
}
```

One-off reads and writes are `try await db.read { tx in … }` and
`try await db.write { tx in … }`. Underneath all of it, `db.commits()` is
the stream of committed writes, with their tags and counters, for code that
wants the events themselves.

## Dates, embeddings and places

Platform types bind and read back directly.

```swift
func addPlace(_ db: Database, name: String, at coordinate: CLLocationCoordinate2D) async throws {
    try await db.write(tags: ["places"]) { tx in
        try tx.run("MERGE (p:Place {name: \(name)}) SET p.location = \(coordinate), p.added_at = \(Date())")
    }
}

func places(_ db: Database, within metres: Double, of here: CLLocationCoordinate2D) async throws -> [String] {
    try await db.read { tx in
        try tx.run(
            """
            MATCH (p:Place) WHERE point.distance(p.location, \(here)) < \(metres)
            RETURN p.name AS name ORDER BY name
            """
        ).map { try $0.get("name", as: String.self) }
    }
}

func storeEmbedding(_ db: Database, key: String, embedding: [Float]) async throws {
    try await db.write { tx in
        try tx.run("MATCH (b:Book {key: \(key)}) SET b.embedding = \(CypherValue(vector: embedding))")
    }
}
```

- A `Date` is stored as a `DATETIME` in UTC, to the microsecond, and reads
  back as the same instant. The engine's own clock is UTC, so pass the
  user's date in rather than calling `date()` in Cypher when the local
  calendar day matters: `CypherValue.Date(date, in: .current)`.
- A `[Float]` is stored as a `FLOAT32` vector only through
  `CypherValue(vector:)`; a bare array binds as a list. A Core ML output
  converts with `CypherValue.Vector(multiArray)`.
- A `CLLocationCoordinate2D` is stored as a WGS 84 point, and
  `point.coordinate` reads it back.
- These conversions check what they are given and throw
  `RelateError.invalidParameter` before anything reaches the engine: for a
  non-finite date, for a vector with a NaN, an infinity or more than 4,096
  coordinates, and for a coordinate off the globe. Core Location marks an
  unknown position with an invalid coordinate
  (`kCLLocationCoordinate2DInvalid`), which is refused too.

## Checkpoint when the app leaves the foreground

Every committed write is already durable in the store's log. A checkpoint
folds the log into a snapshot, so the next launch opens quickly instead of
replaying it. Checkpoint after a bulk load and when the scene goes to the
background.

```swift
struct LibraryScene: Scene {
    let db: Database
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup { LibraryView().relateDatabase(db) }
            .onChange(of: scenePhase) { _, phase in
                if phase == .background {
                    Task { try? await db.checkpointInBackground() }
                }
            }
    }
}
```

## See what the app is doing

`setTraceHandler` reports every statement with its timings, the reads of
the live queries included. A trace has parameter names, not values. Its
statement text holds any literal spliced into it, and its error may quote a
value: log those two only when they carry no user data, as here, where
every input is bound.

```swift
func logSlowStatements(_ db: Database) {
    db.setTraceHandler { trace in
        if trace.wallMs > 50 { print("slow (\(trace.wallMs) ms):", trace.statement) }
    }
}
```

Each statement is also a signpost interval in Instruments (subsystem
`ai.relatedb.relate`, category `Query`).

## Limits to design around

- **One writer per store.** An app and its extension that share a store keep
  it in their App Group container as a store directory (a name without
  `.rl`). Only one of them opens it writable; the other opens it with
  `readOnly: true` and sees later commits when it opens the store again. A
  single-file `.rl` store cannot be shared this way: it does not open
  read-only while a writer has it open.
- **A running statement cannot be cancelled.** Cancelling a task stops work
  that has not started. Keep statements bounded (`LIMIT`) where a user can
  type faster than they finish.
- **Bodies run more than once under `retries`.** A write that passes
  `retries` must be safe to repeat.
- **Observation is in-process.** Another process's writes do not appear in
  `commits()` and do not re-run a live query.
- **A read that throws ends its live query.** The property keeps its last
  value, `$property.error` holds the error, and later commits are not
  followed. The query starts again when its statement (or `id`) changes or
  the environment's database does. Show the error, and write statements
  that hold for any data the app can store: a `Decodable` type with a
  non-optional property fails on a node that lacks it.
- **A live query re-runs its whole statement.** There are no incremental
  updates. Keep the statements a screen shows bounded (`LIMIT`), and narrow
  with `tags:` only where every writer sets them.
- **The engine is a large static library.** It adds tens of megabytes to
  the app's executable; measure a release build before setting a size
  budget.
