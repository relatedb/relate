import Foundation
import Relate

struct Book: Decodable, Sendable, Equatable {
    let title: String
    let year: Int
}

// One Database, shared. It opens and owns the connections behind read and
// write: a single writer and up to `maxReaders` readers.
let db = try Database.inMemory(options: .init(maxReaders: 2))

// A write is one transaction, committed when the body returns. The body is
// synchronous: it runs on the writer connection's own queue while this task
// is suspended.
let created = try await db.write { tx in
    try tx.run("CREATE (:Book {title: \("Dune"), year: \(1965)})")
    try tx.run("CREATE (:Book {title: \("Emma"), year: \(1815)})")
    return try tx.query("MATCH (b:Book) RETURN count(b)").scalar(as: Int.self)
}
precondition(created == 2)

// A read sees one snapshot and keeps nothing.
let books = try await db.read { tx in
    try tx.run("MATCH (b:Book) RETURN b.title AS title, b.year AS year ORDER BY year").decode(Book.self)
}
print("books:", books.map(\.title))
precondition(books == [Book(title: "Emma", year: 1815), Book(title: "Dune", year: 1965)])

// One statement needs no closure.
let after = 1900
let recent = try await db.read(try "MATCH (b:Book) WHERE b.year > \(after) RETURN b.title").scalar(as: String.self)
precondition(recent == "Dune")
try await db.write(try "MATCH (b:Book {title: \("Emma")}) SET b.shelf = \("read")")

// A read does not wait for a write in progress, and does not see what that
// write has not committed.
let inside = DispatchSemaphore(value: 0)
let finish = DispatchSemaphore(value: 0)
async let slowWrite: Void = db.write { tx in
    try tx.query("CREATE (:Book {title: 'Uncommitted', year: 2026})")
    inside.signal()
    finish.wait()
}
await withCheckedContinuation { continuation in
    DispatchQueue.global().async {
        inside.wait()
        continuation.resume()
    }
}
let during = try await db.read("MATCH (b:Book) RETURN count(b)").scalar(as: Int.self)
print("books seen during the open write:", during)
precondition(during == 2)
finish.signal()
try await slowWrite
let afterCommit = try await db.read("MATCH (b:Book) RETURN count(b)").scalar(as: Int.self)
precondition(afterCommit == 3)

// Writes through the database run one at a time, so they never conflict
// with each other however many tasks issue them.
try await db.write("CREATE (:Counter {v: 0})")
try await withThrowingTaskGroup(of: Void.self) { group in
    for _ in 0..<20 {
        group.addTask { try await db.write("MATCH (c:Counter) SET c.v = c.v + 1") }
    }
    try await group.waitForAll()
}
let counter = try await db.read("MATCH (c:Counter) RETURN c.v").scalar(as: Int.self)
print("counter:", counter)
precondition(counter == 20)

// A write in a read is refused, and was never going to be kept.
do {
    _ = try await db.read("CREATE (:Book {title: 'Sneaky', year: 1})")
    preconditionFailure("a write in a read must be refused")
} catch {
    guard case RelateError.misuse(let message) = error else {
        preconditionFailure("expected a misuse, got \(error)")
    }
    print("refused:", message)
}
let total = try await db.read("MATCH (b:Book) RETURN count(b)").scalar(as: Int.self)
precondition(total == 3)
