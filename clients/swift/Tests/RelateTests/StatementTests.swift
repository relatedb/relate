import Foundation
import Testing

import Relate

@Suite struct StatementTests {
    let conn = try! Database.inMemory().connect()

    @Test func anInterpolatedValueIsBoundNotSpliced() throws {
        let key = "/works/OL59800W"
        let rating = 4
        let statement: Statement = try "MERGE (b:Book {key: \(key)}) SET b.rating = \(rating) RETURN b.key AS key"
        #expect(statement.cypher == "MERGE (b:Book {key: $_p0}) SET b.rating = $_p1 RETURN b.key AS key")
        #expect(statement.parameters == ["_p0": .string(key), "_p1": .int(4)])
        #expect(try conn.run(statement).scalar() == .string(key))
        // The same literal renders the same text whatever the values, so the
        // engine sees one statement shape.
        let other: Statement = try "MERGE (b:Book {key: \("x")}) SET b.rating = \(1) RETURN b.key AS key"
        #expect(other.cypher == statement.cypher)
    }

    @Test func hostileTextArrivesAsAValue() throws {
        try conn.query("CREATE (:Book {key: 'keep'})")
        let hostile = "x'}) DETACH DELETE b //"
        let matched = try conn.run("MATCH (b:Book {key: \(hostile)}) DETACH DELETE b RETURN count(b)")
        #expect(try matched.scalar() == 0)
        #expect(try conn.query("MATCH (b:Book) RETURN count(b)").scalar() == 1)
        // And it round-trips as data.
        #expect(try conn.run("RETURN \(hostile)").scalar() == .string(hostile))
    }

    @Test func everyConvertibleKindInterpolates() throws {
        let missing: String? = nil
        let when = try Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse("2026-11-08T13:00:00.123Z")
        let row = try conn.run(
            """
            RETURN \(missing) AS nothing, \(["a", "b"]) AS list, \(["k": 1]) AS map, \(when) AS at,
                   \(CypherValue(vector: [Float(1), 2])) AS v, \(CypherValue.Point(latitude: 1, longitude: 2)) AS p,
                   \(2.5) AS f, \(true) AS flag
            """
        ).single()
        #expect(row["nothing"] == .null && row["list"] == ["a", "b"] && row["map"] == ["k": 1])
        #expect(try row.get("at", as: Date.self) == when)
        #expect(row["v"]?.kind == .vector && row["p"]?.kind == .point && row["f"] == 2.5 && row["flag"] == true)
    }

    @Test func aValueCypherCannotHoldThrowsAtTheLiteral() throws {
        _ = try invalidParameterMessage { let _: Statement = try "RETURN \(UInt64.max)" }
    }

    @Test func identifiersAreQuotedAndRawTextIsSpliced() throws {
        let label = "Odd`Label"
        let direction = "DESC"
        try conn.run("CREATE (:\(identifier: label) {n: \(1)}), (:\(identifier: label) {n: \(2)})")
        let statement: Statement = "MATCH (x:\(identifier: label)) RETURN x.n ORDER BY x.n \(raw: direction)"
        #expect(statement.cypher == "MATCH (x:`Odd``Label`) RETURN x.n ORDER BY x.n DESC")
        #expect(statement.parameters.isEmpty)
        #expect(try conn.run(statement).rows == [[2], [1]])
        #expect(Statement.quoted(identifier: "plain") == "`plain`")
        // A name is data between the quotes: an escape sequence or a line
        // break in it does not end the identifier.
        for name in ["a\\u0060) DETACH DELETE (x) //", "back\\slash", "new\nline"] {
            try conn.run("CREATE (:\(identifier: name) {probe: \(name)})")
            let labels = try conn.run("MATCH (n {probe: \(name)}) RETURN labels(n)").scalar()
            #expect(labels == .list([.string(name)]))
        }
        #expect(try conn.query("MATCH (n) RETURN count(n)").scalar() == 5)
    }

    @Test func aStatementEmbedsAnotherWithItsParameters() throws {
        try conn.query("UNWIND range(1, 5) AS i CREATE (:N {i: i, tag: CASE WHEN i % 2 = 0 THEN 'even' ELSE 'odd' END})")
        let minimum = 2
        let filter: Statement = try "n.i >= \(minimum) AND n.tag = \("even")"
        let limit = 10
        let statement: Statement = try "MATCH (n:N) WHERE \(filter) RETURN n.i ORDER BY n.i LIMIT \(limit)"
        // Names are assigned on the whole statement, in order of appearance.
        #expect(statement.cypher == "MATCH (n:N) WHERE n.i >= $_p0 AND n.tag = $_p1 RETURN n.i ORDER BY n.i LIMIT $_p2")
        #expect(statement.parameters == ["_p0": 2, "_p1": "even", "_p2": 10])
        #expect(try conn.run(statement).rows == [[2], [4]])
    }

    @Test func namedParametersAndGeneratedOnesCoexist() throws {
        // A statement that names its own parameters, including one spelled
        // like a generated name: generation skips it.
        let named = Statement("n.i = $_p0 OR n.i = $wanted", ["_p0": 1, "wanted": 3])
        let statement: Statement = try "UNWIND range(1, 5) AS i WITH {i: i} AS n WHERE \(named) OR n.i = \(5) RETURN n.i"
        #expect(statement.parameters == ["_p0": 1, "wanted": 3, "_p1": 5])
        #expect(statement.cypher.hasSuffix("OR n.i = $_p1 RETURN n.i"))
        #expect(try conn.run(statement).rows == [[1], [3], [5]])
        // Two fragments binding one name to different values are refused.
        let a = Statement("$x", ["x": 1])
        let b = Statement("$x", ["x": 2])
        #expect(throws: RelateError.self) { let _: Statement = try "RETURN \(a) + \(b)" }
        let same: Statement = try "RETURN \(a) + \(a)"
        #expect(try conn.run(same).scalar() == 2)
        // A refused append changes nothing: none of the other statement's
        // acceptable names is merged either.
        var target = Statement("RETURN $x", ["x": 1])
        let before = target
        let other = Statement(" + $x + $a + $b + $c", ["a": 1, "b": 2, "c": 3, "x": 2])
        #expect(throws: RelateError.self) { try target.append(other) }
        #expect(target == before)
    }

    @Test func aStatementBuiltFromSwiftValuesAndPlainLiterals() throws {
        let plain: Statement = "RETURN 1"
        #expect(plain.cypher == "RETURN 1" && plain.parameters.isEmpty && plain.description == "RETURN 1")
        let title = "Dune"
        let statement = try Statement("RETURN $title + $suffix", ["title": title, "suffix": "!"])
        #expect(try conn.run(statement).scalar() == "Dune!")
        #expect(statement == Statement("RETURN $title + $suffix", ["title": "Dune", "suffix": "!"]))
        #expect(Set([plain, plain, statement]).count == 2)
    }
}
