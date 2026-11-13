import Relate
import Foundation

// A directory path is a durable store: committed writes go to the WAL,
// checkpoint() folds them into Parquet snapshots.
let dir = FileManager.default.temporaryDirectory
    .appendingPathComponent("relate-example-\(UUID().uuidString)")
defer { try? FileManager.default.removeItem(at: dir) }

do {
    let db = try Database(url: dir)
    let conn = try db.connect()
    try conn.query("UNWIND range(1, 100) AS i CREATE (:Reading {seq: i})")
    try db.checkpoint()
    try conn.query("CREATE (:Reading {seq: 101})")  // WAL tail after the checkpoint
}  // the last reference is released here, closing the store

// Reopen read-only: the checkpoint plus the replayed WAL tail.
let reader = try Database(url: dir, readOnly: true)
let count = try reader.connect().query("MATCH (r:Reading) RETURN count(r)").scalar()
print("readings after reopen:", count)
precondition(count == 101)

do {
    try reader.connect().query("CREATE (:Reading)")
    preconditionFailure("a read-only store must refuse writes")
} catch {
    // The call is `throws(RelateError)`: `error` is typed, no cast.
    print("refused:", error.message)
}
