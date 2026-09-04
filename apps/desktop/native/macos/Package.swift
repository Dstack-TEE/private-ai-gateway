// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "PrivateAIGatewayMac",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "PrivateAIGatewayMac", targets: ["PrivateAIGatewayMac"]),
        .executable(name: "PrivateAIGatewayLoginItem", targets: ["PrivateAIGatewayLoginItem"]),
    ],
    targets: [
        .executableTarget(
            name: "PrivateAIGatewayMac",
            path: "Sources/PrivateAIGatewayMac",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        .executableTarget(
            name: "PrivateAIGatewayLoginItem",
            path: "Sources/PrivateAIGatewayLoginItem",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        .testTarget(
            name: "PrivateAIGatewayMacTests",
            dependencies: ["PrivateAIGatewayMac"],
            path: "Tests/PrivateAIGatewayMacTests"
        ),
    ]
)
