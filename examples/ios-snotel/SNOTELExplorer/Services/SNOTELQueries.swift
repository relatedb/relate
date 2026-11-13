import CoreLocation
import Relate

/// Which stations the map shows: exactly one filter is active at a time.
enum StationFilter {
    case all
    case basin(String)
    case nearby(CLLocationCoordinate2D, radiusKm: Double)
    case search(String)
}

/// The app's reads, as values. A view shows one with `@LiveQuery` and is
/// redrawn when a write changes its answer; a test runs the same value with
/// `Database.read`. Every input is bound as a parameter, never spliced into
/// the text.
enum SNOTELQueries {
    /// The stations `filter` selects, each with its SWE reading, as the
    /// columns `Station` decodes. Throws for a center that is not a point
    /// on the globe.
    static func stations(_ filter: StationFilter) throws -> Statement {
        switch filter {
        case .all:
            stations(matching: "MATCH (s:Station)")
        case .basin(let name):
            stations(
                matching: "MATCH (s:Station)-[:IN_BASIN]->(:Basin {name: $basinName})",
                parameters: ["basinName": .string(name)])
        case .nearby(let center, let radiusKm):
            try stations(
                matching: """
                MATCH (s:Station)
                WHERE point.distance(s.location, $center) < $radiusMeters
                """,
                extraColumns: ", point.distance(s.location, $center) AS distanceMeters",
                orderBy: "distanceMeters",
                parameters: [
                    // A typed point parameter: the labels carry the order.
                    "center": .point(.init(latitude: center.latitude, longitude: center.longitude)),
                    "radiusMeters": .float(radiusKm * 1000),
                ])
        case .search(let text):
            stations(
                matching: """
                MATCH (s:Station)
                WHERE toLower(s.name) CONTAINS toLower($searchText)
                   OR toLower(s.state) = toLower($searchText)
                """,
                parameters: ["searchText": .string(text)])
        }
    }

    /// The stations inside a bounding box, by the point index.
    static func stations(
        southWest: CLLocationCoordinate2D, northEast: CLLocationCoordinate2D
    ) throws -> Statement {
        try stations(
            matching: "MATCH (s:Station) WHERE point.withinBBox(s.location, $southWest, $northEast)",
            parameters: [
                "southWest": .point(.init(latitude: southWest.latitude, longitude: southWest.longitude)),
                "northEast": .point(.init(latitude: northEast.latitude, longitude: northEast.longitude)),
            ])
    }

    /// Every basin with its stored GeoJSON geometry and its snowpack
    /// aggregates: the mean SWE of its stations' readings and how many
    /// stations report one (nil and 0 for a basin without readings).
    static let basins: Statement = """
        MATCH (b:Basin)
        OPTIONAL MATCH (b)<-[:IN_BASIN]-(s:Station)-[:HAS_READING]->(r:Reading)
        RETURN b.name AS name, b.huc2 AS huc2, b.geojson AS geoJSON,
               avg(r.swe_inches) AS avgSWE, count(DISTINCT s) AS stationCount
        ORDER BY b.name
        """

    /// The five counts of the insights screen, read in one snapshot.
    static let stats: @Sendable (Transaction) throws -> DatabaseStats = { tx in
        func count(_ statement: Statement) throws -> Int {
            try tx.run(statement).scalar(as: Int.self)
        }
        return DatabaseStats(
            stations: try count("MATCH (n:Station) RETURN count(n)"),
            readings: try count("MATCH (n:Reading) RETURN count(n)"),
            basins: try count("MATCH (n:Basin) RETURN count(n)"),
            relationships: try count("MATCH ()-[r]->() RETURN count(r)"),
            basinMemberships: try count("MATCH ()-[r:IN_BASIN]->() RETURN count(r)"))
    }

    /// The one station projection: `match` binds `s` (and filters it); every
    /// station query shares the OPTIONAL MATCH on readings, the columns
    /// `Station` decodes, and the ordering.
    private static func stations(
        matching match: String,
        extraColumns: String = "",
        orderBy: String = "s.name",
        parameters: [String: CypherValue] = [:]
    ) -> Statement {
        Statement(
            """
            \(match)
            OPTIONAL MATCH (s)-[:HAS_READING]->(r:Reading)
            RETURN s.id AS id, s.name AS name, s.state AS state,
                   s.latitude AS latitude, s.longitude AS longitude,
                   s.elevation AS elevation, r.swe_inches AS sweInches\(extraColumns)
            ORDER BY \(orderBy)
            """,
            parameters)
    }
}
