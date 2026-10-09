// swift-tools-version:5.9
import Foundation
import PackageDescription

// Default links the nucleus built in this repository. An explicit override is useful for
// local validation against a compatible build; distribution always builds its own nucleus.
let nucleus = ProcessInfo.processInfo.environment["QUALL_MONITOR_LIBQUALL"]
    ?? "../../target/release/libquall.a"
let screen: [SwiftSetting] = [.define("QUALL_TELA_ESTENDIDA_FUTURA")]
let package = Package(
    name: "QuallMonitor",
    defaultLocalization: "pt",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "quall-monitor-app", targets: ["QuallMonitorApp"]),
        .executable(name: "quall-monitor-display", targets: ["quall-monitor-display"])
    ],
    targets: [
        .target(name: "QuallIdiomaKit", resources: [.process("Recursos")]),
        .target(name: "CMonitorVirtual", publicHeadersPath: "include",
                linkerSettings: [.linkedFramework("CoreGraphics"), .linkedFramework("Foundation")]),
        .target(name: "QuallCaptureKit", dependencies: ["QuallIdiomaKit", "CMonitorVirtual"],
                swiftSettings: screen),
        .systemLibrary(name: "CQuall"),
        .target(name: "QuallReceptorKit", dependencies: ["QuallIdiomaKit"]),
        .target(name: "QuallNetKit", dependencies: ["CQuall", "QuallReceptorKit"]),
        .target(name: "QuallMonitorKit", dependencies: ["QuallCaptureKit", "QuallIdiomaKit"]),
        .executableTarget(name: "quall-monitor-display", dependencies: ["QuallCaptureKit"],
                          path: "Sources/quall-monitor-virtual", swiftSettings: screen),
        .executableTarget(name: "QuallMonitorApp",
                          dependencies: ["QuallCaptureKit", "QuallNetKit", "QuallReceptorKit", "QuallIdiomaKit", "QuallMonitorKit", "CQuall"],
                          swiftSettings: screen,
                          linkerSettings: [.unsafeFlags([nucleus, "-lc++"])]),
        .testTarget(name: "QuallMonitorTests", dependencies: ["QuallCaptureKit", "QuallReceptorKit", "QuallMonitorKit", "QuallIdiomaKit"],
                    swiftSettings: screen)
    ]
)
