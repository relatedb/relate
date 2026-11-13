// MARK: - Point conveniences

extension CypherValue.Point {
    /// The four coordinate reference systems a point can carry, by srid.
    public enum CRS: Int32, Sendable, Hashable, CaseIterable {
        /// 2D Cartesian (`x`, `y`).
        case cartesian = 7203
        /// 3D Cartesian (`x`, `y`, `z`).
        case cartesian3D = 9157
        /// WGS 84 longitude and latitude in degrees.
        case wgs84 = 4326
        /// WGS 84 longitude, latitude and height in metres.
        case wgs84_3D = 4979

        /// Whether `x`/`y` are longitude/latitude.
        public var isGeographic: Bool {
            switch self {
            case .wgs84, .wgs84_3D: true
            case .cartesian, .cartesian3D: false
            }
        }
    }

    /// This point's coordinate reference system; `nil` for a srid outside
    /// the four the engine defines.
    public var crs: CRS? { CRS(rawValue: srid) }

    /// A WGS 84 point from a latitude and longitude in degrees (and an
    /// optional height in metres). The labels carry the order, so the
    /// longitude cannot land in `y` by accident.
    public init(latitude: Double, longitude: Double, height: Double? = nil) {
        self.init(
            srid: (height == nil ? CRS.wgs84 : CRS.wgs84_3D).rawValue,
            x: longitude, y: latitude, z: height)
    }

    /// A Cartesian point (2D, or 3D with `z`).
    public init(x: Double, y: Double, z: Double? = nil) {
        self.init(srid: (z == nil ? CRS.cartesian : CRS.cartesian3D).rawValue, x: x, y: y, z: z)
    }

    /// Latitude in degrees; `nil` unless the point is geographic.
    public var latitude: Double? { crs?.isGeographic == true ? y : nil }

    /// Longitude in degrees; `nil` unless the point is geographic.
    public var longitude: Double? { crs?.isGeographic == true ? x : nil }

    /// Height in metres; `nil` unless the point is 3D geographic.
    public var height: Double? { crs == .wgs84_3D ? z : nil }
}

#if canImport(CoreLocation)
import CoreLocation

extension CypherValue.Point {
    /// A 2D WGS 84 point at a Core Location coordinate.
    public init(_ coordinate: CLLocationCoordinate2D) {
        self.init(latitude: coordinate.latitude, longitude: coordinate.longitude)
    }

    /// The point as a Core Location coordinate; `nil` unless the point is
    /// geographic (a 3D point's height is dropped).
    public var coordinate: CLLocationCoordinate2D? {
        guard let latitude, let longitude else { return nil }
        return CLLocationCoordinate2D(latitude: latitude, longitude: longitude)
    }
}

extension CypherValue {
    /// A 2D WGS 84 `.point` at a Core Location coordinate.
    public init(_ coordinate: CLLocationCoordinate2D) {
        self = .point(Point(coordinate))
    }
}
#endif
