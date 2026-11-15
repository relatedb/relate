import CoreLocation
import Relate

/// Which stations the map shows: exactly one filter is active at a time.
enum StationFilter {
    case all
    /// The stations of one basin, by the basin's identity: its HUC2 code.
    case basin(Basin.ID)
    case nearby(CLLocationCoordinate2D, radiusKm: Double)
    case search(String)
}

/// The app's reads, as values. A view shows one with `@LiveQuery` and is
/// redrawn when a write changes its answer; a test runs the same value with
/// `Database.read`.
///
/// Every value a query depends on — a basin, a center, a radius, the text
/// of a search — is bound as a parameter. The only text assembled here is
/// this file's own constants: a station pattern, the shared reading match
/// and the shared column list.
enum SNOTELQueries {
    /// The stations `filter` selects, each with its SWE reading, as the
    /// columns `Station` decodes. Throws for a center that is not a point
    /// on the globe.
    static func stations(_ filter: StationFilter) throws -> Statement {
        switch filter {
        case .all:
            stationsByName(matching: "MATCH (s:Station)")
        case .basin(let huc2):
            stationsByName(
                matching: "MATCH (s:Station)-[:IN_BASIN]->(:Basin {huc2: $huc2})",
                parameters: ["huc2": .string(huc2)])
        case .nearby(let center, let radiusKm):
            // The one query with a column of its own, which it also sorts
            // by: the distance to the center.
            Statement(
                """
                MATCH (s:Station)
                WHERE point.distance(s.location, $center) < $radiusMeters
                \(readingMatch)
                RETURN \(stationColumns), point.distance(s.location, $center) AS distanceMeters
                ORDER BY distanceMeters
                """,
                [
                    // A typed point parameter: the labels carry the order.
                    "center": .point(try .init(latitude: center.latitude, longitude: center.longitude)),
                    "radiusMeters": .float(radiusKm * 1000),
                ])
        case .search(let text):
            stationsByName(
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
        stationsByName(
            matching: "MATCH (s:Station) WHERE point.withinBBox(s.location, $southWest, $northEast)",
            parameters: [
                "southWest": .point(try .init(latitude: southWest.latitude, longitude: southWest.longitude)),
                "northEast": .point(try .init(latitude: northEast.latitude, longitude: northEast.longitude)),
            ])
    }

    /// Every basin with its stored geometry (GeoJSON text, which `Basin`
    /// parses into its outline) and its snowpack aggregates: the mean SWE of its stations' readings and how many
    /// stations report one (nil and 0 for a basin without readings).
    static let basins: Statement = """
        MATCH (b:Basin)
        OPTIONAL MATCH (b)<-[:IN_BASIN]-(s:Station)-[:HAS_READING]->(r:Reading)
        RETURN b.name AS name, b.huc2 AS huc2, b.geojson AS geoJSON,
               avg(r.swe_inches) AS avgSWE, count(DISTINCT s) AS stationCount
        ORDER BY b.name
        """

    /// The insights screen's value: the five counts and the basins, read
    /// in one snapshot.
    static let insights: @Sendable (Transaction) throws -> Insights = { tx in
        func count(_ statement: Statement) throws -> Int {
            try tx.run(statement).scalar(as: Int.self)
        }
        return Insights(
            stats: DatabaseStats(
                stations: try count("MATCH (n:Station) RETURN count(n)"),
                readings: try count("MATCH (n:Reading) RETURN count(n)"),
                basins: try count("MATCH (n:Basin) RETURN count(n)"),
                relationships: try count("MATCH ()-[r]->() RETURN count(r)"),
                basinMemberships: try count("MATCH ()-[r:IN_BASIN]->() RETURN count(r)")),
            basins: try tx.run(basins).decode(Basin.self))
    }

    /// The reading of a station bound as `s`, as `r`; a station without
    /// one keeps its row.
    private static let readingMatch = "OPTIONAL MATCH (s)-[:HAS_READING]->(r:Reading)"

    /// The columns `Station` decodes, of a station `s` and its reading `r`.
    private static let stationColumns = """
        s.id AS id, s.name AS name, s.state AS state,
        s.latitude AS latitude, s.longitude AS longitude,
        s.elevation AS elevation, r.swe_inches AS sweInches
        """

    /// The stations `match` binds as `s`, with their readings, in name
    /// order. `match` is one of this file's constant patterns; its values
    /// arrive in `parameters`.
    private static func stationsByName(
        matching match: String, parameters: [String: CypherValue] = [:]
    ) -> Statement {
        Statement(
            """
            \(match)
            \(readingMatch)
            RETURN \(stationColumns)
            ORDER BY s.name
            """,
            parameters)
    }
}
