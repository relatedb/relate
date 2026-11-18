Imports System.Collections.Generic
Imports Relate.Net
Imports Xunit

''' <summary>
''' Proves the API is natural to consume from Visual Basic (Option Strict On): Using blocks,
''' optional parameters, anonymous types, dictionaries, Async/Await and TypeOf checks.
''' </summary>
Public Class RelateTests

    <Fact>
    Public Sub QueryWithAnonymousTypeParameters()
        Using db = RelateDatabase.OpenInMemory(), conn = db.Connect()
            Dim result = conn.Query("RETURN $a * $b AS product", New With {.a = 6, .b = 7})

            Assert.Equal(42L, result.Scalar(Of Long)())
            Assert.Equal("product", result.Columns(0))
        End Using
    End Sub

    <Fact>
    Public Sub QueryWithDictionaryParameters()
        Using db = RelateDatabase.OpenInMemory(), conn = db.Connect()
            Dim parameters As New Dictionary(Of String, Object) From {{"name", "Ada"}, {"born", New DateOnly(1815, 12, 10)}}

            conn.Query("CREATE (:Person {name: $name, born: $born})", parameters)
            Dim person = conn.Query("MATCH (p:Person) RETURN p")(0).Get(Of Node)("p")

            Assert.Equal("Ada", person.Get(Of String)("name"))
            Assert.Equal(New DateOnly(1815, 12, 10), person.Get(Of DateOnly)("born"))
            Assert.Contains("Person", person.Labels)
        End Using
    End Sub

    <Fact>
    Public Sub RecordsCanBeIteratedAndIndexed()
        Using db = RelateDatabase.OpenInMemory(), conn = db.Connect()
            Dim total As Long = 0
            For Each record In conn.Query("UNWIND [1, 2, 3] AS x RETURN x")
                total += CLng(record("x"))
            Next

            Assert.Equal(6L, total)
        End Using
    End Sub

    <Fact>
    Public Sub TransactionCommitAndRollback()
        Using db = RelateDatabase.OpenInMemory(), conn = db.Connect()
            Using tx = conn.BeginTransaction()
                tx.Query("CREATE (:Kept)")
                Dim bookmark As Bookmark = tx.Commit()
                Assert.NotNull(bookmark)
            End Using

            Using tx = conn.BeginTransaction()
                tx.Query("CREATE (:Gone)")
                tx.Rollback()
            End Using

            Assert.Equal(1L, conn.Query("MATCH (n:Kept) RETURN count(n)").Scalar(Of Long)())
            Assert.Equal(0L, conn.Query("MATCH (n:Gone) RETURN count(n)").Scalar(Of Long)())
        End Using
    End Sub

    <Fact>
    Public Sub ErrorsAreRelateExceptions()
        Using db = RelateDatabase.OpenInMemory(), conn = db.Connect()
            Dim ex = Assert.Throws(Of RelateException)(Function() conn.Query("RETRN 1"))

            Assert.Equal(ErrorStage.Compile, ex.Stage)
            Assert.Equal("Neo.ClientError.Statement.SyntaxError", ex.Code)
        End Using
    End Sub

    <Fact>
    Public Async Function AsyncQueryWorks() As Task
        Using db = RelateDatabase.OpenInMemory(), conn = db.Connect()
            Dim result = Await conn.QueryAsync("RETURN 'hello from VB' AS greeting")

            Assert.Equal("hello from VB", result(0).Get(Of String)("greeting"))
        End Using
    End Function

    <Fact>
    Public Sub GraphValuesCanBeTypeChecked()
        Using db = RelateDatabase.OpenInMemory(), conn = db.Connect()
            Dim record = conn.Query("CREATE p = (:A)-[:R]->(:B) RETURN p")(0)

            Assert.True(TypeOf record("p") Is GraphPath)
            Dim path = DirectCast(record("p"), GraphPath)
            Assert.Equal(1, path.Length)
            Assert.Equal("R", path.Relationships(0).Type)
        End Using
    End Sub

End Class
