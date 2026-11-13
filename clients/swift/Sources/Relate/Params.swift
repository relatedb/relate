import CRelate

// MARK: - The params builder (ADR-0502 §4)

/// The envelope every put arm shares: run the putter with the child's name
/// (interior-NUL refusal built into the crossing) and the shared out-param,
/// then the one status→throw policy over that out-param.
private func go(
    _ name: String?,
    _ call: (UnsafePointer<CChar>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?)
        throws(RelateError) -> relate_status
) throws(RelateError) {
    var err: UnsafeMutablePointer<CChar>? = nil
    let status = try withOptionalCString(name, what: "parameter name") {
        (pointer) throws(RelateError) in try call(pointer, &err)
    }
    try check(status, err, "binding a parameter")
}

/// The one deterministic bind order — sorted by key; the engine is
/// order-free — for the root map and every nested map.
private func putEntries(_ entries: [String: CypherValue], into builder: OpaquePointer) throws(RelateError) {
    for (name, value) in entries.sorted(by: { $0.key < $1.key }) {
        try put(value, into: builder, name: name)
    }
}

/// Opens a container child builder — the container arms' shared opening:
/// the putter through the `go` envelope, then the OK-without-handle
/// refusal.
private func openChild(
    _ name: String?, into builder: OpaquePointer,
    _ putter: (
        OpaquePointer, UnsafePointer<CChar>?, UnsafeMutablePointer<OpaquePointer?>?,
        UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
    ) -> relate_status,
    _ what: String
) throws(RelateError) -> OpaquePointer {
    var out: OpaquePointer?
    try go(name) { namePointer, err in putter(builder, namePointer, &out, err) }
    return try requireHandle(out, what)
}

/// One binding for both coordinate families: the putter and the element
/// type are the only differences.
private func putVector<T>(
    _ putter: (
        OpaquePointer, UnsafePointer<CChar>?, UnsafePointer<CChar>?, UnsafePointer<T>?, Int,
        UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
    ) -> relate_status,
    _ dtype: CypherValue.Vector.DType, _ values: [T],
    into builder: OpaquePointer, name: String?
) throws(RelateError) {
    try values.withUnsafeBufferPointer { buffer in
        dtype.rawValue.withCString { dtypePointer in
            typedResult { () throws(RelateError) in
                try go(name) { namePointer, err in
                    putter(builder, namePointer, dtypePointer, buffer.baseAddress, buffer.count, err)
                }
            }
        }
    }.get()
}

/// Builds the statement's root MAP of named parameters. Every entry crosses
/// through a typed putter — no JSON. The caller frees the root (which owns
/// every child minted below it).
func bindParameters(_ parameters: [String: CypherValue]) throws(RelateError) -> OpaquePointer {
    // The contract: a root never comes back NULL.
    let root = try requireHandle(relate_params_new(), "relate_params_new")
    do {
        try putEntries(parameters, into: root)
    } catch {
        relate_params_free(root)
        throw error
    }
    return root
}

/// Puts one value under `name` (a LIST child passes `nil`). Every arm
/// crosses through `go` — the one envelope holding the name crossing, the
/// shared out-param and the single refusal policy (`check`) — so a putter
/// refusal surfaces as a typed `.engine` error, never a silent skip.
private func put(_ value: CypherValue, into builder: OpaquePointer, name: String?) throws(RelateError) {
    switch value {
    case .null:
        try go(name) { relate_params_put_null(builder, $0, $1) }
    case .bool(let v):
        try go(name) { relate_params_put_bool(builder, $0, v ? 1 : 0, $1) }
    case .int(let v):
        try go(name) { relate_params_put_int(builder, $0, v, $1) }
    case .float(let v):
        try go(name) { relate_params_put_float(builder, $0, v, $1) }
    case .string(let v):
        try go(name) { namePointer, err in
            withUTF8(v) { base, length in
                relate_params_put_string(builder, namePointer, base, length, err)
            }
        }
    case .date(let d):
        try go(name) { relate_params_put_date(builder, $0, d.year, d.month, d.day, $1) }
    case .localTime(let t):
        try go(name) {
            relate_params_put_local_time(builder, $0, t.hour, t.minute, t.second, t.nanosecond, $1)
        }
    case .time(let t):
        // A TIME keeps no zone id; the offset is the whole story.
        try go(name) {
            relate_params_put_time(
                builder, $0, t.hour, t.minute, t.second, t.nanosecond, t.offsetSeconds, nil, $1)
        }
    case .localDateTime(let d):
        try go(name) {
            relate_params_put_local_datetime(
                builder, $0, d.year, d.month, d.day, d.hour, d.minute, d.second, d.nanosecond, $1)
        }
    case .dateTime(let d):
        // A named zone is kept by the value and re-read by the walker; with
        // none, the offset alone carries the value.
        try go(name) { (namePointer, err) throws(RelateError) in
            try withOptionalCString(d.zone, what: "the datetime's zone") { zonePointer in
                relate_params_put_datetime(
                    builder, namePointer, d.year, d.month, d.day, d.hour, d.minute, d.second,
                    d.nanosecond, d.offsetSeconds, zonePointer, err)
            }
        }
    case .duration(let d):
        try go(name) {
            relate_params_put_duration(builder, $0, d.months, d.days, d.seconds, d.nanoseconds, $1)
        }
    case .point(let p):
        try go(name) { namePointer, err in
            withOptionalPointer(to: p.z) { zPointer in
                relate_params_put_point(builder, namePointer, p.srid, p.x, p.y, zPointer, err)
            }
        }
    case .vector(let v):
        switch v {
        case .floats(let dtype, let values):
            try putVector(relate_params_put_vector_float, dtype, values, into: builder, name: name)
        case .ints(let dtype, let values):
            try putVector(relate_params_put_vector_int, dtype, values, into: builder, name: name)
        }
    case .list(let items):
        let list = try openChild(name, into: builder, relate_params_put_list, "the list putter")
        for item in items { try put(item, into: list, name: nil) }
    case .map(let entries):
        let map = try openChild(name, into: builder, relate_params_put_map, "the map putter")
        try putEntries(entries, into: map)
    case .node, .relationship, .path:
        throw RelateError.invalidParameter(
            "nodes, relationships and paths cannot be passed as parameters; pass their element id or properties")
    }
}
