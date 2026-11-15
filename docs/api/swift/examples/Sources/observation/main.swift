import Foundation
import Relate
import os

let db = try Database.inMemory()

// A trace handler sees every statement on every connection of the database,
// with its timings. A trace has parameter names, not values. Its statement
// text holds any literal spliced into it, and its error may quote a value:
// log those two only when they carry no user data, as here, where every
// input is bound.
let slowest = OSAllocatedUnfairLock(initialState: [String: Double]())
db.setTraceHandler { trace in
    slowest.withLock { $0[trace.statement] = max($0[trace.statement] ?? 0, trace.wallMs) }
    if let error = trace.error { print("failed:", trace.statement, "-", error.message) }
}

// A commit stream reports each committed write, with the tags its writer
// attached. Each call to commits() is a new, independent stream.
let commits = db.commits()

let secret = "a-private-note"
try await db.write(tags: ["notes"]) { tx in
    try tx.run("CREATE (:Note {text: \(secret)})")
    try tx.run("CREATE (:Note {text: \("second")})")
}
try await db.write("CREATE (:Tag {name: 'favorites'})", tags: ["tags"])
_ = try await db.read("MATCH (n:Note) RETURN count(n)")  // a read commits nothing

// Commits arrive in order and wait in the stream until consumed. (An app
// would loop for as long as its view lives; this example stops after two.)
var seen: [Commit] = []
for await commit in commits {
    print("commit:", commit.bookmark, commit.tags.sorted(), "nodes:", commit.stats.nodesCreated)
    seen.append(commit)
    if seen.count == 2 { break }
}
precondition(seen[0].tags == ["notes"] && seen[0].stats.nodesCreated == 2 && seen[0].stats.propertiesSet == 2)
precondition(seen[1].tags == ["tags"] && seen[1].stats.nodesCreated == 1)

// An observer reacts to the commits that concern it.
let noteCommits = seen.filter { $0.tags.contains("notes") }
precondition(noteCommits.count == 1)

// A failed statement is traced too.
do {
    _ = try await db.read("RETURN 1/0")
    preconditionFailure("the division must fail")
} catch {}

let traced = slowest.withLock { $0 }
print("statements traced:", traced.count)
precondition(traced.keys.contains("CREATE (:Note {text: $_p0})"))
precondition(traced.keys.contains("RETURN 1/0"))
precondition(!traced.keys.contains { $0.contains(secret) })

db.setTraceHandler(nil)
