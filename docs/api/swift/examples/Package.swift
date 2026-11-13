// swift-tools-version: 6.0
// Runnable examples for docs/api/swift (ADR-0433). Each executable asserts
// its own output, so `swift run <name>` exiting 0 is the test; CI runs them
// all with `make swift-examples`.
import PackageDescription

let examples = [
    "quickstart", "transactions", "result-metadata", "namespaces", "persistence", "errors",
    "bridges",
]

let package = Package(
    name: "RelateSwiftExamples",
    platforms: [.macOS(.v13)],
    dependencies: [.package(path: "../../../..")],
    targets: examples.map { name in
        .executableTarget(
            name: name,
            dependencies: [.product(name: "Relate", package: "relate")],
            path: "Sources/\(name)"
        )
    },
    swiftLanguageModes: [.v6]
)
