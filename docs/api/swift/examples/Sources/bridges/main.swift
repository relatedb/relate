import CoreLocation
import Foundation
import Relate

let conn = try Database.inMemory().connect()

// A Foundation.Date binds as a UTC DATETIME, to the microsecond — no ISO
// string, no datetime($text) wrapper in the statement.
let finished = try Date.ISO8601FormatStyle(includingFractionalSeconds: true)
    .parse("2026-11-08T13:00:00.123Z")
try conn.query(
    "CREATE (:Book {title: $title, finished_at: $at, finished_on: $day})",
    [
        "title": "The Left Hand of Darkness",
        "at": try CypherValue(finished),
        // The reader's local calendar day, as a DATE.
        "day": .date(try .init(finished, in: TimeZone(identifier: "Pacific/Auckland")!)),
    ])

let row = try conn.query("MATCH (b:Book) RETURN b.finished_at AS at, b.finished_on AS day").records[0]
let readBack = row["at"]!.dateTimeValue!.foundationDate
print("finished:", readBack.formatted(.iso8601), "local day:", row["day"]!)
precondition(readBack == finished)
let day = row["day"]!.dateValue!.dateComponents
precondition(day.year == 2026 && day.month == 11 && day.day == 9)

// A fixed-length Swift.Duration; a duration with months or days has none.
let listened = CypherValue.Duration(.seconds(5_400))
let fixed = try conn.query("RETURN $d", ["d": .duration(listened)]).scalar().durationValue!
precondition(fixed.swiftDuration == .seconds(5_400))
let calendar = try conn.query("RETURN duration('P1M')").scalar().durationValue!
precondition(calendar.swiftDuration == nil)

// Float embeddings bind as a FLOAT32 vector and read back exactly.
let embedding: [Float] = [0.12, -0.5, 0.33, 0.9]
try conn.query("MATCH (b:Book) SET b.embedding = $v", ["v": CypherValue(vector: embedding)])
let stored = try conn.query("MATCH (b:Book) RETURN b.embedding").scalar().vectorValue!
print("embedding:", stored.dtype, stored.count, "coordinates")
precondition(stored.dtype == .float32 && stored.float32Values == embedding)

// A point by latitude and longitude: the labels carry the order.
let missoula = CypherValue.Point(latitude: 46.87, longitude: -113.99)
try conn.query("CREATE (:Place {name: 'Missoula', location: $p})", ["p": .point(missoula)])
let place = try conn.query("MATCH (p:Place) RETURN p.location").scalar().pointValue!
precondition(place.crs == .wgs84 && place.latitude == 46.87 && place.longitude == -113.99)

// Core Location both ways.
let coordinate: CLLocationCoordinate2D = place.coordinate!
precondition(CypherValue(coordinate) == .point(missoula))
let metres = try conn.query(
    "MATCH (p:Place) RETURN point.distance(p.location, $here)",
    ["here": CypherValue(CLLocationCoordinate2D(latitude: 46.87, longitude: -113.99))]
).scalar().doubleValue!
print("distance:", metres, "m")
precondition(metres < 1)
