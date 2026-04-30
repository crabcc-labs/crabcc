// Project.swift — Tuist 4.x declaration for Crabcc.app (issue #192 phase 0).
//
// Generated Xcode project lands in `apps/macos/Crabcc.xcodeproj` and is
// gitignored. `apps/macos/Derived/` holds Tuist's intermediate state.
//
// To use:
//   cd apps/macos
//   tuist install         # resolve SPM dependencies into Tuist/.swiftpm
//   tuist generate        # produce Crabcc.xcodeproj
//   xcodebuild -project Crabcc.xcodeproj -scheme Crabcc -configuration Release build
//
// `mise install tuist` (or `brew install tuist`) installs the Tuist CLI.
// The legacy `swiftc -O -parse-as-library *.swift` path in
// scripts/build-dmg.sh stays intact through this phase — Phase 1 of the
// #192 migration is when build-dmg.sh switches over.

import ProjectDescription

let project = Project(
    name: "Crabcc",
    organizationName: "crabcc",
    options: .options(
        automaticSchemesOptions: .enabled(targetSchemesGrouping: .singleScheme),
        defaultKnownRegions: ["Base", "en"],
        developmentRegion: "en"
    ),
    packages: [
        .remote(
            url: "https://github.com/pointfreeco/swift-composable-architecture",
            requirement: .upToNextMajor(from: "1.16.0")
        ),
        .remote(
            url: "https://github.com/airbnb/lottie-ios",
            requirement: .upToNextMajor(from: "4.5.0")
        ),
        .remote(
            url: "https://github.com/dfed/swift-async-queue",
            requirement: .upToNextMajor(from: "0.5.0")
        ),
    ],
    settings: .settings(
        base: [
            "SWIFT_VERSION": "5.10",
            "MACOSX_DEPLOYMENT_TARGET": "13.0",
        ]
    ),
    targets: [
        .target(
            name: "Crabcc",
            destinations: [.mac],
            product: .app,
            bundleId: "com.crabcc.app",
            deploymentTargets: .macOS("13.0"),
            infoPlist: .extendingDefault(with: [
                "LSUIElement": true,
                "CFBundleShortVersionString": "0.1.0",
                "NSHumanReadableCopyright": "Crabcc — single-host menubar.",
            ]),
            sources: ["Sources/**"],
            resources: ["Resources/**"],
            dependencies: [
                .package(product: "ComposableArchitecture"),
                .package(product: "Lottie"),
                .package(product: "AsyncQueue"),
            ]
        ),
        .target(
            name: "CrabccTests",
            destinations: [.mac],
            product: .unitTests,
            bundleId: "com.crabcc.app.tests",
            deploymentTargets: .macOS("13.0"),
            sources: ["Tests/**"],
            dependencies: [
                .target(name: "Crabcc"),
                .package(product: "ComposableArchitecture"),
            ]
        ),
    ]
)
