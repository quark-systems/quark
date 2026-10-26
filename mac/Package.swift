// swift-tools-version:5.9
import PackageDescription

// The Quark menubar app. It is a client of quarkd only: every fact it shows and
// every action it takes goes through quarkd's HTTP API (see api/openapi.json).
let package = Package(
    name: "QuarkMenuBar",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "QuarkKit", targets: ["QuarkKit"]),
        .executable(name: "QuarkMenuBar", targets: ["QuarkMenuBar"]),
    ],
    targets: [
        // quarkd API models and client. No AppKit, so it builds and tests anywhere Swift runs.
        .target(name: "QuarkKit"),
        .executableTarget(name: "QuarkMenuBar", dependencies: ["QuarkKit"]),
        .testTarget(
            name: "QuarkKitTests",
            dependencies: ["QuarkKit"],
            resources: [.copy("Fixtures")]
        ),
    ]
)
