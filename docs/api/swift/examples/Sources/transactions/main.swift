import Relate

let conn = try Database.inMemory().connect()

// transaction(_:) commits when the body returns. The body gets a Transaction:
// a scope that can run statements and nothing else.
try conn.transaction { tx in
    try tx.query("CREATE (:Account {id: 1, balance: 100})")
    try tx.query("CREATE (:Account {id: 2, balance: 0})")
}

// ... and rolls back, then rethrows, when it throws.
struct InsufficientFunds: Error {}
do {
    try conn.transaction { tx in
        try tx.query("MATCH (a:Account {id: 1}) SET a.balance = a.balance - 500")
        let balance = try tx.query("MATCH (a:Account {id: 1}) RETURN a.balance").scalar()
        if (balance.intValue ?? 0) < 0 { throw InsufficientFunds() }
    }
    preconditionFailure("the transfer should have been refused")
} catch is InsufficientFunds {
    print("transfer refused; rolled back")
}

// Explicit begin/commit/rollback are available too.
try conn.begin()
try conn.query("CREATE (:Temp)")
try conn.rollback()

let balance = try conn.query("MATCH (a:Account {id: 1}) RETURN a.balance").scalar()
let temps = try conn.query("MATCH (t:Temp) RETURN count(t)").scalar()
print("balance:", balance)
precondition(balance == 100)
precondition(temps == 0)

// read(_:) is a transaction that always rolls back: its statements see one
// snapshot, and a statement that is not a read is refused.
let other = try conn.database.connect()
let (before, after) = try conn.read { tx in
    let before = try tx.query("MATCH (a:Account) RETURN count(a)").scalar(as: Int.self)
    try other.query("CREATE (:Account {id: 3, balance: 5})")  // commits meanwhile
    let after = try tx.query("MATCH (a:Account) RETURN count(a)").scalar(as: Int.self)
    return (before, after)
}
print("snapshot:", before, after)
precondition(before == 2 && after == 2)
do {
    _ = try conn.read { tx in try tx.query("CREATE (:Account {id: 4})") }
    preconditionFailure("a write in a read must be refused")
} catch {
    guard case RelateError.misuse(let message) = error else {
        preconditionFailure("expected a misuse, got \(error)")
    }
    print("refused:", message)
}
let accounts = try conn.query("MATCH (a:Account) RETURN count(a)").scalar(as: Int.self)
precondition(accounts == 3)

// retries: a failure the engine marks retryable (a write-write conflict)
// rolls back and runs the body again.
let deposit: Statement = "MATCH (a:Account {id: 2}) SET a.balance = a.balance + 10 RETURN a.balance"
var attempts = 0
let newBalance = try conn.transaction(retries: 2) { tx in
    attempts += 1
    if attempts == 1 { try other.run(deposit) }  // the other connection wins the first race
    return try tx.run(deposit).scalar(as: Int.self)
}
print("deposited after", attempts, "attempts:", newBalance)
precondition(attempts == 2 && newBalance == 20)

// A fixed list of statements, all or none.
try conn.runInTransaction([
    "MATCH (a:Account {id: 1}) SET a.balance = a.balance - 30",
    "MATCH (a:Account {id: 2}) SET a.balance = a.balance + 30",
])
let total = try conn.query("MATCH (a:Account) WHERE a.id IN [1, 2] RETURN sum(a.balance)").scalar(as: Int.self)
precondition(total == 120)
