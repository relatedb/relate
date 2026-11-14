// swift-tools-version: 6.0
// The Swift surface (ADR-0433). SwiftPM resolves a git dependency from the
// repository root, so the manifest lives here and the sources live in
// clients/swift.
import Foundation
import PackageDescription

// Stamped into the release commit by the release workflow's `version` job
// (scripts/stamp-swift-package.sh) from the XCFramework zip it attaches to the
// GitHub release. Never edit by hand. Between releases they name the previous
// release.
let releaseTag = "v1.0.2"
let releaseChecksum = "079b7350f8066fe0ecd428cae994793dfc4aed59a6dc32aa19fa1142f3b175e3"

// RELATE_LOCAL_XCFRAMEWORK=1 links the locally built binary
// (scripts/build-xcframework.sh) instead: a checkout that builds from source
// must link the engine it was built from, not the last release. The switch is
// an environment variable, not a file-existence check, because SwiftPM caches
// manifest evaluation keyed on the manifest and the environment — a file
// appearing later would not be seen. Before the first release there is no
// remote binary, so the path target is the only option and its "does not
// exist" error names the missing build.
let localXCFramework = "target/apple/RelateFFI.xcframework"
let useLocalXCFramework = ProcessInfo.processInfo.environment["RELATE_LOCAL_XCFRAMEWORK"] == "1"

let ffiTarget: Target = useLocalXCFramework || releaseChecksum.isEmpty
    ? .binaryTarget(name: "RelateFFI", path: localXCFramework)
    : .binaryTarget(
        name: "RelateFFI",
        url: "https://github.com/relatedb/relate/releases/download/\(releaseTag)/RelateFFI.xcframework.zip",
        checksum: releaseChecksum
    )

let package = Package(
    name: "Relate",
    platforms: [.iOS(.v16), .macOS(.v13)],
    products: [
        .library(name: "Relate", targets: ["Relate"]),
    ],
    targets: [
        ffiTarget,
        .target(
            name: "Relate",
            dependencies: ["RelateFFI"],
            path: "clients/swift/Sources/Relate",
            resources: [.copy("PrivacyInfo.xcprivacy")]
        ),
        .testTarget(
            name: "RelateTests",
            dependencies: ["Relate"],
            path: "clients/swift/Tests/RelateTests"
        ),
    ],
    swiftLanguageModes: [.v6]
)
