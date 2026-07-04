// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "DaemonKit",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .library(name: "DaemonKit", targets: ["DaemonKit"])
    ],
    targets: [
        .target(name: "DaemonKit"),
        .testTarget(name: "DaemonKitTests", dependencies: ["DaemonKit"])
    ]
)
