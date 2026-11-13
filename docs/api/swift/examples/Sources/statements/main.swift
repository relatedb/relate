import Foundation
import Relate

struct Book: Codable, Equatable {
    var key: String
    var title: String
    var firstPublishYear: Int?
    var subjects: [String]
    var addedOn: Date
}

let conn = try Database.inMemory().connect()
let added = try Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse("2026-11-08T13:00:00.123Z")
let book = Book(
    key: "/works/OL1W", title: "The Left Hand of Darkness", firstPublishYear: 1969,
    subjects: ["Gethen", "Hainish Cycle"], addedOn: added)

// An Encodable model binds as one map parameter. An interpolated value in a
// Statement literal is bound, never spliced into the text.
let props = try CypherValue(encoding: book, keyStrategy: .snakeCase)
let create: Statement = try "MERGE (b:Book {key: \(book.key)}) SET b += \(props) RETURN b.title"
print(create.cypher)
precondition(create.cypher == "MERGE (b:Book {key: $_p0}) SET b += $_p1 RETURN b.title")
precondition(create.parameters["_p0"] == .string(book.key))
try conn.run(create)

// Hostile input stays data.
let hostile = "x'}) DETACH DELETE b //"
let deleted = try conn.run("MATCH (b:Book {key: \(hostile)}) DETACH DELETE b RETURN count(b)").scalar(as: Int.self)
precondition(deleted == 0)

// Labels and property keys are identifiers, not values; trusted text is raw.
let label = "Subject"
for name in book.subjects {
    try conn.run(
        "MATCH (b:Book {key: \(book.key)}) MERGE (s:\(identifier: label) {name: \(name)}) MERGE (b)-[:ABOUT]->(s)")
}

// A statement embeds another with its parameters; names are assigned once,
// on the whole statement.
let minimumYear = 1960
let filter: Statement = try "b.first_publish_year >= \(minimumYear)"
let order = "DESC"
let page: Statement = try """
    MATCH (b:Book) WHERE \(filter)
    RETURN b.key AS key, b.title AS title, b.first_publish_year AS firstPublishYear,
           [(b)-[:ABOUT]->(s) | s.name] AS subjects, b.added_on AS addedOn
    ORDER BY b.title \(raw: order) LIMIT \(20)
    """
precondition(page.parameters == ["_p0": 1960, "_p1": 20])

// Records decode into the model by column name…
var decoded = try conn.run(page).decode(Book.self)
decoded[0].subjects.sort()
print("decoded:", decoded[0].title, decoded[0].subjects)
precondition(decoded == [book])

// …and a node decodes by its property keys.
struct Stored: Decodable {
    let title: String
    let firstPublishYear: Int
    let addedOn: Date
}
let stored = try conn.query("MATCH (b:Book) RETURN b")
    .decode(Stored.self, column: "b", using: CypherValueDecoder(keyStrategy: .snakeCase))
precondition(stored[0].firstPublishYear == 1969 && stored[0].addedOn == added)

// Decoding is strict and says where it failed.
struct Wrong: Decodable { let firstPublishYear: String }
do {
    _ = try conn.run(page).decode(Wrong.self)
    preconditionFailure("an integer is not a String")
} catch {
    guard case .unexpectedResult(let mismatch) = error else { throw error }
    print("mismatch:", mismatch.message)
    precondition(mismatch.path == ["firstPublishYear"] && mismatch.found == "int")
}
