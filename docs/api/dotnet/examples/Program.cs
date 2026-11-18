using Relate.Net;

using var db = RelateDatabase.OpenInMemory();
using var conn = db.Connect();
using (var tx = conn.BeginTransaction())
{
    tx.Query("CREATE (:Person {name: $name, born: $born})",
        new { name = "Ada", born = new DateOnly(1815, 12, 10) });
    if (tx.Commit() is null) throw new Exception("A write commit must have a bookmark.");
}
var result = await conn.QueryAsync("MATCH (p:Person) RETURN p");
var person = result[0].Get<Node>("p");
if (person is null || person.Get<string>("name") != "Ada" || !person.HasLabel("Person"))
    throw new Exception("Graph values did not round-trip.");
if (result.QueryType != QueryType.Read || result.Namespace != "neo4j")
    throw new Exception("Unexpected statement metadata.");
using (var rollback = conn.BeginTransaction())
    rollback.Query("CREATE (:Discarded)");
if (conn.Query("MATCH (n:Discarded) RETURN count(n)").Scalar<long>() != 0)
    throw new Exception("Disposal must roll back.");
conn.ChangeNamespace("system");
if (conn.Query("SHOW DATABASES").Namespace != "system")
    throw new Exception("Namespace did not change.");
conn.ChangeNamespace("neo4j");
try
{
    conn.Query("RETRN 1");
    throw new Exception("Expected a syntax error.");
}
catch (RelateException error) when (error.Stage == ErrorStage.Compile && !error.IsRetryable)
{
    Console.WriteLine(error.Code);
}
conn.Dispose();
if (person.Get<DateOnly>("born") != new DateOnly(1815, 12, 10))
    throw new Exception("Result did not survive connection disposal.");
Console.WriteLine("Ada — .NET client example passed");
