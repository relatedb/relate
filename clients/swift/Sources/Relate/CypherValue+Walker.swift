import CRelate

// MARK: - The walker decoder (ADR-0502 §1)

extension CypherValue {
    /// Decodes one value handle through the typed walker. Every handle is
    /// result-owned; nothing is freed here.
    static func decode(_ handle: relate_value?) throws(RelateError) -> CypherValue {
        guard let handle else {
            throw RelateError.decoding("the C ABI returned a NULL value handle")
        }
        switch try CypherValueKind(abi: relate_value_kind(handle)) {
        case .null: return .null
        case .bool:
            return .bool(try scalar(handle, relate_value_bool, initial: 0, what: "bool") != 0)
        case .int:
            return .int(try scalar(handle, relate_value_int, initial: 0, what: "int"))
        case .float:
            return .float(try scalar(handle, relate_value_float, initial: 0, what: "float"))
        case .string: return .string(try string(handle))
        case .node:
            return .node(Node(
                id: try int64(handle, relate_value_id, what: "entity id"),
                elementId: try required(relate_value_element_id(handle), "an element id"),
                labels: try labels(handle),
                properties: try mapEntries(handle)))
        case .relationship:
            return .relationship(Relationship(
                id: try int64(handle, relate_value_id, what: "entity id"),
                elementId: try required(relate_value_element_id(handle), "an element id"),
                type: try string(handle),
                startId: try int64(handle, relate_value_start_id, what: "start entity id"),
                endId: try int64(handle, relate_value_end_id, what: "end entity id"),
                startElementId: try required(relate_value_start_element_id(handle), "an element id"),
                endElementId: try required(relate_value_end_element_id(handle), "an element id"),
                properties: try mapEntries(handle)))
        case .path:
            let items = try listItems(handle)
            guard items.count % 2 == 1 else {
                throw RelateError.decoding("a path decoded with an even element count \(items.count)")
            }
            var nodes: [Node] = []
            var relationships: [Relationship] = []
            for (index, item) in items.enumerated() {
                if index % 2 == 0 {
                    guard case .node(let node) = item else {
                        throw RelateError.decoding("path position \(index) is not a node")
                    }
                    nodes.append(node)
                } else {
                    guard case .relationship(let relationship) = item else {
                        throw RelateError.decoding("path position \(index) is not a relationship")
                    }
                    relationships.append(relationship)
                }
            }
            return .path(Path(nodes: nodes, relationships: relationships))
        case .list: return .list(try listItems(handle))
        case .map: return .map(try mapEntries(handle))
        case .point:
            var srid: Int32 = 0
            var dim: Int32 = 0
            var x: Double = 0
            var y: Double = 0
            var z: Double = 0
            try refuse(relate_value_point(handle, &srid, &dim, &x, &y, &z), "point")
            // The ABI's points are 2D or 3D; anything else is a contract
            // violation, refused like every other out-of-contract shape
            // rather than silently flattened to 2D.
            guard dim == 2 || dim == 3 else {
                throw RelateError.decoding("unsupported point dimension \(dim)")
            }
            return .point(Point(srid: srid, x: x, y: y, z: dim == 3 ? z : nil))
        case .date:
            let date = try calendarDate(handle)
            return .date(Date(year: date.year, month: date.month, day: date.day))
        case .time:
            let clock = try wallClock(handle)
            return .time(Time(
                hour: clock.hour, minute: clock.minute, second: clock.second,
                nanosecond: clock.nanosecond,
                offsetSeconds: try int64(handle, relate_value_offset_seconds, what: "offset")))
        case .localTime:
            let clock = try wallClock(handle)
            return .localTime(LocalTime(
                hour: clock.hour, minute: clock.minute, second: clock.second,
                nanosecond: clock.nanosecond))
        case .dateTime:
            let date = try calendarDate(handle)
            let clock = try wallClock(handle)
            return .dateTime(DateTime(
                year: date.year, month: date.month, day: date.day,
                hour: clock.hour, minute: clock.minute, second: clock.second,
                nanosecond: clock.nanosecond,
                offsetSeconds: try int64(handle, relate_value_offset_seconds, what: "offset"),
                zone: try zone(handle)))
        case .localDateTime:
            let date = try calendarDate(handle)
            let clock = try wallClock(handle)
            return .localDateTime(LocalDateTime(
                year: date.year, month: date.month, day: date.day,
                hour: clock.hour, minute: clock.minute, second: clock.second,
                nanosecond: clock.nanosecond))
        case .duration:
            var months: Int64 = 0
            var days: Int64 = 0
            var seconds: Int64 = 0
            var nanoseconds: Int64 = 0
            try refuse(relate_value_duration(handle, &months, &days, &seconds, &nanoseconds), "duration")
            return .duration(Duration(
                months: months, days: days, seconds: seconds, nanoseconds: nanoseconds))
        case .vector:
            return .vector(try vector(handle))
        }
    }

    /// A scalar read after the kind switch proved it in-kind: a refusal can
    /// only be an ABI contract violation. `initial` seeds the out-cell,
    /// which an in-kind OK always overwrites.
    private static func scalar<T>(
        _ handle: relate_value,
        _ get: (relate_value, UnsafeMutablePointer<T>?) -> relate_status,
        initial: T, what: String
    ) throws(RelateError) -> T {
        var out = initial
        try refuse(get(handle, &out), what)
        return out
    }

    /// An in-kind `Int64` scalar read (a temporal component, or one of
    /// ADR-0506's numeric entity ids): a refusal can only be an ABI
    /// contract violation.
    private static func int64(
        _ handle: relate_value,
        _ get: (relate_value, UnsafeMutablePointer<Int64>?) -> relate_status,
        what: String
    ) throws(RelateError) -> Int64 {
        try scalar(handle, get, initial: 0, what: what)
    }

    /// The scalar-read policy at an index (a vector coordinate), for the
    /// accessors that take one: `initial` seeds the out-cell, which an
    /// in-kind OK always overwrites.
    private static func scalarAt<T>(
        _ handle: relate_value, _ index: Int64,
        _ get: (relate_value, Int64, UnsafeMutablePointer<T>?) -> relate_status,
        initial: T, what: String
    ) throws(RelateError) -> T {
        var out = initial
        try refuse(get(handle, index, &out), what)
        return out
    }

    /// The wall-clock component bundle the ABI exposes on TIME, LOCALTIME
    /// and the datetimes.
    private static func wallClock(_ handle: relate_value) throws(RelateError) ->
        (hour: Int64, minute: Int64, second: Int64, nanosecond: Int64)
    {
        (
            hour: try int64(handle, relate_value_hour, what: "hour"),
            minute: try int64(handle, relate_value_minute, what: "minute"),
            second: try int64(handle, relate_value_second, what: "second"),
            nanosecond: try int64(handle, relate_value_nanosecond, what: "nanosecond")
        )
    }

    /// The calendar-date component bundle the ABI exposes on DATE and the
    /// datetimes.
    private static func calendarDate(_ handle: relate_value) throws(RelateError) ->
        (year: Int64, month: Int64, day: Int64)
    {
        (
            year: try int64(handle, relate_value_year, what: "year"),
            month: try int64(handle, relate_value_month, what: "month"),
            day: try int64(handle, relate_value_day, what: "day")
        )
    }

    /// A refused status after the kind switch proved the read in-kind:
    /// only an ABI contract violation, never a value-shaped failure.
    private static func refuse(_ status: relate_status, _ what: String) throws(RelateError) {
        guard status == RELATE_OK else {
            throw RelateError.decoding("the walker refused an in-kind \(what) read")
        }
    }

    /// The ABI's container-length contract: a negative raw is the walker's
    /// refusal of an in-kind length read — one spelling for LIST, MAP and
    /// VECTOR.
    private static func length(_ raw: Int64, _ what: String) throws(RelateError) -> Int64 {
        guard raw >= 0 else {
            throw RelateError.decoding("the walker refused an in-kind \(what) length")
        }
        return raw
    }

    /// STRING (or a RELATIONSHIP's type) with its byte length, so an
    /// interior NUL crosses whole.
    private static func string(_ handle: relate_value) throws(RelateError) -> String {
        var length = 0
        guard let pointer = relate_value_string(handle, &length) else {
            throw RelateError.decoding("the walker returned NULL for a string value")
        }
        return try utf8(pointer, length, what: "a string value")
    }

    private static func zone(_ handle: relate_value) throws(RelateError) -> String? {
        let text = try required(relate_value_zone(handle), "a zone")
        return text.isEmpty ? nil : text
    }

    /// A NODE's labels: its list items, each of which the ABI promises is a
    /// string.
    private static func labels(_ handle: relate_value) throws(RelateError) -> [String] {
        var labels: [String] = []
        for item in try listItems(handle) {
            guard case .string(let label) = item else {
                throw RelateError.decoding("a node label is not a string")
            }
            labels.append(label)
        }
        return labels
    }

    /// LIST items, PATH elements or a NODE's labels.
    private static func listItems(_ handle: relate_value) throws(RelateError) -> [CypherValue] {
        let count = try length(relate_value_list_len(handle), "list")
        var items: [CypherValue] = []
        items.reserveCapacity(Int(count))
        for index in 0..<count {
            guard let child = relate_value_list_get(handle, index) else {
                throw RelateError.decoding("the walker returned NULL for list item \(index)")
            }
            items.append(try decode(child))
        }
        return items
    }

    /// MAP entries or an entity's properties, in the engine's order.
    private static func mapEntries(_ handle: relate_value) throws(RelateError) -> [String: CypherValue] {
        let count = try length(relate_value_map_len(handle), "map")
        var entries: [String: CypherValue] = [:]
        entries.reserveCapacity(Int(count))
        for index in 0..<count {
            var length = 0
            guard let keyPointer = relate_value_map_key(handle, index, &length) else {
                throw RelateError.decoding("the walker returned NULL for map key \(index)")
            }
            let key = try utf8(keyPointer, length, what: "map key \(index)")
            // map_get reads its key NUL-terminated, so a key holding an
            // interior NUL would resolve to its prefix — possibly a
            // different key's value. Refuse it, as the Go and Java clients
            // do, rather than return the wrong value. NUL is valid UTF-8,
            // so the decoded key's own bytes answer this.
            if key.utf8.contains(0) {
                throw RelateError.decoding(
                    "map key '\(key)' holds an interior NUL and cannot be read through the walker")
            }
            // An in-range key is present, so NULL is a contract
            // violation — never a silent Cypher null.
            guard let child = relate_value_map_get(handle, keyPointer) else {
                throw RelateError.decoding("map key '\(key)' at index \(index) has no value")
            }
            entries[key] = try decode(child)
        }
        return entries
    }

    private static func vector(_ handle: relate_value) throws(RelateError) -> Vector {
        let name = try required(relate_value_vector_dtype(handle), "a vector dtype")
        guard let dtype = Vector.DType(rawValue: name) else {
            throw RelateError.decoding("unknown vector dtype '\(name)'")
        }
        let dim = try length(relate_value_vector_dim(handle), "vector")
        /// One coordinate loop for both coordinate families: the accessor
        /// and the element type are the only differences.
        func coordinates<T>(_ read: (Int64) throws(RelateError) -> T) throws(RelateError) -> [T] {
            var values: [T] = []
            values.reserveCapacity(Int(dim))
            for index in 0..<dim { values.append(try read(index)) }
            return values
        }
        if dtype.isFloat {
            return .floats(dtype: dtype, values: try coordinates { (index) throws(RelateError) in
                try scalarAt(handle, index, relate_value_vector_float, initial: 0, what: "vector coordinate")
            })
        }
        return .ints(dtype: dtype, values: try coordinates { (index) throws(RelateError) in
            try scalarAt(handle, index, relate_value_vector_int, initial: 0, what: "vector coordinate")
        })
    }
}
