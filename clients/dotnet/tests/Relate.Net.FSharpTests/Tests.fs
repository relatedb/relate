module Relate.Net.FSharpTests.Tests

open System
open System.Collections.Generic
open Xunit
open Relate.Net

/// Proves the API is natural to consume from F#: optional arguments, `use` disposal,
/// pattern matching on result values, F# collections as parameters and task workflows.

[<Fact>]
let ``query with an F# map as parameters`` () =
    use db = RelateDatabase.OpenInMemory()
    use conn = db.Connect()

    let parameters = Map.ofList [ "a", 40; "b", 2 ]
    let result = conn.Query("RETURN $a + $b AS answer", parameters)

    Assert.Equal(42L, result.Scalar<int64>())

[<Fact>]
let ``query with dict and anonymous record parameters`` () =
    use db = RelateDatabase.OpenInMemory()
    use conn = db.Connect()

    let fromDict = conn.Query("RETURN $name AS name", dict [ "name", box "Ada" ])
    let fromRecord = conn.Query("RETURN $name AS name, $tags AS tags", {| name = "Grace"; tags = [ "navy"; "cobol" ] |})

    Assert.Equal("Ada", fromDict.[0].Get<string>("name"))
    Assert.Equal("Grace", fromRecord.[0].Get<string>("name"))
    Assert.Equal<string list>([ "navy"; "cobol" ], fromRecord.[0].Get<string[]>("tags") |> List.ofArray)

[<Fact>]
let ``pattern match on returned graph values`` () =
    use db = RelateDatabase.OpenInMemory()
    use conn = db.Connect()

    let record = conn.Query("CREATE (a:Person {name: 'Ada'})-[r:KNOWS]->(b:Person {name: 'Bob'}) RETURN a, r, b, 1 AS n").[0]

    let describe (value: objnull) =
        match value with
        | :? Node as node -> sprintf "node %s" (node.Get<string>("name"))
        | :? Relationship as rel -> sprintf "rel %s" rel.Type
        | :? int64 as i -> sprintf "int %d" i
        | _ -> "other"

    let described = record.Values |> Seq.map describe |> List.ofSeq
    Assert.Equal<string list>([ "node Ada"; "rel KNOWS"; "node Bob"; "int 1" ], described)

[<Fact>]
let ``transactions with use roll back automatically`` () =
    use db = RelateDatabase.OpenInMemory()
    use conn = db.Connect()

    do
        use tx = conn.BeginTransaction()
        tx.Query("CREATE (:Gone)") |> ignore
    // tx disposed here without commit

    do
        use tx = conn.BeginTransaction()
        tx.Query("CREATE (:Kept)") |> ignore
        let bookmark = tx.Commit()
        Assert.NotNull(bookmark)

    let count label = conn.Query(sprintf "MATCH (n:%s) RETURN count(n)" label).Scalar<int64>()
    Assert.Equal(0L, count "Gone")
    Assert.Equal(1L, count "Kept")

[<Fact>]
let ``errors surface as RelateException`` () =
    use db = RelateDatabase.OpenInMemory()
    use conn = db.Connect()

    let ex = Assert.Throws<RelateException>(fun () -> conn.Query("RETURN 1 / 0") |> ignore)
    Assert.Equal(ErrorStage.Runtime, ex.Stage)

[<Fact>]
let ``async queries compose with task`` () =
    task {
        use db = RelateDatabase.OpenInMemory()
        use conn = db.Connect()
        let! result = conn.QueryAsync("UNWIND range(1, 5) AS x RETURN sum(x) AS total")
        Assert.Equal(15, result.Scalar<int>())
    }

[<Fact>]
let ``temporal and spatial values`` () =
    use db = RelateDatabase.OpenInMemory()
    use conn = db.Connect()

    let p = CypherPoint.Wgs84(12.57, 55.68)
    let r = conn.Query("RETURN $p AS p, $d AS d, $dur AS dur", {| p = p; d = DateOnly(2024, 1, 31); dur = TimeSpan.FromMinutes(5.0) |}).[0]

    Assert.Equal(p, r.Get<CypherPoint>("p"))
    Assert.Equal(DateOnly(2024, 1, 31), r.Get<DateOnly>("d"))
    Assert.Equal(TimeSpan.FromMinutes(5.0), r.Get<TimeSpan>("dur"))
