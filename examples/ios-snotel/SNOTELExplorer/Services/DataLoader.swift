import Foundation

/// One station of the bundled dataset. Its property names are the keys the
/// station load statement reads (`row.elevationFt`, …).
struct StationCSVRow: Encodable {
    let id: Int
    let name: String
    let state: String
    let latitude: Double
    let longitude: Double
    let elevationFt: Int
}

/// One reading of the bundled dataset. Its property names are the keys the
/// reading load statement reads (`row.stationId`, …).
struct ReadingCSVRow: Encodable {
    let stationId: Int
    let date: String
    let sweInches: Double
    let snowDepthInches: Double
    let tempAvgF: Int
}

struct BasinFeature {
    let name: String
    let huc2: String
    /// The feature's GeoJSON `geometry` object, serialized, for the map overlay.
    let geoJSON: String
    /// Polygon rings as `[longitude, latitude]` pairs; the first is the outer ring.
    let rings: [[[Double]]]

    /// Even-odd ray-casting containment: inside the outer ring and outside
    /// every hole. A point exactly on an edge may land on either side, which
    /// is fine for basin-scale polygons.
    func contains(latitude: Double, longitude: Double) -> Bool {
        func inRing(_ ring: [[Double]]) -> Bool {
            var inside = false
            var j = ring.count - 1
            for i in ring.indices {
                let (xi, yi, xj, yj) = (ring[i][0], ring[i][1], ring[j][0], ring[j][1])
                if (yi > latitude) != (yj > latitude),
                   longitude < (xj - xi) * (latitude - yi) / (yj - yi) + xi {
                    inside.toggle()
                }
                j = i
            }
            return inside
        }
        guard let outer = rings.first, inRing(outer) else { return false }
        return !rings.dropFirst().contains(where: inRing)
    }
}

/// A failure surfaced while loading a bundled data resource.
enum DataLoaderError: LocalizedError {
    /// A required resource (named `"<file>.<ext>"`) was not present in the bundle.
    case missingResource(String)
    /// A resource was present but a record in it did not parse; the payload
    /// names the record.
    case malformed(String)

    var errorDescription: String? {
        switch self {
        case .missingResource(let name):
            return "Bundled resource not found: \(name)"
        case .malformed(let record):
            return "Malformed bundled data: \(record)"
        }
    }
}

enum DataLoader {

    private static var resourceBundle: Bundle {
#if SWIFT_PACKAGE
        .module
#else
        .main
#endif
    }

    static func loadStationsCSV(from bundle: Bundle? = nil) throws -> [StationCSVRow] {
        try parseStationsCSV(resourceText("snotel_stations", "csv", from: bundle))
    }

    static func parseStationsCSV(_ csv: String) throws -> [StationCSVRow] {
        try csvRecords(csv).map { line in
            let cols = parseCSVLine(line)
            guard cols.count >= 6,
                  let id = Int(cols[0]),
                  let lat = Double(cols[3]),
                  let lon = Double(cols[4]),
                  let elev = Int(cols[5]) else {
                throw DataLoaderError.malformed("station row: \(line)")
            }
            return StationCSVRow(id: id, name: cols[1], state: cols[2],
                                 latitude: lat, longitude: lon, elevationFt: elev)
        }
    }

    static func loadReadingsCSV(from bundle: Bundle? = nil) throws -> [ReadingCSVRow] {
        try parseReadingsCSV(resourceText("snotel_readings", "csv", from: bundle))
    }

    static func parseReadingsCSV(_ csv: String) throws -> [ReadingCSVRow] {
        try csvRecords(csv).map { line in
            let cols = parseCSVLine(line)
            guard cols.count >= 5,
                  let stationId = Int(cols[0]),
                  let swe = Double(cols[2]),
                  let depth = Double(cols[3]),
                  let temp = Int(cols[4]) else {
                throw DataLoaderError.malformed("reading row: \(line)")
            }
            return ReadingCSVRow(stationId: stationId, date: cols[1],
                                sweInches: swe, snowDepthInches: depth, tempAvgF: temp)
        }
    }

    static func loadBasinsGeoJSON(from bundle: Bundle? = nil) throws -> [BasinFeature] {
        try parseBasinsGeoJSON(resourceData("basins", "json", from: bundle))
    }

    static func parseBasinsGeoJSON(_ data: Data) throws -> [BasinFeature] {
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            return try JSONDecoder().decode(BasinCollection.self, from: data).features.map { feature in
                BasinFeature(
                    name: feature.properties.name, huc2: feature.properties.huc2,
                    geoJSON: String(decoding: try encoder.encode(feature.geometry), as: UTF8.self),
                    rings: feature.geometry.coordinates)
            }
        } catch {
            // The coder's own error says which feature and key did not parse.
            throw DataLoaderError.malformed("basins: \(error)")
        }
    }

    /// The shape of the bundled basins document: a GeoJSON
    /// FeatureCollection of named Polygon features.
    private struct BasinCollection: Decodable {
        let features: [Feature]

        struct Feature: Decodable {
            let properties: Properties
            let geometry: Polygon
        }

        struct Properties: Decodable {
            let name: String
            let huc2: String
        }

        /// A GeoJSON Polygon geometry object; encoding it again gives the
        /// text the map overlay decodes.
        struct Polygon: Codable {
            let type: String
            let coordinates: [[[Double]]]
        }
    }

    // MARK: - Helpers

    /// The one bundled-resource lookup: the package's or app's bundle unless
    /// a caller supplies one, failing with the resource's name.
    private static func resourceData(_ name: String, _ ext: String, from bundle: Bundle?) throws -> Data {
        guard let url = (bundle ?? resourceBundle).url(forResource: name, withExtension: ext) else {
            throw DataLoaderError.missingResource("\(name).\(ext)")
        }
        return try Data(contentsOf: url)
    }

    private static func resourceText(_ name: String, _ ext: String, from bundle: Bundle?) throws -> String {
        guard let text = String(data: try resourceData(name, ext, from: bundle), encoding: .utf8) else {
            throw DataLoaderError.malformed("\(name).\(ext): not UTF-8")
        }
        return text
    }

    /// The data lines of a CSV document: blank lines and the header row dropped.
    private static func csvRecords(_ csv: String) -> ArraySlice<String> {
        csv.components(separatedBy: .newlines).filter { !$0.isEmpty }.dropFirst()
    }

    private static func parseCSVLine(_ line: String) -> [String] {
        var columns: [String] = []
        var field = ""
        var isQuoted = false
        var index = line.startIndex

        while index < line.endIndex {
            let character = line[index]
            if character == "\"" {
                let next = line.index(after: index)
                if isQuoted, next < line.endIndex, line[next] == "\"" {
                    field.append("\"")
                    index = line.index(after: next)
                    continue
                }
                isQuoted.toggle()
            } else if character == ",", !isQuoted {
                columns.append(field.trimmingCharacters(in: .whitespaces))
                field = ""
            } else {
                field.append(character)
            }
            index = line.index(after: index)
        }

        columns.append(field.trimmingCharacters(in: .whitespaces))
        return columns
    }
}
