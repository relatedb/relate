/// One Cypher value, decoded through the C ABI's typed value walker
/// (ADR-0502 §1: `relate_value_at` and the `relate_value_*`
/// accessors). There is no wire JSON: every case below corresponds to one
/// enumerator of the ABI's `enum relate_value_kind`, discriminated
/// structurally at every depth — a user map shaped like a node stays a
/// `.map`, a path nested in a list stays a `.path`.
public enum CypherValue: Sendable, Hashable {
    case null
    case bool(Bool)
    case int(Int64)
    /// NaN and ±∞ arrive as themselves (the v1 string spellings are gone).
    case float(Double)
    /// A string may contain interior NUL bytes; they cross whole.
    case string(String)
    case list([CypherValue])
    case map([String: CypherValue])
    case node(Node)
    case relationship(Relationship)
    case path(Path)
    case point(Point)
    case date(Date)
    case time(Time)
    case localTime(LocalTime)
    case dateTime(DateTime)
    case localDateTime(LocalDateTime)
    case duration(Duration)
    case vector(Vector)

    /// A graph node. `id` is the deprecated numeric entity id — exactly the
    /// INTEGER Cypher's `id()` returns for the same node (ADR-0506).
    /// `elementId` is the engine's element id (`"4:relate-neo4j:n0"`), spelled
    /// exactly as Cypher's `elementId()` returns it, so
    /// `WHERE elementId(n) = $id` round-trips.
    public struct Node: Sendable, Hashable {
        public let id: Int64
        public let elementId: String
        public let labels: [String]
        public let properties: [String: CypherValue]
    }

    /// A graph relationship, between the start and end nodes' ids and
    /// element ids (`id`/`startId`/`endId` are the deprecated numeric ids
    /// Cypher's `id()` / `id(startNode(r))` / `id(endNode(r))` return,
    /// ADR-0506).
    public struct Relationship: Sendable, Hashable {
        public let id: Int64
        public let elementId: String
        public let type: String
        public let startId: Int64
        public let endId: Int64
        public let startElementId: String
        public let endElementId: String
        public let properties: [String: CypherValue]
    }

    /// A path: `nodes.count == relationships.count + 1`, in walk order
    /// (`nodes[i]` and `nodes[i + 1]` flank `relationships[i]`).
    public struct Path: Sendable, Hashable {
        public let nodes: [Node]
        public let relationships: [Relationship]
    }

    /// A point: `srid` 7203/4326 (2D) or 9157/4979 (3D); `x`/`y` are
    /// longitude/latitude for the geographic srids. `z` is `nil` in 2D.
    public struct Point: Sendable, Hashable {
        public let srid: Int32
        public let x: Double
        public let y: Double
        public let z: Double?
        /// Memberwise, public: the spec's typed parameter binding
        /// constructs these values.
        public init(srid: Int32, x: Double, y: Double, z: Double?) {
            self.srid = srid
            self.x = x
            self.y = y
            self.z = z
        }
    }

    /// A calendar date as its three components.
    public struct Date: Sendable, Hashable {
        public let year: Int64
        public let month: Int64
        public let day: Int64
        /// Memberwise, public: the spec's typed parameter binding
        /// constructs these values.
        public init(year: Int64, month: Int64, day: Int64) {
            self.year = year
            self.month = month
            self.day = day
        }
    }

    /// A zoned time: the local wall clock plus its UTC offset. A TIME keeps
    /// no zone id on the engine side (the ABI's contract), so there is none
    /// here.
    public struct Time: Sendable, Hashable {
        public let hour: Int64
        public let minute: Int64
        public let second: Int64
        public let nanosecond: Int64
        public let offsetSeconds: Int64
        /// Memberwise, public: the spec's typed parameter binding
        /// constructs these values.
        public init(hour: Int64, minute: Int64, second: Int64, nanosecond: Int64, offsetSeconds: Int64) {
            self.hour = hour
            self.minute = minute
            self.second = second
            self.nanosecond = nanosecond
            self.offsetSeconds = offsetSeconds
        }
    }

    /// A zoneless time of day.
    public struct LocalTime: Sendable, Hashable {
        public let hour: Int64
        public let minute: Int64
        public let second: Int64
        public let nanosecond: Int64
        /// Memberwise, public: the spec's typed parameter binding
        /// constructs these values.
        public init(hour: Int64, minute: Int64, second: Int64, nanosecond: Int64) {
            self.hour = hour
            self.minute = minute
            self.second = second
            self.nanosecond = nanosecond
        }
    }

    /// A zoned datetime: the local wall clock, its UTC offset and — when the
    /// value carries one — the IANA zone id (`nil` for offset-only values).
    public struct DateTime: Sendable, Hashable {
        public let year: Int64
        public let month: Int64
        public let day: Int64
        public let hour: Int64
        public let minute: Int64
        public let second: Int64
        public let nanosecond: Int64
        public let offsetSeconds: Int64
        public let zone: String?
        /// Memberwise, public: the spec's typed parameter binding
        /// constructs these values.
        public init(year: Int64, month: Int64, day: Int64, hour: Int64, minute: Int64, second: Int64, nanosecond: Int64, offsetSeconds: Int64, zone: String?) {
            self.year = year
            self.month = month
            self.day = day
            self.hour = hour
            self.minute = minute
            self.second = second
            self.nanosecond = nanosecond
            self.offsetSeconds = offsetSeconds
            self.zone = zone
        }
    }

    /// A zoneless datetime.
    public struct LocalDateTime: Sendable, Hashable {
        public let year: Int64
        public let month: Int64
        public let day: Int64
        public let hour: Int64
        public let minute: Int64
        public let second: Int64
        public let nanosecond: Int64
        /// Memberwise, public: the spec's typed parameter binding
        /// constructs these values.
        public init(year: Int64, month: Int64, day: Int64, hour: Int64, minute: Int64, second: Int64, nanosecond: Int64) {
            self.year = year
            self.month = month
            self.day = day
            self.hour = hour
            self.minute = minute
            self.second = second
            self.nanosecond = nanosecond
        }
    }

    /// A duration as its four lossless components (`duration('P1Y2M')` is
    /// months = 14; `PT36H` stays 36 hours of seconds, not 1½ days).
    public struct Duration: Sendable, Hashable {
        public let months: Int64
        public let days: Int64
        public let seconds: Int64
        public let nanoseconds: Int64
        /// Memberwise, public: the spec's typed parameter binding
        /// constructs these values.
        public init(months: Int64, days: Int64, seconds: Int64, nanoseconds: Int64) {
            self.months = months
            self.days = days
            self.seconds = seconds
            self.nanoseconds = nanoseconds
        }
    }

    /// A vector: its coordinate dtype and coordinates. The int dtypes carry
    /// exact `Int64` values, the float dtypes `Double`s.
    public enum Vector: Sendable, Hashable {
        case floats(dtype: DType, values: [Double])
        case ints(dtype: DType, values: [Int64])

        /// The ABI's closed set of canonical coordinate-type names, matched
        /// exactly — never by prefix.
        public enum DType: String, Sendable, Hashable, CaseIterable {
            case integer8 = "INTEGER8"
            case integer16 = "INTEGER16"
            case integer32 = "INTEGER32"
            case integer = "INTEGER"
            case float16 = "FLOAT16"
            case bfloat16 = "BFLOAT16"
            case float32 = "FLOAT32"
            case float = "FLOAT"

            var isFloat: Bool {
                switch self {
                case .float16, .bfloat16, .float32, .float: true
                case .integer8, .integer16, .integer32, .integer: false
                }
            }
        }
    }
}
