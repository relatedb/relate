import Foundation
import Relate

// A stand-in for an app bundle and its Application Support directory.
let scratch = FileManager.default.temporaryDirectory
    .appendingPathComponent("relate-example-\(UUID().uuidString)", isDirectory: true)
defer { try? FileManager.default.removeItem(at: scratch) }
let bundled = scratch.appendingPathComponent("Bundle/books.rl")
try FileManager.default.createDirectory(
    at: bundled.deletingLastPathComponent(), withIntermediateDirectories: true)
do {
    let seed = try Database(url: bundled)
    try seed.connect().query("CREATE (:Book {title: 'Dune'}), (:Book {title: 'Emma', status: 'read'})")
    try seed.checkpoint()
}
try FileManager.default.setAttributes([.posixPermissions: 0o444], ofItemAtPath: bundled.path)  // bundles are read-only

// In an app: let storeURL = try Database.defaultStoreURL(named: "library.rl")
let storeURL = scratch.appendingPathComponent("Application Support/library.rl")

// First launch copies the bundled store into place; later launches find it.
let copied = try Database.provision(at: storeURL, from: bundled)
let copiedAgain = try Database.provision(at: storeURL, from: bundled)
print("first launch copied:", copied, "- second launch copied:", copiedAgain)
precondition(copied && !copiedAgain)

// The schema this build expects: idempotent statements run every launch,
// versioned steps run once each.
var migrator = Migrator()
migrator.schema = [
    "CREATE CONSTRAINT tag_name IF NOT EXISTS FOR (t:Tag) REQUIRE t.name IS UNIQUE",
    "CREATE INDEX book_status IF NOT EXISTS FOR (b:Book) ON (b.status)",
]
migrator.register(version: 1, ["MATCH (b:Book) WHERE b.status IS NULL SET b.status = 'want'"])
migrator.register(version: 2) { tx in
    try tx.run("CREATE (:Tag {name: \("Favorites")})")
}

let db = try Database(url: storeURL)
let version = try await migrator.migrate(db)
print("schema version:", version)
precondition(version == 2)

let statuses = try await db.read("MATCH (b:Book) RETURN b.status ORDER BY b.title")
    .map { try $0.get(0, as: String.self) }
precondition(statuses == ["want", "read"])

// The next launch: nothing to do, and the one-time step does not repeat.
let again = try await migrator.migrate(db)
let tags = try await db.read("MATCH (t:Tag) RETURN count(t)").scalar(as: Int.self)
precondition(again == 2 && tags == 1)

// A later build adds a step; only that step runs.
migrator.register(version: 3, ["MATCH (t:Tag) SET t.color = 'yellow'"])
let upgraded = try await migrator.migrate(db)
print("after the next build:", upgraded)
precondition(upgraded == 3)

// Checkpoint when the app goes to the background, off the main thread.
try await db.checkpointInBackground()
