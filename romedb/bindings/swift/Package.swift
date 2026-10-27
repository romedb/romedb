// swift-tools-version: 5.9
import PackageDescription
let package = Package(
    name: "RomeDB",
    products: [.library(name: "RomeDB", targets: ["RomeDB"])],
    targets: [
        .systemLibrary(name: "CRomeDB"),
        .target(name: "RomeDB", dependencies: ["CRomeDB"]),
        .testTarget(name: "RomeDBTests", dependencies: ["RomeDB"])
    ]
)
