import Testing
import Foundation
@testable import SNOTELCore

@Suite("BasinOutline Tests")
struct BasinOutlineTests {
    /// The square outline with a 2° square hole in its middle.
    private func squareWithHole() throws -> BasinOutline {
        try BasinOutline(rings: [try squareOutline().outer, ring((4, 4), (6, 4), (6, 6), (4, 6), (4, 4))])
    }

    @Test("Containment is inside the outer ring and outside every hole", arguments: [
        (2.0, 2.0, true),
        (5.0, 5.0, false),
        (11.0, 5.0, false),
    ])
    func containment(latitude: Double, longitude: Double, inside: Bool) throws {
        #expect(try squareWithHole().contains(latitude: latitude, longitude: longitude) == inside)
    }

    @Test("An outline survives being stored as GeoJSON text and parsed back")
    func roundTrip() throws {
        let outline = try squareWithHole()
        #expect(try BasinOutline(geoJSON: outline.geoJSON()) == outline)
    }

    @Test("Rings that are not a polygon are refused", arguments: [
        [],
        [[]],
        [ring((0, 0), (1, 1), (0, 0))],
        [ring((0, 0), (10, 0), (10, 10), (0, 0)), ring((4, 4))],
    ] as [[[BasinOutline.Position]]])
    func notAPolygon(rings: [[BasinOutline.Position]]) {
        #expect(throws: BasinOutline.NotAPolygon.self) { try BasinOutline(rings: rings) }
    }
}
