# Apple Rich Notifications — Research Dossier

> Verbatim research input that motivates Track C of the
> [native-desktop-and-rich-notifications initiative](./RESEARCH-native-desktop-and-rich-notifications.md).
> Lightly edited only for headings + line wrap; technical content
> preserved as-is. Cite this when implementing C.0–C.5.

## Architecting Apple Native Notification Customization: Rust and JavaScript Integration Strategies

### Introduction to the Apple Notification Ecosystem

The integration of native operating system-level notifications into
cross-platform desktop and mobile applications represents a highly
complex intersection of user experience design, system architecture,
and inter-process communication. On Apple platforms — encompassing
macOS, iOS, iPadOS, and visionOS — the programmatic generation,
styling, handling, and delivery of notifications are exclusively
governed by the `UserNotifications` and `UserNotificationsUI`
frameworks. For developers working outside the traditional Apple
ecosystem, specifically those utilizing systems programming languages
like Rust or web-based frontend technologies like JavaScript via
Electron or Tauri, accessing the deeper capabilities of these
frameworks requires navigating strict architectural boundaries,
foreign function interfaces (FFI), and rigid application bundling
constraints.

Standard cross-platform notification APIs generally default to basic
text strings, a system-generated application icon, and a default
system sound. However, modern user expectations demand "rich
notifications" — dynamic alerts that embed media, present interactive
buttons, allow inline text replies, and display entirely custom
graphical user interfaces. Achieving this level of programmatic
customization from a Rust or JavaScript execution context is not a
trivial undertaking; it demands a nuanced understanding of Apple's
extension architecture, specifically Notification Service Extensions
and Notification Content Extensions, alongside the mechanisms required
to bridge these isolated components with host applications.

### Core Architectural Concepts of Apple Rich Notifications

#### Payload Construction and Delivery Requests

The generation of a local notification on any Apple platform requires
the construction of a `UNMutableNotificationContent` object. This
object acts as the primary data container, exposing mutable properties
for the alert's title, subtitle, body text, application badge count,
and designated sound. Once this content object is fully defined, it
must be packaged into a `UNNotificationRequest` alongside a specific
trigger mechanism. Triggers dictate the delivery condition and can
include a `UNTimeIntervalNotificationTrigger` for time-delayed
delivery, a calendar-based trigger, or a location-based geographic
trigger. This request is then submitted to the shared
`UNUserNotificationCenter` for scheduling and eventual display to the
user.

#### Actionable Notifications: Categories and Custom Actions

To append interactive elements — such as executable buttons or text
input fields — to a notification, the developer must pre-register
`UNNotificationCategory` and `UNNotificationAction` objects. A
critical limitation of the Apple ecosystem is that actions cannot be
injected dynamically on the fly within the payload itself; the host
application must declare the actionable notification types at
application launch.

A `UNNotificationCategory` serves to group a specific set of actions
under a unique string identifier. When an incoming local or remote
notification payload includes this `categoryIdentifier`, the
operating system automatically renders the predefined UI state and
associated buttons.

The spatial constraints and behavioral heuristics applied to these
actions are strictly managed by the operating system. For example,
when the system has unlimited spatial real estate (such as an expanded
notification on an iPad or macOS desktop), it can display up to ten
distinct actions. Conversely, in limited space constraints, the
system restricts the display to a maximum of two actions. Furthermore,
specific hardware interactions, such as the Double Tap gesture on the
Apple Watch Series 9 or Apple Watch Ultra 2, automatically invoke the
first non-destructive action associated with the notification,
requiring developers to carefully prioritize the order in which
actions are registered within the category.

| Native Construct | Purpose and Functionality | Constraints and Characteristics |
|---|---|---|
| `UNNotificationAction` | A standard interactive button presented to the user (e.g., "Accept", "Decline"). | Can be explicitly configured to open the host app in the foreground, execute a task silently in the background, or display in a visually destructive style (typically red text). |
| `UNTextInputNotificationAction` | A specialized action that invokes a system-level keyboard for an inline response. | Returns a specialized `UNTextInputNotificationResponse` containing the user-typed text. Highly utilized for chat and messaging applications to enable rapid replies. |
| `UNNotificationCategory` | The identifier mapping a specific notification payload to a predefined UI configuration and action set. | Must be registered via `setNotificationCategories(_:)` on the `UNUserNotificationCenter` prior to any notification delivery. |

#### The Extension Paradigm: Service vs. Content

While basic actions and replies provide interactivity, the ultimate
flexibility in notification delivery and styling relies entirely on
two specific App Extensions. App Extensions are distinct, separate,
and sandboxed binaries compiled and bundled within the main
application package, executing in entirely separate processes from
the host application.

The first is the **Notification Service Extension**
(`UNNotificationServiceExtension`). This extension provides a
background execution environment that grants the application a brief
window to modify the payload of a remote notification before it is
displayed to the user. It is primarily utilized to execute short-lived
network requests to download rich media (images, audio, or video) and
append them to the payload as `UNNotificationAttachment` objects. The
invocation of this extension is strictly conditional; it requires the
incoming remote push payload to explicitly include the
`"mutable-content": 1` flag.

The second is the **Notification Content Extension**
(`UNNotificationContentExtension`). This is a user interface
controller that completely supplements or replaces the default Apple
notification banner with a fully custom graphical view. This
extension is the sole mechanism available on macOS and iOS to truly
"style" a notification. It enables developers to alter typography,
customize internal layouts, inject brand-specific graphical
components, and display dynamic application-specific data extracted
from the notification's payload.

### Bypassing Abstractions: Interfacing with UserNotifications via Rust

#### The Architectural Limitations of Generic Notification Crates

Historically, Rust applications aiming for cross-platform compatibility
have relied heavily on the `notify-rust` crate for desktop
notifications. While this library is highly effective on Linux (where
it elegantly implements the XDG Desktop Notifications Specification
via D-Bus or zbus) and maintains functional parity on Windows, its
implementation on macOS presents severe architectural limitations.

The semantics of the XDG specification and Apple's `NSUserNotification`
or `UserNotifications` frameworks are fundamentally incongruent. The
`notify-rust` crate explicitly notes that only a very small subset of
functions is supported on macOS, forcing developers to implement
platform-specific toggles. Prior implementations within the Rust
ecosystem relied on the deprecated `mac-notification-sys` library to
bridge this gap, which introduced profound stability and performance
bottlenecks. Developers utilizing these legacy bindings documented
severe blocking issues, noting that invoking macOS listeners caused
application CPU utilization to spike uncontrollably to 100% the moment
a notification was received, persisting until the user manually
dismissed the alert. Furthermore, these generic abstractions
fundamentally lack robust support for capturing notification
interactions, such as button clicks or inline text responses,
rendering actionable notifications effectively impossible through
standard cross-platform crates.

#### Direct Objective-C Bindings via the objc2 Framework

To programmatically generate native Apple notifications in Rust with
absolute customizability, developers must bypass cross-platform
abstractions entirely and interface directly with the Objective-C
runtime. This is achieved using the `objc2` ecosystem, specifically
the `objc2-user-notifications`, `objc2-user-notifications-ui`, and
`objc2-foundation` crates.

The `objc2` framework provides zero-cost, memory-safe bindings to
Apple frameworks by utilizing Rust's strict type system to manage
Objective-C object lifecycles. It employs `Retained` pointers to
safely interface with Automatic Reference Counting (ARC) and enforces
thread safety constraints using `MainThreadMarker`, guaranteeing that
UI-blocking calls are correctly routed.

#### Defining and Dispatching Notifications in Pure Rust

Using `objc2`, a Rust backend can directly instantiate a
`UNMutableNotificationContent` object, assign properties, and dispatch
it to the `UNUserNotificationCenter` without relying on intermediary
cross-platform translation layers.

Creating a fully localized, actionable notification purely in Rust
involves a precise programmatic sequence:

1. **System Authorization**: The Rust application must first request
   authorization by invoking `requestAuthorizationWithOptions:` on the
   current notification center, explicitly requesting alert, sound,
   and badge capabilities.
2. **Category and Action Registration**: The application creates
   `UNNotificationAction` instances (e.g., using
   `actionWithIdentifier:title:options:`), groups them into a
   `UNNotificationCategory`, and registers them to the center via
   `setNotificationCategories:`.
3. **Content Construction**: The `UNMutableNotificationContent` object
   is instantiated. Strings for titles and bodies are generated using
   `NSString` bindings. If custom styling is desired, the
   `categoryIdentifier` property must be populated with a registered
   ID that matches a bundled Notification Content Extension.
4. **Request Delivery**: The constructed content is packaged into a
   `UNNotificationRequest` alongside an appropriate trigger, and
   executed via the `addNotificationRequest:withCompletionHandler:`
   message.

#### Managing Delegate Callbacks and Exception Handling

To actively process user interactions, the Rust application must
implement the `UNUserNotificationCenterDelegate` protocol. The `objc2`
crate provides a powerful macro, `declare_class!`, which allows
developers to construct an Objective-C class entirely within Rust
that conforms to this specific Apple protocol.

When the user selects a custom action, the operating system executes
the
`userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:`
method on the registered delegate. The Rust implementation parses the
`actionIdentifier` property of the returned `UNNotificationResponse`.
If the user utilized a `UNTextInputNotificationAction`, the text is
carefully extracted from the `UNTextInputNotificationResponse`
subclass. Crucially, the completion handler must be called rapidly by
the Rust thread; failing to do so causes the operating system to
penalize the application's background execution privileges or
forcefully terminate the background execution context.

Interfacing deeply with Objective-C frameworks necessitates rigorous
error and exception handling within Rust. A common pitfall when
configuring native audio, video, or notification settings is passing
raw Rust strings where the system expects specifically defined
`CFStringRef` or `NSString` constants. Passing invalid keys into
Apple's dictionary structures can result in immediate, fatal runtime
segment faults that Rust's standard error handling cannot catch.
Developers must utilize the explicit constants provided by `objc2`
crates, safely casting them to `NSString` references before passing
them into the notification construction payload.

### Server-Side Orchestration: Node.js and APNs Integration

While local programmatic notifications allow immediate, on-device UI
rendering, many modern applications rely on remote push notifications
delivered via APNs (Apple Push Notification service) or FCM (Firebase
Cloud Messaging). Generating, styling, and customizing these payloads
remotely introduces distinct architectural requirements that are
typically handled by backend services utilizing runtime environments
like Node.js.

#### Cryptographic Configuration and the Token Lifecycle

Before a Node.js server can dispatch a rich notification to an Apple
device, cryptographic trust must be established. This traditionally
involves acquiring an Apple Push Notification service authentication
token signing key or a certificate. Often, developers export a `.p12`
(PKCS #12) file from the macOS Keychain. For security and
compatibility with standard Node.js cryptographic libraries or the
OpenSSL CLI, this `.p12` file must be converted into distinct
`public.crt.pem` (certificate) and `private.key.pem` (private key)
files.

Concurrently, the native Rust or JavaScript client application must
request a device token from APNs. This token is a highly specific,
32-byte binary value (typically represented as a 64-character
hexadecimal string) that uniquely identifies the specific application
installation on a specific hardware device. This token is transmitted
back to the Node.js server to serve as the routing destination.

#### Constructing the APNs Payload over HTTP/2

Modern APNs interactions mandate the use of the HTTP/2 protocol. The
transition to HTTP/2 allows for multiplexing, meaning a Node.js server
can maintain a persistent connection and pipeline thousands of
notification requests asynchronously without opening new TCP
connections. This is a critical architectural requirement; opening
new connections for every individual request will trigger Apple's
internal Denial of Service protections, resulting in the permanent
blocking of the sending IP address.

When the Node.js backend dispatches a push notification, the JSON
payload must be meticulously structured to invoke the custom styles
and extensions compiled into the native application.

To trigger a Notification Content Extension (for custom visual
styling), the payload must include the `category` key, mapped to the
exact string registered by the `UNNotificationCategory` in the native
application code. Furthermore, to invoke the Notification Service
Extension (for downloading rich media), the `"mutable-content": 1`
key-value pair is absolutely mandatory.

A fully featured JSON payload generated by a Node.js backend would
mirror the following structure:

```json
{
  "aps": {
    "alert": {
      "title": "System Diagnostic Complete",
      "body": "Your scheduled task has finalized."
    },
    "category": "CUSTOM_DATA_VIEW",
    "mutable-content": 1,
    "sound": "custom_alert.caf"
  },
  "attachment_url": "https://server.domain/media/diagnostic_chart.png",
  "task_id": "84a8b-911"
}
```

In this architecture, the Node.js server offloads the heavy media
assets to a standard CDN, passing only the URL in the payload. The
native Service Extension intercepts the notification, downloads the
image, and the Content Extension subsequently renders the
`CUSTOM_DATA_VIEW` utilizing the downloaded asset. Furthermore, the
sound parameter can specify a custom audio signal, but the targeted
`.caf` audio file must be physically present within the host
application's compiled bundle resources to trigger successfully.

### Bridging the Gap: JavaScript Desktop Integration via Electron

When wrapping native code in web technologies for desktop distribution,
frameworks like Electron act as orchestration layers. They must
translate JavaScript API calls into native Objective-C execution
securely and efficiently.

Electron explicitly bypasses the standard HTML5 web Notifications API
internally, instead mapping developer calls directly to macOS native
capabilities via its Main Process Notification class. This
architecture exposes a robust API tailored specifically for macOS,
allowing developers to set standard configurations while accessing
Apple-specific rich features.

JavaScript developers utilizing Electron can dynamically configure
several advanced parameters natively:

- **Subtitles**: Utilizing the `subtitle` option, a secondary string of
  text is rendered prominently between the primary title and the body
  content, adhering to Apple's modern notification design language.
- **Inline Replies**: Setting the `hasReply: true` boolean instructs
  the macOS notification center to create a text input box directly
  within the notification banner. A `replyPlaceholder` string can be
  defined to guide the user. When the user submits the inline text,
  the Electron notification instance emits a `'reply'` event
  containing the user's string data, allowing the main process to
  handle the response without bringing the application to the
  foreground.
- **Actions and Interactivity**: An array of objects can be passed to
  the `actions` property to append interactive buttons. Selecting
  these actions emits the `'action'` event, providing an `actionIndex`
  corresponding to the triggered button.
- **Sound Customization**: Custom sounds can be triggered by passing a
  specific string name to the `sound` property. However, the macOS
  environment mandates that these custom sound files must be
  physically copied into the compiled application bundle (e.g.,
  `YourApp.app/Contents/Resources`) or reside in specific system
  directories such as `/Library/Sounds` or `~/Library/Sounds` to
  function properly, interfacing under the hood with the `NSSound`
  framework.

Electron accomplishes this robust integration internally by
maintaining an active, highly optimized
`UNUserNotificationCenterDelegate` written in Objective-C++,
translating the asynchronous native callbacks directly into Node.js
event emitter streams that JavaScript developers can easily consume.

### The Tauri Ecosystem Evolution: From v1 Limitations to v2 Architecture

[…]

(See the kickoff message for the rest of this section: Tauri v1 vs.
v2, the `@tauri-apps/plugin-notification` plugin, the Swift singleton
deadlock workaround, the granular permissions table.)

### Advanced Visual Styling: UNNotificationContentExtension Architectures

While Electron and Tauri v2 efficiently expose actions, inline
replies, and basic attachments, they cannot natively alter the visual
aesthetics — such as specific typography, brand colors, complex
layouts, or custom graphical elements — of an Apple notification. The
standard notification alert banner is strictly controlled by the
operating system to enforce visual consistency and prevent malicious
UI spoofing.

If application requirements dictate true "customization styling" on
macOS or iOS, the programmatic solution invariably requires stepping
entirely outside of the JavaScript, Node.js, or pure Rust context to
engineer a native Notification Content Extension.

#### Constructing the Extension Paradigm

A `UNNotificationContentExtension` is an isolated, sandboxed object
conforming to the `NSObjectProtocol` that presents a completely
custom graphical interface. Available on macOS 11.0+, Mac Catalyst
10.0+, and iOS 10.0+, it acts as a manager for an `NSViewController`
(macOS) or `UIViewController` (iOS) that completely replaces or
heavily supplements the default system notification banner.

Because this extension executes in a highly constrained,
memory-limited sandbox — generally capped at approximately 120 MB of
RAM on mobile platforms — and must render its user interface almost
instantaneously to prevent user frustration, Apple strictly forbids
complex, long-running operations like synchronous network fetching.

#### Bypassing Framework Limitations for Custom User Interfaces

Crucially, the Content Extension cannot be authored in JavaScript or
standard web technologies. It is a strictly native view controller.
Therefore, developers utilizing Rust/Tauri or Electron must pivot to
authoring this specific component in Swift, Objective-C, or heavily
specialized, complex Rust code leveraging `objc2-app-kit` or
`objc2-uikit`.

To successfully integrate the custom UI into a cross-platform
application, a rigorous configuration process is mandatory:

1. **Category Mapping**: The extension's internal `Info.plist` file
   must declare the `UNNotificationExtensionCategory` key. The string
   value of this key must flawlessly match the `categoryIdentifier`
   string the Rust or JavaScript application uses when initially
   firing the notification payload.
2. **Suppressing System Interfaces**: By default, Apple automatically
   renders the application icon, the default title, and the body text
   alongside the developer's custom interface. To achieve absolute
   styling control, the developer must inject the
   `UNNotificationExtensionDefaultContentHidden` key into the
   `Info.plist` and set it to `true`. This action completely
   suppresses all system-generated user interface elements, leaving
   only the developer's custom view and the system-managed action
   buttons.
3. **UI Construction and Data Binding**: The user interface is
   typically constructed visually via Xcode Storyboards
   (`MainInterface.storyboard`) or programmatically via SwiftUI.
   During the `didReceiveNotification:` lifecycle event, the
   extension extracts application-specific payload data passed via
   the `userInfo` dictionary to dynamically populate the custom UI
   elements, such as dynamic progress bars, customized typography, or
   localized brand graphics.

#### Overcoming Inter-Process Communication (IPC) Constraints

A critical architectural challenge arises with this paradigm: How
does the primary Rust or JavaScript host application interact, share
state, or pass complex styling configuration to the isolated Content
Extension?

By definition, app extensions run in entirely separate, secured
processes. They cannot directly access the host application's memory
space, nor can they invoke JavaScript functions or utilize standard
cross-process URL schemes reliably (as the iOS sandbox explicitly
blocks extensions from automatically launching their host
applications). If the Content Extension needs to read user
preferences, session decryption keys, or real-time state from the
host app to style the notification properly, direct variable access
fundamentally fails.

The mandatory, Apple-sanctioned solution is the implementation of
**App Groups**. App Groups allow distinct, sandboxed processes
produced by the same development team (sharing the same Apple
Developer Team ID) to share a containerized filesystem and a shared
`NSUserDefaults` domain.

- **State Writing (Host App Layer)**: The primary application
  (utilizing Rust FFI bindings or native modules in JavaScript)
  writes a state object to the shared preferences using a specialized
  initialization:
  `[[NSUserDefaults alloc] initWithSuiteName:@"group.com.company.appName"]`.
- **State Reading (Extension Layer)**: When the notification is
  delivered and the extension awakes, it accesses the identical suite
  name to retrieve the styling configuration, cached images, or
  payload data immediately before rendering the user interface.

Furthermore, when a user interacts with a custom element inside the
stylized interface, the extension itself must not execute complex
business logic. Instead, the extension triggers the
`performNotificationDefaultAction()` API, effectively dismissing the
notification UI and gracefully handing execution back to the host
application's `UNUserNotificationCenterDelegate` (operating in the
Rust/JS background layer) for processing.

### Compilation, Bundling, and Apple Silicon Security Architecture

The architectural requirement to deploy a separate App Extension
radically complicates the compilation and bundling pipeline for Rust,
Tauri, and Node.js-based desktop applications. A standard `cargo
build` or `npm run build` generates a singular executable binary.
However, macOS and iOS strictly dictate that the Content Extension
(compiled as an `.appex` file) must be bundled structurally within
the `Contents/PlugIns/` directory of the primary `.app` package.

#### Embedding Extensions within Tauri and Rust Workflows

Applications built with raw Rust or the Tauri framework traditionally
utilize `cargo-bundle` or `cargo-packager` to generate macOS `.app`
bundles. However, these utilities generally lack the sophisticated,
multi-target sub-bundling logic natively provided by the Xcode build
system.

To successfully integrate an iOS or macOS extension into a Tauri
project, the build process necessitates the implementation of custom
shell scripting or external tools like `xcodegen` to execute
concurrently with the Cargo pipeline. Because standard commands like
`tauri ios init` frequently overwrite or reset the generated Apple
project structures (located in `src-tauri/gen/apple/`), relying on
manual Xcode configuration is highly volatile and prone to continuous
regression.

The industry-standard approach involves writing an automated
pre-build script (e.g., `setup-apple-extension.sh`) that hooks
intimately into the continuous integration or build pipeline to
perform several critical steps:

1. Automatically generate the extension `.pbxproj` target within the
   overarching workspace.
2. Programmatically inject the mandatory App Groups capabilities into
   the generated `.entitlements` files to facilitate IPC.
3. Ensure the primary Rust binary is compiled, while the Swift or
   Objective-C App Extension is concurrently built and structurally
   nested into the correct final output bundle location.
4. Automate the injection of proper XPC connections to satisfy the
   `ExtensionFoundation` requirements demanded by macOS 11+ and
   modern iOS versions for inter-process execution.

#### Ad-Hoc Signing and Apple Silicon (ARM64) Enforcement

A paramount consideration for developers architecting customized
notifications on macOS is the rigid security model enforced by Apple
Silicon (ARM64) hardware. ARM64 macOS strictly refuses to execute any
binary, or load any `.appex` extension into memory, that lacks a
mathematically valid cryptographic code signature.

When a developer bundles a Rust or JavaScript application alongside a
newly created Content Extension without relying entirely on Xcode's
internal pipeline, standard linkers (like GNU or LLVM `lld`) might
only generate an ad-hoc signature for the primary executable. The
deeply nested `.appex` inside the `PlugIns` directory frequently
remains unsigned or invalidly signed, resulting in a silent failure
where the OS refuses to load the custom notification UI.

To resolve this during development, developers must implement a
recursive, deep-signing shell command post-compilation:

```bash
codesign --force --deep -s - /path/to/program.app
```

For production distribution, particularly for applications distributed
outside of the heavily guarded Mac App Store, the entire `.app` bundle
— inclusive of the main Rust/JavaScript binary and all nested
`PlugIns/*.appex` dependencies — must be cryptographically signed
with a valid Developer ID Application certificate and subsequently
submitted to the Apple Notary Service. Failure to properly apply and
sign the entitlements, specifically the App Group entitlement linking
the main process and the extension, will result in immediate
execution rejection by the operating system's Gatekeeper utility.

### Synthesis and Architectural Trajectories

The evolution of Apple's notification framework clearly signals a
definitive trajectory toward deeply isolated, highly declarative
extension architectures. While cross-platform integration frameworks
like Electron and Tauri provide highly effective, abstracted bridges
for standard operating system notification features — such as basic
actionable buttons, media attachments, and inline text replies —
they inevitably hit a hard architectural ceiling when developers
demand true visual interface customization.

The absolute requirement to author a `UNNotificationContentExtension`
to manipulate notification appearance forces a polyglot development
model upon engineering teams. Cross-platform applications cannot
remain 100% Rust or 100% JavaScript; they are forced to incorporate
Swift or Objective-C components to appease the rigorous
`ExtensionFoundation` requirements of the Apple operating systems.
Even low-level, high-performance bindings like `objc2` face immense
technical friction when attempting to map standard UI rendering logic
across strict process boundaries into a sandboxed `.appex` bundle
securely.

Looking forward, as Apple continues to aggressively unify its core
notification architecture across iOS, iPadOS, macOS, and visionOS,
developers leveraging Rust and JavaScript must adopt increasingly
rigorous build-time orchestration. Success in delivering high-
fidelity, highly customized native notifications requires treating
the cross-platform framework not as the exclusive execution
environment, but rather as a central orchestrator. This orchestrator
must securely manage, cryptographically sign, and communicate with
highly specialized, natively compiled extension binaries via
constrained IPC mechanisms like App Groups. Only through mastering
this complex, multi-process architecture can developers truly unlock
the deep native styling capabilities of the Apple ecosystem from
within external runtimes.
