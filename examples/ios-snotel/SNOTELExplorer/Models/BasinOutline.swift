import Foundation

/// A basin's outline: a GeoJSON Polygon geometry object, as its outer ring
/// and any holes.
///
/// This is the app's one reading of that geometry. The loader decodes a
/// feature's `geometry` into it, tests stations against it and stores it as
/// text; a basin row parses the stored text back into it. A geometry that
/// is not a Polygon therefore fails where it is read — the load, or the
/// basin query — and never reaches the map.
struct BasinOutline: Equatable, Sendable, Codable {
    let outer: [Position]
    let holes: [[Position]]

    /// A GeoJSON position: `[longitude, latitude]`. A third member (an
    /// altitude) is not kept; a position with fewer than two is refused.
    struct Position: Equatable, Sendable, Codable {
        let longitude: Double
        let latitude: Double

        init(longitude: Double, latitude: Double) {
            self.longitude = longitude
            self.latitude = latitude
        }

        init(from decoder: Decoder) throws {
            var members = try decoder.unkeyedContainer()
            longitude = try members.decode(Double.self)
            latitude = try members.decode(Double.self)
        }

        func encode(to encoder: Encoder) throws {
            var members = encoder.unkeyedContainer()
            try members.encode(longitude)
            try members.encode(latitude)
        }
    }

    /// Why a list of rings is not a polygon.
    struct NotAPolygon: Error, CustomStringConvertible {
        let description: String
    }

    /// GeoJSON's shortest linear ring: three positions, and the first
    /// again to close it.
    private static let shortestRing = 4

    /// Whether `ring` is a GeoJSON linear ring: at least `shortestRing`
    /// positions, the last of them the first again.
    private static func isLinearRing(_ ring: [Position]) -> Bool {
        ring.count >= shortestRing && ring.first == ring.last
    }

    /// The outline with these linear rings: the outer one, then any holes.
    /// This is the only way to make an outline, so every outline has an
    /// outer ring and every ring of one is closed and encloses something.
    init(rings: [[Position]]) throws(NotAPolygon) {
        guard let outer = rings.first else {
            throw NotAPolygon(description: "a \(Self.geometryType) needs an outer ring")
        }
        guard rings.allSatisfy(Self.isLinearRing) else {
            throw NotAPolygon(
                description: "every ring of a \(Self.geometryType) needs at least \(Self.shortestRing) positions, "
                    + "the last equal to the first")
        }
        self.outer = outer
        holes = Array(rings.dropFirst())
    }

    private enum CodingKeys: String, CodingKey {
        case type, coordinates
    }

    /// The GeoJSON geometry type an outline is.
    private static let geometryType = "Polygon"

    init(from decoder: Decoder) throws {
        let geometry = try decoder.container(keyedBy: CodingKeys.self)
        let type = try geometry.decode(String.self, forKey: .type)
        guard type == Self.geometryType else {
            throw DecodingError.dataCorruptedError(
                forKey: .type, in: geometry,
                debugDescription: "a basin outline is a \(Self.geometryType), not a \(type)")
        }
        let rings = try geometry.decode([[Position]].self, forKey: .coordinates)
        do {
            self = try BasinOutline(rings: rings)
        } catch {
            throw DecodingError.dataCorruptedError(
                forKey: .coordinates, in: geometry, debugDescription: error.description)
        }
    }

    func encode(to encoder: Encoder) throws {
        var geometry = encoder.container(keyedBy: CodingKeys.self)
        try geometry.encode(Self.geometryType, forKey: .type)
        try geometry.encode([outer] + holes, forKey: .coordinates)
    }

    /// The outline a GeoJSON geometry text describes: what the graph
    /// stores as `Basin.geojson`.
    init(geoJSON text: String) throws {
        self = try JSONDecoder().decode(BasinOutline.self, from: Data(text.utf8))
    }

    /// The outline as GeoJSON geometry text, for the graph to store.
    func geoJSON() throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return String(decoding: try encoder.encode(self), as: UTF8.self)
    }

    /// Even-odd ray-casting containment: inside the outer ring and outside
    /// every hole. A point exactly on an edge may land on either side, which
    /// is fine for basin-scale polygons.
    func contains(latitude: Double, longitude: Double) -> Bool {
        func inRing(_ ring: [Position]) -> Bool {
            var inside = false
            var j = ring.count - 1
            for i in ring.indices {
                let (xi, yi, xj, yj) = (ring[i].longitude, ring[i].latitude, ring[j].longitude, ring[j].latitude)
                if (yi > latitude) != (yj > latitude),
                   longitude < (xj - xi) * (latitude - yi) / (yj - yi) + xi {
                    inside.toggle()
                }
                j = i
            }
            return inside
        }
        return inRing(outer) && !holes.contains(where: inRing)
    }
}
