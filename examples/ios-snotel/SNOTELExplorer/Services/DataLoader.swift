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

/// One basin of the bundled document: its name, its HUC2 code and its
/// outline, which decides which stations it contains.
struct BasinFeature {
    let name: String
    let huc2: String
    let outline: BasinOutline
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
            return try JSONDecoder().decode(BasinCollection.self, from: data).features.map { feature in
                BasinFeature(
                    name: feature.properties.name, huc2: feature.properties.huc2, outline: feature.geometry)
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
            let geometry: BasinOutline
        }

        struct Properties: Decodable {
            let name: String
            let huc2: String
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
