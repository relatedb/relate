import CoreLocation
import MapKit
import SwiftUI

struct MapContainerView: UIViewRepresentable {
    /// What the map shows, from the screen's live queries.
    var stations: [Station]
    var basins: [Basin]
    var viewModel: MapViewModel

    func makeUIView(context: Context) -> MKMapView {
        let mapView = MKMapView(frame: .zero)
        let configuration = MKStandardMapConfiguration(elevationStyle: .flat)
        configuration.emphasisStyle = .muted
        configuration.pointOfInterestFilter = .excludingAll
        mapView.preferredConfiguration = configuration
        mapView.delegate = context.coordinator
        mapView.isPitchEnabled = false
        mapView.isRotateEnabled = false
        mapView.showsCompass = false
        mapView.showsScale = true
        mapView.register(
            StationAnnotationView.self,
            forAnnotationViewWithReuseIdentifier: Coordinator.stationReuseIdentifier
        )
        mapView.register(
            StationClusterAnnotationView.self,
            forAnnotationViewWithReuseIdentifier: Coordinator.clusterReuseIdentifier
        )
        mapView.setCameraZoomRange(
            MKMapView.CameraZoomRange(
                minCenterCoordinateDistance: 15_000,
                maxCenterCoordinateDistance: 6_000_000
            ),
            animated: false
        )
        mapView.setRegion(Coordinator.westernUSRegion, animated: false)
        return mapView
    }

    func updateUIView(_ mapView: MKMapView, context: Context) {
        context.coordinator.viewModel = viewModel
        context.coordinator.stations = stations
        context.coordinator.basins = basins
        context.coordinator.updateStations(on: mapView)
        context.coordinator.updateBasinOverlays(on: mapView)
        context.coordinator.updateBasinStyles(on: mapView)
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(viewModel: viewModel)
    }

    @MainActor
    final class Coordinator: NSObject, MKMapViewDelegate {
        static let stationReuseIdentifier = "snotel-station"
        static let clusterReuseIdentifier = "snotel-cluster"
        static let westernUSRegion = MKCoordinateRegion(
            center: CLLocationCoordinate2D(latitude: 43, longitude: -114),
            span: MKCoordinateSpan(latitudeDelta: 15, longitudeDelta: 24)
        )

        var viewModel: MapViewModel
        var stations: [Station] = []
        var basins: [Basin] = []
        private var stationAnnotations: [StationAnnotation] = []
        private var appliedStations: [Station] = []
        private var basinByOverlay: [ObjectIdentifier: Basin] = [:]
        private var overlaidBasins: [Basin] = []
        private var styledSelection: String?
        private var wasFiltered = false

        init(viewModel: MapViewModel) {
            self.viewModel = viewModel
        }

        func updateStations(on mapView: MKMapView) {
            guard stations != appliedStations else { return }

            mapView.removeAnnotations(stationAnnotations)
            stationAnnotations = stations.map(StationAnnotation.init)
            mapView.addAnnotations(stationAnnotations)
            appliedStations = stations

            if viewModel.isFiltered, !stationAnnotations.isEmpty {
                fit(stationAnnotations, on: mapView, animated: true)
            } else if wasFiltered, !viewModel.isFiltered {
                mapView.setRegion(Self.westernUSRegion, animated: true)
            }
            wasFiltered = viewModel.isFiltered
        }

        /// Builds the basin overlays from the geometry the graph stores
        /// (`Basin.geoJSON`, a GeoJSON geometry object), once per basin list;
        /// each overlay keeps its `Basin`, so styling reads it directly.
        func updateBasinOverlays(on mapView: MKMapView) {
            guard basins != overlaidBasins else { return }
            overlaidBasins = basins

            mapView.removeOverlays(mapView.overlays)
            basinByOverlay.removeAll()
            var overlays: [MKOverlay] = []
            for basin in basins {
                let objects: [MKGeoJSONObject]
                do {
                    objects = try MKGeoJSONDecoder().decode(Data(basin.geoJSON.utf8))
                } catch {
                    // The loader wrote this geometry from a parsed Polygon.
                    preconditionFailure("basin \(basin.name): stored geometry is not GeoJSON: \(error)")
                }
                for case let overlay as MKOverlay in objects {
                    overlays.append(overlay)
                    basinByOverlay[ObjectIdentifier(overlay as AnyObject)] = basin
                }
            }
            mapView.addOverlays(overlays, level: .aboveRoads)
        }

        /// Restyles the live renderers when the basin selection changes; a
        /// rebuilt overlay list needs no restyle, since each new renderer is
        /// styled as `mapView(_:rendererFor:)` creates it.
        func updateBasinStyles(on mapView: MKMapView) {
            guard viewModel.selectedBasinName != styledSelection else { return }
            styledSelection = viewModel.selectedBasinName

            for overlay in mapView.overlays {
                guard let renderer = mapView.renderer(for: overlay) as? MKOverlayPathRenderer else {
                    continue
                }
                style(renderer, for: overlay)
                renderer.setNeedsDisplay()
            }
        }

        func mapView(_ mapView: MKMapView, viewFor annotation: MKAnnotation) -> MKAnnotationView? {
            if annotation is MKClusterAnnotation {
                return mapView.dequeueReusableAnnotationView(
                    withIdentifier: Self.clusterReuseIdentifier,
                    for: annotation
                )
            }
            guard annotation is StationAnnotation else { return nil }
            return mapView.dequeueReusableAnnotationView(
                withIdentifier: Self.stationReuseIdentifier,
                for: annotation
            )
        }

        func mapView(_ mapView: MKMapView, didSelect view: MKAnnotationView) {
            if let cluster = view.annotation as? MKClusterAnnotation {
                fit(cluster.memberAnnotations, on: mapView, animated: true)
                mapView.deselectAnnotation(cluster, animated: true)
                return
            }
            guard let annotation = view.annotation as? StationAnnotation else { return }
            viewModel.selectedStation = stations.first { $0.id == annotation.stationID }
            mapView.deselectAnnotation(annotation, animated: true)
        }

        func mapView(_ mapView: MKMapView, rendererFor overlay: MKOverlay) -> MKOverlayRenderer {
            let renderer: MKOverlayPathRenderer
            if let polygon = overlay as? MKPolygon {
                renderer = MKPolygonRenderer(polygon: polygon)
            } else if let multiPolygon = overlay as? MKMultiPolygon {
                renderer = MKMultiPolygonRenderer(multiPolygon: multiPolygon)
            } else {
                return MKOverlayRenderer(overlay: overlay)
            }
            renderer.lineJoin = .round
            renderer.lineCap = .round
            style(renderer, for: overlay)
            return renderer
        }

        private func style(_ renderer: MKOverlayPathRenderer, for overlay: MKOverlay) {
            let basin = basinByOverlay[ObjectIdentifier(overlay as AnyObject)]
            let color = UIColor(SWELevel.from(swe: basin?.avgSWE).color)
            let emphasis = BasinEmphasis(basin: basin?.name, selected: viewModel.selectedBasinName)

            renderer.fillColor = color.withAlphaComponent(emphasis.fillAlpha)
            renderer.strokeColor = color.withAlphaComponent(emphasis.strokeAlpha)
            renderer.lineWidth = emphasis.lineWidth
        }

        private func fit(
            _ annotations: [MKAnnotation],
            on mapView: MKMapView,
            animated: Bool
        ) {
            guard !annotations.isEmpty else { return }
            if annotations.count == 1, let coordinate = annotations.first?.coordinate {
                mapView.setRegion(
                    MKCoordinateRegion(
                        center: coordinate,
                        span: MKCoordinateSpan(latitudeDelta: 1.7, longitudeDelta: 1.7)
                    ),
                    animated: animated
                )
                return
            }

            let mapRect = annotations.reduce(MKMapRect.null) { partial, annotation in
                let point = MKMapPoint(annotation.coordinate)
                let rect = MKMapRect(x: point.x, y: point.y, width: 1, height: 1)
                return partial.union(rect)
            }
            let bottomInset = min(mapView.bounds.height * 0.17, 170)
            mapView.setVisibleMapRect(
                mapRect,
                edgePadding: UIEdgeInsets(top: 115, left: 45, bottom: bottomInset, right: 45),
                animated: animated
            )
        }
    }
}

/// A basin overlay's visual weight: the selected basin stands out and the
/// others recede while one is selected; with none selected all are plain.
private enum BasinEmphasis {
    case selected, dimmed, plain

    init(basin: String?, selected: String?) {
        guard let selected else { self = .plain; return }
        self = basin == selected ? .selected : .dimmed
    }

    var fillAlpha: CGFloat {
        switch self {
        case .selected: return 0.34
        case .dimmed: return 0.055
        case .plain: return 0.20
        }
    }

    var strokeAlpha: CGFloat {
        switch self {
        case .selected: return 0.95
        case .dimmed: return 0.28
        case .plain: return 0.72
        }
    }

    var lineWidth: CGFloat {
        switch self {
        case .selected: return 2.75
        case .dimmed: return 0.75
        case .plain: return 1.15
        }
    }
}

private final class StationAnnotation: MKPointAnnotation {
    let stationID: Int
    let sweInches: Double?
    let elevation: Int

    var sweLevel: SWELevel { SWELevel.from(swe: sweInches) }

    init(station: Station) {
        stationID = station.id
        sweInches = station.sweInches
        elevation = station.elevation
        super.init()
        coordinate = CLLocationCoordinate2D(
            latitude: station.latitude,
            longitude: station.longitude
        )
        title = station.name
        subtitle = station.sweInches.map {
            String(format: "SWE: %.1f\" • %d ft", $0, station.elevation)
        } ?? "No SWE data • \(station.elevation) ft"
    }
}

private final class StationAnnotationView: MKAnnotationView {
    private static let size: CGFloat = 20

    override var annotation: MKAnnotation? {
        didSet { applyStyle() }
    }

    override init(annotation: MKAnnotation?, reuseIdentifier: String?) {
        super.init(annotation: annotation, reuseIdentifier: reuseIdentifier)
        frame = CGRect(x: 0, y: 0, width: Self.size, height: Self.size)
        centerOffset = .zero
        collisionMode = .circle
        clusteringIdentifier = "snotel"
        displayPriority = .defaultHigh
        canShowCallout = false
        layer.cornerRadius = Self.size / 2
        layer.borderWidth = 2.25
        layer.borderColor = UIColor.white.cgColor
        layer.shadowColor = UIColor.black.cgColor
        layer.shadowOpacity = 0.28
        layer.shadowRadius = 2.5
        layer.shadowOffset = CGSize(width: 0, height: 1)
        isAccessibilityElement = true
        applyStyle()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func setSelected(_ selected: Bool, animated: Bool) {
        super.setSelected(selected, animated: animated)
        let changes = { self.transform = selected ? CGAffineTransform(scaleX: 1.35, y: 1.35) : .identity }
        animated ? UIView.animate(withDuration: 0.18, animations: changes) : changes()
    }

    private func applyStyle() {
        guard let annotation = annotation as? StationAnnotation else { return }
        backgroundColor = UIColor(annotation.sweLevel.color)
        accessibilityLabel = annotation.title ?? "SNOTEL station"
        accessibilityValue = annotation.sweInches.map {
            String(format: "Snow water equivalent %.1f inches, elevation %d feet", $0, annotation.elevation)
        } ?? "No snow water equivalent data, elevation \(annotation.elevation) feet"
    }
}

private final class StationClusterAnnotationView: MKAnnotationView {
    private let countLabel = UILabel()

    override var annotation: MKAnnotation? {
        didSet { applyStyle() }
    }

    override init(annotation: MKAnnotation?, reuseIdentifier: String?) {
        super.init(annotation: annotation, reuseIdentifier: reuseIdentifier)
        frame = CGRect(x: 0, y: 0, width: 34, height: 34)
        collisionMode = .circle
        displayPriority = .required
        layer.cornerRadius = 17
        layer.borderWidth = 2.5
        layer.borderColor = UIColor.white.cgColor
        layer.shadowColor = UIColor.black.cgColor
        layer.shadowOpacity = 0.25
        layer.shadowRadius = 3
        layer.shadowOffset = CGSize(width: 0, height: 1.5)

        countLabel.frame = bounds
        countLabel.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        countLabel.textAlignment = .center
        countLabel.textColor = .white
        countLabel.font = .monospacedDigitSystemFont(ofSize: 12, weight: .bold)
        addSubview(countLabel)
        isAccessibilityElement = true
        applyStyle()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    private func applyStyle() {
        guard let cluster = annotation as? MKClusterAnnotation else { return }
        let stations = cluster.memberAnnotations.compactMap { $0 as? StationAnnotation }
        let values = stations.compactMap(\.sweInches)
        let average = values.isEmpty ? nil : values.reduce(0, +) / Double(values.count)
        backgroundColor = UIColor(SWELevel.from(swe: average).color)
        countLabel.text = "\(stations.count)"
        accessibilityLabel = "\(stations.count) SNOTEL stations"
        accessibilityHint = "Double tap to zoom in"
    }
}
