import Foundation
import Testing

@testable import Relate

@Suite struct ErrorChannelTests {
    @Test func aStatementFailureRidesTheResult() throws {
        let conn = try Database.inMemory().connect()
        let error = try #require(throws: RelateError.self) { try conn.query("RETURN 1/0") }
        let engine = try #require(error.engineError)
        #expect(engine.code == "Neo.ClientError.Statement.ArithmeticError")
        #expect(engine.gqlStatus == "22G03")
        #expect(engine.stage == .runtime)
        #expect(!engine.retryable)
        #expect(!engine.message.isEmpty)
    }

    @Test func aSyntaxErrorIsACompileStageFailure() throws {
        let conn = try Database.inMemory().connect()
        let error = try #require(throws: RelateError.self) { try conn.query("RETURN nope(") }
        let engine = try #require(error.engineError)
        #expect(engine.stage == .compile && !engine.retryable)
        #expect(engine.code.hasPrefix("Neo.ClientError."))
    }

    @Test func callFailuresTravelWithTheCall() throws {
        try withTemporaryStore { path in
            let first = try Database(path: path)
            try first.connect().query("CREATE (:P)")
            // A second writable open fails through the open call's own
            // out-param — the same typed shape, not a thread-local slot.
            let second = try #require(throws: RelateError.self) { try Database(path: path) }
            #expect(second.engineError != nil)
            let reader = try Database(readOnlyPath: path)
            let write = try #require(throws: RelateError.self) {
                try reader.connect().query("CREATE (:P)")
            }
            #expect(write.engineError?.retryable == false)
        }
    }

    // Each predicate is provoked from the live engine, so a code the engine
    // renames fails here rather than silently never matching.

    @Test func aConstraintViolationIsRecognized() throws {
        let conn = try Database.inMemory().connect()
        try conn.query("CREATE CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE")
        try conn.query("CREATE (:P {id: 1})")
        let error = try #require(throws: RelateError.self) { try conn.query("CREATE (:P {id: 1})") }
        let engine = try #require(error.engineError)
        #expect(engine.isConstraintViolation)
        #expect(engine.classification == .clientError)
        #expect(!engine.isSyntaxError && !engine.isMissingParameter && !engine.isWriteConflict)
    }

    @Test func aSyntaxErrorIsRecognized() throws {
        let conn = try Database.inMemory().connect()
        let error = try #require(throws: RelateError.self) { try conn.query("RETURN nope(") }
        let engine = try #require(error.engineError)
        #expect(engine.isSyntaxError && engine.classification == .clientError)
    }

    @Test func aMissingParameterIsRecognized() throws {
        let conn = try Database.inMemory().connect()
        let error = try #require(throws: RelateError.self) { try conn.query("RETURN $absent") }
        let engine = try #require(error.engineError)
        #expect(engine.isMissingParameter && engine.classification == .clientError)
    }

    @Test func aWriteConflictIsRecognizedAndRetryable() throws {
        let error = try losingCommitError()
        let engine = try #require(error.engineError)
        #expect(engine.isWriteConflict && engine.retryable && error.isRetryable)
        #expect(engine.classification == .transientError)
    }

    @Test func aCodeOutsideTheNeoShapeHasNoClassification() throws {
        func engine(code: String) -> EngineError {
            EngineError(code: code, gqlStatus: "50N42", stage: .runtime, retryable: false, message: "m")
        }
        #expect(engine(code: "Neo.DatabaseError.General.UnknownError").classification == .databaseError)
        #expect(engine(code: "Neo.FutureClass.General.Something").classification == nil)
        #expect(engine(code: "not.a.neo.code").classification == nil)
        #expect(engine(code: "Neo.ClientError.TooFew").classification == nil)
        // The client-side cases carry no engine code at all, and are never retryable.
        #expect(RelateError.invalidParameter("x").engineError == nil)
        #expect(!RelateError.decoding("x").isRetryable)
    }

    // MARK: Poison

    @Test func aHealthyConnectionIsNotPoisoned() throws {
        let conn = try Database.inMemory().connect()
        #expect(!conn.isPoisoned())
        // A boundary-stage refusal that is not poison: the params builder
        // rejects an impossible date. The connection stays usable.
        let error = try #require(throws: RelateError.self) {
            try conn.query("RETURN $d", ["d": .date(.init(year: 2026, month: 2, day: 31))])
        }
        #expect(error.engineError?.stage == .internal)
        #expect(!conn.isPoisoned())
        #expect(try conn.query("RETURN 1").scalar() == 1)
        // Inside an open transaction the probe is harmless.
        try conn.transaction { tx in
            try tx.query("CREATE (:Probe)")
            #expect(!conn.isPoisoned())
        }
        #expect(try conn.query("MATCH (n:Probe) RETURN count(n)").scalar() == 1)
    }

    @Test func theProbeReadsPoisonOffTheBoundaryStage() throws {
        func engine(_ stage: EngineError.Stage) -> RelateError {
            .engine(EngineError(
                code: "Neo.DatabaseError.General.UnknownError", gqlStatus: "50N42", stage: stage,
                retryable: false, message: "handle poisoned by a previous panic; free it and open a new one"))
        }
        #expect(Connection.probeIndicatesPoison(engine(.internal)))
        #expect(!Connection.probeIndicatesPoison(engine(.runtime)))
        #expect(!Connection.probeIndicatesPoison(engine(.compile)))
        #expect(!Connection.probeIndicatesPoison(.decoding("x")))
    }
}
