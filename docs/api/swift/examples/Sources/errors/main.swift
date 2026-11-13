import Relate

let conn = try Database.inMemory().connect()
try conn.query("CREATE CONSTRAINT person_email FOR (p:Person) REQUIRE p.email IS UNIQUE")
try conn.query("CREATE (:Person {email: $email})", ["email": "ada@example.com"])

// Calls are `throws(RelateError)`: the catch block's `error` is typed.
do {
    try conn.query("CREATE (:Person {email: $email})", ["email": "ada@example.com"])
    preconditionFailure("the duplicate must be refused")
} catch {
    let engine = error.engineError!
    print("constraint:", engine.code)
    precondition(engine.isConstraintViolation && engine.classification == .clientError)
    precondition(!error.isRetryable)
}

do {
    try conn.query("RETURN $absent")
    preconditionFailure("an unbound parameter must be refused")
} catch {
    precondition(error.engineError!.isMissingParameter)
}

do {
    try conn.query("MATCH (")
    preconditionFailure("a syntax error must be refused")
} catch {
    precondition(error.engineError!.isSyntaxError && error.engineError!.stage == .compile)
}

// A write-write conflict is the retryable case: two transactions create the
// same unique key and the second commit loses.
let other = try conn.database.connect()
try conn.begin()
try other.begin()
try conn.query("CREATE (:Person {email: 'grace@example.com'})")
try other.query("CREATE (:Person {email: 'grace@example.com'})")
try conn.commit()
do {
    try other.commit()
    preconditionFailure("the second commit must lose")
} catch {
    try? other.rollback()
    print("conflict:", error.engineError!.code, "retryable:", error.isRetryable)
    precondition(error.engineError!.isWriteConflict && error.isRetryable)
    precondition(error.engineError!.classification == .transientError)
}
