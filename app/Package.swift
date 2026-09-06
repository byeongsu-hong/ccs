// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "CcsMenu",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "ccs-menu", targets: ["CcsMenu"])],
    targets: [
        .executableTarget(name: "CcsMenu", path: "Sources/CcsMenu"),
        .testTarget(name: "CcsMenuTests", dependencies: ["CcsMenu"], path: "Tests/CcsMenuTests"),
    ]
)
