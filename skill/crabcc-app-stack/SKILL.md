---
name: crabcc-app-stack
description: Native Apple stack guidance for Crabcc.app — TCA + Tuist + Lottie + AsyncQueue. Use when writing or reviewing Swift / SwiftUI code under apps/macos/, when scaffolding new features in the macOS menubar app, or when a contributor reaches for a third-party iOS library (e.g. SwiftUIRoutes, MonarchRouter, Appz) — this skill steers them toward the agreed native replacements (NavigationStack, Link/openURL).
---

# Native Apple stack adoption — TCA + Tuist + Lottie + AsyncQueue

This stack — **TCA + Tuist + Lottie + AsyncQueue** — represents the
"Professional-Grade Swift 6 Standard" for 2026. It balances Apple's
native power with surgical third-party enhancements where the
first-party SDK still lacks specialized tooling.

## 🧩 Describing the Stack: The "Scale & Safety" Choice

This selection is designed for teams that prioritize **long-term
maintainability** and **compiler-guaranteed safety**.

- **TCA (The Composable Architecture):** Acts as the "Brain." It
  replaces messy MVVM with a predictable, state-driven flow. In 2026 it
  is the standard for handling Swift 6's strict concurrency because it
  forces you to manage state transitions through a single, testable
  path.
- **Tuist:** Acts as the "Skeleton." It removes the "merge conflict
  hell" of `.xcodeproj` files by generating them on the fly from Swift
  manifests. It's the industry standard for modularizing large apps
  into smaller, faster-compiling features.
- **Lottie:** Acts as the "Soul." While SwiftUI has improved
  animations, Lottie remains the only way to ship high-fidelity,
  designer-led vector animations without manually writing thousands of
  lines of drawing code.
- **AsyncQueue:** Acts as the "Traffic Controller." It solves a
  specific gap in Swift 6's structured concurrency: **serial
  execution.** While Swift has Actors, it doesn't have a native "FIFO"
  queue for async tasks. AsyncQueue ensures your background tasks
  (like database writes) happen in the exact order they were sent.

## 📚 Official Documentation Links

- **[The Composable Architecture (TCA)](https://pointfreeco.github.io/swift-composable-architecture/)** — Official tutorials and API reference.
- **[Tuist Documentation](https://docs.tuist.io/)** — Guides on project generation, caching, and modularity.
- **[Lottie iOS (Airbnb)](https://airbnb.io/lottie/#/ios)** — Documentation for the industry-standard animation engine.
- **[AsyncQueue (GitHub)](https://github.com/dfed/swift-async-queue)** — Documentation for the FIFO async task manager.
- **[Swift Concurrency Guide](https://docs.swift.org/swift-book/documentation/the-swift-programming-language/concurrency/)** — The foundation upon which this entire stack is built.

## ⚡ 20-Entry Stack Cheatsheet (2026 Edition)

| #  | Tool        | Category    | Command / Concept       | Purpose                                                                  |
|----|-------------|-------------|-------------------------|--------------------------------------------------------------------------|
| 1  | Tuist       | Setup       | `tuist edit`            | Edit your project manifest in a temporary Xcode project.                 |
| 2  | Tuist       | Workflow    | `tuist generate`        | Turn your `Project.swift` into a real `.xcodeproj`.                      |
| 3  | Tuist       | Speed       | `tuist cache`           | Use pre-compiled binaries to skip building unchanged modules.            |
| 4  | Tuist       | Cloud       | `tuist share`           | (2026) Share build artifacts with your team instantly.                   |
| 5  | TCA         | Core        | `Reducer`               | The logic block where all state changes happen.                          |
| 6  | TCA         | Action      | `enum Action`           | Every single thing that can happen in your app is an action.             |
| 7  | TCA         | Effect      | `Effect.run`            | How you perform async work (API calls, DB) inside TCA.                   |
| 8  | TCA         | Testing     | `TestStore`             | Exhaustively prove that every state change happens correctly.            |
| 9  | TCA         | Compose     | `Scope`                 | Embed a small feature's logic into a larger one.                         |
| 10 | TCA         | Nav         | `StackState`            | The 2026 standard for type-safe, multi-screen navigation.                |
| 11 | Lottie      | UI          | `LottieView`            | The native SwiftUI view to play `.lottie` or `.json` files.              |
| 12 | Lottie      | Performance | `dotLottie`             | Use the `.lottie` format for 10x smaller file sizes than JSON.           |
| 13 | Lottie      | Logic       | `LottiePlaybackMode`    | Control looping, speed, and segments via SwiftUI state.                  |
| 14 | AsyncQueue  | Serial      | `AsyncQueue()`          | Create a serial queue that executes one `await` task at a time.          |
| 15 | AsyncQueue  | Order       | `queue.enqueue`         | Send a task to the back of the line to be executed in FIFO order.        |
| 16 | AsyncQueue  | Safety      | `ActorIsolation`        | Use a queue inside an Actor to prevent data races during writes.         |
| 17 | Swift 6     | Safety      | `@MainActor`            | Ensure UI code always runs on the main thread (compiler-enforced).       |
| 18 | Swift 6     | Transfer    | `Sendable`              | A protocol ensuring data can be safely moved between threads.            |
| 19 | Swift 6     | Async       | `TaskGroup`             | Running multiple async tasks in parallel natively.                       |
| 20 | Modular     | Arch        | `uFeatures`             | The Tuist-recommended way to structure features as separate targets.     |

**The Verdict:** This stack is "boring" in the best way possible — it
is predictable, scalable, and keeps you within the guardrails of the
Swift 6 compiler.

## 🚫 Explicitly NOT in this stack

The 2026 Apple ecosystem has consolidated on native replacements for
several once-popular libraries. Push back on any PR that reaches for
these:

| Don't use            | Use instead                                       | Why                                                                                        |
|----------------------|---------------------------------------------------|--------------------------------------------------------------------------------------------|
| SwiftUIRoutes        | `NavigationStack` + `NavigationPath`              | Native, type-safe, integrates with TCA's `StackState`.                                     |
| MonarchRouter        | `NavigationStack` + `NavigationPath`              | Same — native state-based routing.                                                         |
| Appz                 | `Link` / `openURL` (SwiftUI), `NSWorkspace.open` (AppKit) | Apple's URL APIs cover all the Appz use cases since iOS 16+ / macOS 13+.            |

## How to use this skill

When generating, reviewing, or scaffolding Swift code under
`apps/macos/`:

1. **Default to TCA's `@Reducer` macro** for any non-trivial state. Don't
   reach for `@State` + `ObservableObject` for cross-screen state.
2. **All async work goes through `Effect.run`** — never bare `Task { }`
   inside a Reducer body.
3. **For ordered async work** (file writes, socket sends, queued API
   calls), wrap in an `AsyncQueue`.
4. **For navigation**, use `StackState` (TCA) on top of `NavigationStack`
   (SwiftUI). Never third-party.
5. **For animations**, prefer SwiftUI's native `.animation` for simple
   transitions; reach for Lottie when you have an After Effects file
   from a designer or need vector fidelity.
6. **For new modules**, declare them as separate targets in
   `Project.swift` (uFeatures pattern) — Tuist makes the per-feature
   target pattern free.
7. **Tests use `TestStore`** — exhaustively assert every state
   transition. If you write a Reducer without a corresponding TestStore
   test, that's a review-blocker.

## Where this lives in the repo

- Stack scaffold: `apps/macos/`
- Tuist project: `apps/macos/Project.swift`
- SPM manifest: `apps/macos/Tuist/Package.swift`
- Migration plan + RFC: [#192](https://github.com/peterlodri-sec/crabcc/issues/192)
- Companion docs: [`apps/macos/README.md`](../../apps/macos/README.md), AGENTS.md, CLAUDE.md, `.tools`
