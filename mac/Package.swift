// swift-tools-version:5.9
import PackageDescription

// The Quark menubar app and its widgets. Both are clients of quarkd only: every
// fact they show and every action they take goes through quarkd's HTTP API
// (see api/openapi.json).
let package = Package(
    name: "QuarkMenuBar",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "QuarkKit", targets: ["QuarkKit"]),
        .executable(name: "QuarkMenuBar", targets: ["QuarkMenuBar"]),
        .executable(name: "QuarkWidgets", targets: ["QuarkWidgets"]),
    ],
    targets: [
        // quarkd API models and client. No AppKit, so it builds and tests anywhere Swift runs.
        .target(name: "QuarkKit"),
        // How Quark's state words look, shared by the menubar and the widgets.
        .target(name: "QuarkUI", dependencies: ["QuarkKit"]),
        .executableTarget(name: "QuarkMenuBar", dependencies: ["QuarkKit", "QuarkUI"]),
        // The WidgetKit extension; scripts/package.sh wraps it in an .appex inside QuarkBar.app.
        .executableTarget(name: "QuarkWidgets", dependencies: ["QuarkKit", "QuarkUI"]),
        .testTarget(
            name: "QuarkKitTests",
            dependencies: ["QuarkKit"],
            resources: [.copy("Fixtures")]
        ),
    ]
)
