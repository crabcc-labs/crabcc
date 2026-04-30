// swift-tools-version: 5.10
//
// Tuist's master SPM declaration (Tuist 4.x convention). Lives at
// `apps/macos/Tuist/Package.swift`; resolved into `Tuist/.swiftpm/` by
// `tuist install`. The Project.swift one level up references these
// products via `.package(product: "Foo")`.

@preconcurrency import PackageDescription

#if TUIST
    import struct ProjectDescription.PackageSettings

    let packageSettings = PackageSettings(
        productTypes: [:]
    )
#endif

let package = Package(
    name: "CrabccTuistPackage",
    dependencies: [
        .package(
            url: "https://github.com/pointfreeco/swift-composable-architecture",
            from: "1.16.0"
        ),
        .package(
            url: "https://github.com/airbnb/lottie-ios",
            from: "4.5.0"
        ),
        .package(
            url: "https://github.com/dfed/swift-async-queue",
            from: "0.5.0"
        ),
    ]
)
