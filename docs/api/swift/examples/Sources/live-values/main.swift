import Foundation
import Relate

struct Book: Decodable, Sendable, Equatable {
    let title: String
}

let db = try Database.inMemory()
try await db.write("CREATE (:Book {title: 'Dune'})", tags: ["books"])

/// Follows the titles through two writes and returns what the stream
/// yielded. The observation ends when this function returns and its stream
/// is released.
func followTitles(in db: Database) async throws -> [[String]] {
    // A live value is a read that runs again after a commit. The stream
    // yields the current value first, then one per change. It listens for
    // commits from before its first read, so no write made after this line
    // is missed.
    let titles = db.values { tx in
        try tx.run("MATCH (b:Book) RETURN b.title AS title ORDER BY title").decode(Book.self).map(\.title)
    }
    var seen: [[String]] = []
    for try await current in titles {
        print("books:", current)
        seen.append(current)
        switch seen.count {
        case 1:
            // A write through the database is followed by a fresh value.
            try await db.write("CREATE (:Book {title: 'Emma'})", tags: ["books"])
        case 2:
            // A commit that leaves the answer as it was yields nothing: the
            // value is Equatable, and a repeat is skipped. The next value
            // is the one after the write that follows.
            try await db.write("CREATE (:Shelf {name: 'unsorted'})")
            try await db.write("CREATE (:Book {title: 'Kindred'})", tags: ["books"])
        default:
            return seen
        }
    }
    return seen
}

let seen = try await followTitles(in: db)
precondition(seen == [["Dune"], ["Dune", "Emma"], ["Dune", "Emma", "Kindred"]])

/// Follows the book count, but only through commits tagged "books".
func followTaggedCount(in db: Database) async throws -> [Int] {
    // With tags, only a commit that carries one of them is followed by a
    // read. Narrow this way only where every writer tags its writes: the
    // untagged write below is not seen until a tagged one follows.
    let counts = db.values(tags: ["books"]) { tx in
        try tx.run("MATCH (b:Book) RETURN count(b)").scalar(as: Int.self)
    }
    var counted: [Int] = []
    for try await count in counts {
        print("count:", count)
        counted.append(count)
        guard counted.count == 1 else { break }
        try await db.write("CREATE (:Book {title: 'Untagged'})")
        try await db.write("CREATE (:Book {title: 'Tagged'})", tags: ["books"])
    }
    return counted
}

let counted = try await followTaggedCount(in: db)
precondition(counted == [3, 5])

// In a SwiftUI app the same observation is a property:
//
//     @LiveQuery("MATCH (b:Book) RETURN b.title AS title ORDER BY title")
//     private var books: [Book]
//
// with `.relateDatabase(db)` set once near the root of the view tree.
