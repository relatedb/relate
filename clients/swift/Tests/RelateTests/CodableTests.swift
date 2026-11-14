import Foundation
import Testing

import Relate

private struct Book: Codable, Equatable {
    var key: String
    var title: String
    var firstPublishYear: Int?
    var rating: Double?
    var subjects: [String]
    var coverURL: URL?
    var addedOn: Date
    var shelf: Shelf
    var edition: Edition?

    enum Shelf: String, Codable { case want, reading, read }
    struct Edition: Codable, Equatable {
        var pages: Int
        var format: String
    }
}

private struct Embedded: Decodable, Equatable {
    let key: String
    let embedding: [Float]
}

@Suite struct CodableTests {
    let conn = try! Database.inMemory().connect()

    private func sample() throws -> Book {
        Book(
            key: "/works/OL1W", title: "The Left Hand of Darkness", firstPublishYear: 1969, rating: nil,
            subjects: ["Gethen", "Hainish Cycle"], coverURL: URL(string: "https://example.com/c.jpg"),
            addedOn: try Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse("2026-11-08T13:00:00.123Z"),
            shelf: .read, edition: .init(pages: 304, format: "paperback"))
    }

    @Test func aModelEncodesAsAMap() throws {
        let value = try CypherValue(encoding: try sample())
        let map = try #require(value.mapValue)
        #expect(map["title"] == "The Left Hand of Darkness" && map["firstPublishYear"] == 1969)
        // A nil optional is left out: `SET n += $props` would remove a key held as null.
        #expect(map["rating"] == nil && map.keys.contains("rating") == false)
        #expect(map["subjects"] == ["Gethen", "Hainish Cycle"] && map["shelf"] == "read")
        // Convertible types encode as their Cypher kinds, not as Codable writes them.
        #expect(map["coverURL"] == "https://example.com/c.jpg" && map["addedOn"]?.kind == .dateTime)
        #expect(map["edition"] == ["pages": 304, "format": "paperback"])
        // Snake-case keys for a graph that spells its properties that way.
        let snake = try #require(CypherValue(encoding: try sample(), keyStrategy: .snakeCase).mapValue)
        #expect(snake["first_publish_year"] == 1969 && snake["cover_url"] != nil && snake["added_on"] != nil)
        #expect(snake["firstPublishYear"] == nil)
    }

    @Test func aModelWritesAsPropertiesAndDecodesFromItsNode() throws {
        var book = try sample()
        book.edition = nil  // a nested map is not a property value
        try conn.run("CREATE (b:Book) SET b += \(try CypherValue(encoding: book, keyStrategy: .snakeCase))")
        #expect(try conn.query("MATCH (b:Book) RETURN b.first_publish_year").scalar() == 1969)
        let decoder = CypherValueDecoder(keyStrategy: .snakeCase)
        let fromNode = try conn.query("MATCH (b:Book) RETURN b").decode(Book.self, column: "b", using: decoder)
        #expect(fromNode == [book])
        // The same model from projected columns.
        let fromColumns = try conn.query(
            """
            MATCH (b:Book)
            RETURN b.key AS key, b.title AS title, b.first_publish_year AS firstPublishYear,
                   b.rating AS rating, b.subjects AS subjects, b.cover_url AS coverURL,
                   b.added_on AS addedOn, b.shelf AS shelf
            """
        ).decode(Book.self)
        #expect(fromColumns == [book])
        #expect(try conn.query("MATCH (b:Book) RETURN b").single().decode(Book.self, column: "b", using: decoder) == book)
    }

    @Test func nestedContainersRoundTripThroughTheEngine() throws {
        let book = try sample()
        let value = try CypherValue(encoding: [book, book])
        let back = try conn.query("RETURN $books AS books", ["books": value]).single()
        #expect(try back.decode([Book].self, column: "books") == [book, book])
        #expect(try CypherValueDecoder().decode([Book].self, from: value) == [book, book])
        // A relationship decodes by its properties too.
        struct Tagged: Decodable, Equatable { let on: Date }
        let when = book.addedOn
        try conn.run("CREATE (:A)-[:TAGGED {on: \(when)}]->(:B)")
        #expect(try conn.query("MATCH ()-[r:TAGGED]->() RETURN r").decode(Tagged.self, column: "r") == [Tagged(on: when)])
    }

    @Test func aVectorDecodesAsAnArrayOfFloats() throws {
        let floats: [Float] = [0.25, 0.5, 0.75]
        try conn.run("CREATE (:E {key: 'k', embedding: \(CypherValue(vector: floats))})")
        let rows = try conn.query("MATCH (e:E) RETURN e.key AS key, e.embedding AS embedding").decode(Embedded.self)
        #expect(rows == [Embedded(key: "k", embedding: floats)])
    }

    @Test func singleValueTypesDecodeFromAOneColumnRow() throws {
        let result = try conn.query("UNWIND ['a', 'b'] AS s RETURN s")
        #expect(try result.decode(String.self) == ["a", "b"])
        #expect(try conn.query("RETURN [1, 2, 3] AS xs").decode([Int].self) == [[1, 2, 3]])
        #expect(try conn.query("RETURN null AS x").decode(String?.self) == [nil])
        let failure = try resultMismatch { _ = try conn.query("RETURN 1 AS a, 2 AS b").decode(Int.self) }
        #expect(failure.expected == "Int" && failure.found == "a record of 2 columns")
    }

    @Test func aDecodeFailureNamesThePath() throws {
        // A missing column.
        var failure = try resultMismatch { _ = try conn.query("RETURN 'k' AS key").decode(Embedded.self) }
        #expect(failure.path == ["embedding"] && failure.found == "no such key")
        // The wrong kind, strictly: a string is not parsed into a number.
        struct Counted: Decodable { let n: Int }
        failure = try resultMismatch { _ = try conn.query("RETURN '4' AS n").decode(Counted.self) }
        #expect(failure.path == ["n"] && failure.expected == "Int" && failure.found == "string")
        // Deep inside a column: column, key, list position.
        struct Shelf: Decodable { let books: [Counted] }
        failure = try resultMismatch {
            _ = try conn.query("RETURN {books: [{n: 1}, {n: 'two'}]} AS shelf").decode(Shelf.self, column: "shelf")
        }
        #expect(failure.path == ["shelf", "books", "[1]", "n"] && failure.found == "string")
        // A struct from something that is not a map.
        failure = try resultMismatch { _ = try conn.query("RETURN 5 AS shelf").decode(Shelf.self, column: "shelf") }
        #expect(failure.path == ["shelf"] && failure.expected == "a map, node or relationship" && failure.found == "int")
        // A raw value outside a Codable enum, reported by the enum's own decoding.
        failure = try resultMismatch {
            _ = try conn.query(
                "RETURN 'k' AS key, 't' AS title, [] AS subjects, datetime() AS addedOn, 'burned' AS shelf"
            ).decode(Book.self)
        }
        #expect(failure.path == ["shelf"] && failure.found == "a value the type refuses")
        #expect(failure.reason?.contains("burned") == true)
    }

    @Test func anErrorFromATypesOwnDecodingIsAMismatch() throws {
        struct Unhappy: Error {}
        struct Picky: Decodable {
            enum Keys: String, CodingKey { case n }
            init(from decoder: any Decoder) throws {
                let container = try decoder.container(keyedBy: Keys.self)
                let n = try container.decode(Int.self, forKey: .n)
                guard n > 0 else {
                    throw DecodingError.typeMismatch(
                        UInt.self,
                        .init(codingPath: container.codingPath + [Keys.n], debugDescription: "\(n) is not positive"))
                }
                guard n < 10 else { throw Unhappy() }
            }
        }
        let decoder = CypherValueDecoder()
        _ = try decoder.decode([Picky].self, from: [["n": 1]])
        var failure = try resultMismatch { _ = try decoder.decode([Picky].self, from: [["n": 1], ["n": 0]]) }
        #expect(failure.path == ["[1]", "n"] && failure.expected == "UInt")
        #expect(failure.found == "a value of another type" && failure.reason == "0 is not positive")
        // An error that is not a `DecodingError` names no path of its own.
        failure = try resultMismatch { _ = try decoder.decode(Picky.self, from: ["n": 10]) }
        #expect(failure.path.isEmpty && failure.found == "an error from the type's decoding")
        #expect(failure.reason?.contains("Unhappy") == true)
    }

    @Test func anEncodeFailureIsAnInvalidParameter() throws {
        struct Big: Encodable { let n: UInt64 }
        let message = try invalidParameterMessage { _ = try CypherValue(encoding: Big(n: .max)) }
        #expect(message.contains("does not fit"))
        struct Refuses: Encodable {
            func encode(to encoder: any Encoder) throws {
                throw EncodingError.invalidValue(
                    0, .init(codingPath: [], debugDescription: "nothing to write"))
            }
        }
        let refused = try #require(throws: RelateError.self) { _ = try CypherValue(encoding: Refuses()) }
        #expect(refused.message.contains("nothing to write"))
    }

    @Test func keysConvertBetweenCamelAndSnakeCase() throws {
        struct Keys: Codable, Equatable {
            var readingStatus = "want"
            var coverURL = "u"
            var myURLProperty = "p"
            var plain = "x"
            var ol2Key = "k"
        }
        let map = try #require(CypherValue(encoding: Keys(), keyStrategy: .snakeCase).mapValue)
        #expect(Set(map.keys) == ["reading_status", "cover_url", "my_url_property", "plain", "ol2_key"])
        struct Read: Decodable, Equatable {
            let readingStatus: String
            let plain: String
            let _leading: Int
        }
        let value: CypherValue = ["reading_status": "read", "plain": "x", "_leading": 1]
        #expect(
            try CypherValueDecoder(keyStrategy: .snakeCase).decode(Read.self, from: value)
                == Read(readingStatus: "read", plain: "x", _leading: 1))
    }

    @Test func scalarsAndEmptyValuesEncode() throws {
        struct Empty: Encodable {}
        #expect(try CypherValue(encoding: Empty()) == .map([:]))
        #expect(try CypherValue(encoding: 7) == 7)
        #expect(try CypherValue(encoding: [String]()) == .list([]))
        #expect(try CypherValue(encoding: Optional<Int>.none) == .null)
        struct Explicit: Encodable {
            enum Keys: String, CodingKey { case gone, list, k }
            func encode(to encoder: any Encoder) throws {
                var container = encoder.container(keyedBy: Keys.self)
                try container.encodeNil(forKey: .gone)
                var list = container.nestedUnkeyedContainer(forKey: .list)
                try list.encode(1)
                try list.encodeNil()
                var inner = list.nestedContainer(keyedBy: Keys.self)
                try inner.encode("v", forKey: .k)
            }
        }
        #expect(try CypherValue(encoding: Explicit()) == ["gone": nil, "list": [1, nil, ["k": "v"]]])
    }
}
